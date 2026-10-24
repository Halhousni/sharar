//! Reader for CED Spike2 `.smrx` (SON64) files.
//!
//! The 64-bit format has no published specification. The layout below was recovered from
//! public `.smrx` recordings and checked against CED's own `sonpy` library reading the same
//! files (see `tools/smrx_oracle.py`). All integers are little-endian.
//!
//! ```text
//! header stream  first 64 KiB of the file, then each extra header block minus its
//!                16-byte block header. Offsets in the file header and the channel
//!                records count in this stream, not in the file.
//! file header    "S64" magic, creator, seconds per tick (f64), channel table offset,
//!                string table offset, channel slot count, bytes per channel record (272)
//! channel table  one 272-byte record per slot; kind 0 marks a deleted slot
//! string table   titles, units and comments, referred to by 1-based index
//! blocks         16-byte header, then payload. Each channel owns a tree of 4 KiB index
//!                blocks (entries of first time + child offset) over 64 KiB data blocks.
//! ```
//!
//! Times are 64-bit ticks. Waveform data blocks hold runs of `(first tick, sample count,
//! samples)`; event blocks hold 8-byte times; marker items carry a 16-byte head (time, four
//! marker bytes, four spare bytes) followed by any waveform or text payload.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::{Block, Channel, Kind, SonFile};

const BLOCK_LEN: u64 = 65_536;
const BLOCK_HEADER_LEN: u64 = 16;
const INDEX_ENTRIES: usize = 255;
const MAX_LEVELS: usize = 8;
const MAX_HEADER_BLOCKS: usize = 4_096;
const MAX_STRINGS: usize = 1 << 20;
/// Smallest channel record that still holds every field this reader uses.
const MIN_RECORD_LEN: usize = 0x60;

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Bounds-checked little-endian reads from a byte slice.
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn get(&self, at: usize, n: usize) -> io::Result<&'a [u8]> {
        at.checked_add(n)
            .and_then(|end| self.0.get(at..end))
            .ok_or_else(|| bad(format!("SON64 header read past its end at {at}")))
    }
    fn u8(&self, at: usize) -> io::Result<u8> {
        Ok(self.get(at, 1)?[0])
    }
    fn u16(&self, at: usize) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.get(at, 2)?.try_into().unwrap()))
    }
    fn u32(&self, at: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.get(at, 4)?.try_into().unwrap()))
    }
    fn u64(&self, at: usize) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.get(at, 8)?.try_into().unwrap()))
    }
    fn i64(&self, at: usize) -> io::Result<i64> {
        Ok(self.u64(at)? as i64)
    }
    fn f64(&self, at: usize) -> io::Result<f64> {
        Ok(f64::from_bits(self.u64(at)?))
    }
}

fn read_at(file: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut buf = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// First 64 KiB of the file followed by the payload of every extra header block.
fn header_stream(file: &mut File, file_len: u64) -> io::Result<Vec<u8>> {
    let mut stream = read_at(file, 0, BLOCK_LEN as usize)?;
    if stream.len() < 1024 {
        return Err(bad("SON64 file is shorter than its header"));
    }
    let extra = Bytes(&stream).u32(996)? as usize;
    if extra > MAX_HEADER_BLOCKS || 1024 + 8 * extra > stream.len() {
        return Err(bad(format!("SON64 header lists {extra} extra header blocks")));
    }
    let offsets: Vec<u64> = (0..extra)
        .map(|i| Bytes(&stream).u64(1024 + 8 * i))
        .collect::<io::Result<_>>()?;
    for off in offsets {
        if off == 0 || off >= file_len {
            return Err(bad(format!("SON64 extra header block at {off} is outside the file")));
        }
        let block = read_at(file, off, BLOCK_LEN as usize)?;
        if block.len() < BLOCK_HEADER_LEN as usize {
            return Err(bad("SON64 extra header block is truncated"));
        }
        stream.extend_from_slice(&block[BLOCK_HEADER_LEN as usize..]);
    }
    Ok(stream)
}

/// Strings are 1-based. Index 0 means "none" and comes back as an empty string.
fn string_table(stream: &[u8], at: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let bytes = Bytes(stream);
    let (Ok(_len), Ok(count)) = (bytes.u32(at), bytes.u32(at + 4)) else {
        return out;
    };
    let count = (count as usize).min(MAX_STRINGS);
    let mut pos = at + 8;
    for _ in 0..count {
        // A reference count precedes each string.
        let start = pos + 4;
        let Some(tail) = stream.get(start..) else { break };
        let Some(nul) = tail.iter().position(|b| *b == 0) else { break };
        out.push(tail[..nul].iter().map(|b| *b as char).collect::<String>().trim_end().to_string());
        pos = at + 8 + ((start + nul + 1 - (at + 8)) + 3) / 4 * 4;
    }
    out
}

struct Walk<'a> {
    file: &'a mut File,
    file_len: u64,
    kind: Kind,
    channel: u16,
    generation: u16,
    /// Bytes per item for event and marker blocks, per sample for waveforms.
    item_bytes: u64,
    interval_ticks: i64,
    blocks: Vec<Block>,
}

impl Walk<'_> {
    fn node(&mut self, off: u64, depth: usize) -> io::Result<()> {
        if off == 0 || off + BLOCK_HEADER_LEN > self.file_len || depth > MAX_LEVELS {
            return Ok(());
        }
        let head = read_at(self.file, off, BLOCK_HEADER_LEN as usize)?;
        let h = Bytes(&head);
        let level = (h.u64(0)? >> 8) & 0xf;
        let channel = h.u16(8)?;
        let generation = h.u16(10)?;
        let count = h.u32(12)? as usize;

        if level == 0 {
            // Left-overs of a deleted channel share the file with the live one.
            if channel == self.channel && generation == self.generation {
                self.data(off, count)?;
            }
            return Ok(());
        }

        let n = count.min(INDEX_ENTRIES);
        let raw = read_at(self.file, off + BLOCK_HEADER_LEN, n * 16)?;
        let entries = Bytes(&raw);
        for k in 0..raw.len() / 16 {
            let child = entries.u64(k * 16 + 8)?;
            self.node(child, depth + 1)?;
        }
        Ok(())
    }

    fn data(&mut self, off: u64, count: usize) -> io::Result<()> {
        let payload = off + BLOCK_HEADER_LEN;
        if self.kind.is_waveform() {
            // `count` runs of (first tick, sample count, samples).
            let mut pos = payload;
            for _ in 0..count {
                let head = read_at(self.file, pos, 16)?;
                if head.len() < 16 {
                    break;
                }
                let h = Bytes(&head);
                let start = h.i64(0)?;
                let n = h.u64(8)?;
                let data_pos = pos + 16;
                let end_of_run = data_pos.saturating_add(n.saturating_mul(self.item_bytes));
                if end_of_run > self.file_len || n > i32::MAX as u64 {
                    return Err(bad(format!("SON64 waveform run at {pos} runs past the file")));
                }
                let last = start + (n.saturating_sub(1) as i64) * self.interval_ticks;
                self.blocks.push(Block {
                    data_pos,
                    items: n as i32,
                    start_tick: start,
                    end_tick: last,
                });
                pos = end_of_run;
            }
        } else if count > 0 {
            let stride = self.item_bytes;
            if payload + count as u64 * stride > self.file_len {
                return Err(bad(format!("SON64 event block at {off} runs past the file")));
            }
            let first = read_at(self.file, payload, 8)?;
            let last = read_at(self.file, payload + (count as u64 - 1) * stride, 8)?;
            self.blocks.push(Block {
                data_pos: payload,
                items: count as i32,
                start_tick: Bytes(&first).i64(0)?,
                end_tick: Bytes(&last).i64(0)?,
            });
        }
        Ok(())
    }
}

pub(crate) fn open(path: PathBuf, mut file: File) -> io::Result<SonFile> {
    let file_len = file.metadata()?.len();
    let stream = header_stream(&mut file, file_len)?;
    let h = Bytes(&stream);

    if &stream[..3] != b"S64" {
        return Err(bad("not a SON64 file"));
    }
    let creator: String = stream[16..24]
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let mut tick = h.f64(32)?;
    if !(tick.is_finite() && tick > 0.0) {
        tick = 1e-6;
    }
    let chan_table = h.u32(44)? as usize;
    let str_table = h.u32(52)? as usize;
    let slots = h.u32(56)? as usize;
    let record = h.u32(60)? as usize;
    if record < MIN_RECORD_LEN {
        return Err(bad(format!("SON64 channel records are {record} bytes")));
    }
    if slots > 1 << 16 {
        return Err(bad(format!("SON64 file claims {slots} channel slots")));
    }

    let strings = string_table(&stream, str_table);
    let text = |i: u32| strings.get(i as usize).cloned().unwrap_or_default();

    let mut comments = Vec::new();
    for k in 0..5 {
        let c = text(h.u32(64 + 4 * k)?);
        if !c.is_empty() {
            comments.push(c);
        }
    }

    let mut channels = Vec::new();
    for idx in 0..slots {
        let b = chan_table + idx * record;
        if b + record > stream.len() {
            break;
        }
        let kind = Kind::from_code(h.u8(b + 0x2e)?);
        if kind == Kind::Empty {
            continue;
        }

        let root = h.u64(b)?;
        let item_len = h.u32(b + 0x20)? as u64;
        let generation = h.u16(b + 0x2c)?;
        let title = text(h.u32(b + 0x34)?);
        let units = text(h.u32(b + 0x38)?);
        let comment = text(h.u32(b + 0x3c)?);
        let interval_ticks = h.u64(b + 0x40)? as i64;
        let ideal_rate = h.f64(b + 0x48)? as f32;
        let (mut scale, mut offset) = (1.0f32, 0.0f32);
        if matches!(kind, Kind::Adc | Kind::AdcMark) {
            scale = h.f64(b + 0x50)? as f32;
            offset = h.f64(b + 0x58)? as f32;
        }

        let timed = kind.is_waveform() || kind.is_wavemark();
        let sample_interval = if timed { interval_ticks as f64 * tick } else { 0.0 };
        let n_extra = match kind {
            Kind::AdcMark | Kind::RealMark | Kind::TextMark => item_len.saturating_sub(16),
            _ => 0,
        };

        let mut blocks = Vec::new();
        if root != 0 {
            let mut walk = Walk {
                file: &mut file,
                file_len,
                kind,
                channel: idx as u16,
                generation,
                item_bytes: match kind {
                    Kind::Adc => 2,
                    Kind::RealWave => 4,
                    _ => item_len.max(8),
                },
                interval_ticks,
                blocks: Vec::new(),
            };
            walk.node(root, 0)?;
            blocks = walk.blocks;
            blocks.sort_by_key(|b| b.start_tick);
        }
        let items = blocks.iter().map(|b| b.items.max(0) as u64).sum();

        channels.push(Channel {
            index: idx,
            title_raw: title.clone(),
            units_raw: units.clone(),
            comment_raw: comment.clone(),
            title,
            comment,
            kind,
            units,
            scale,
            offset,
            ideal_rate,
            sample_interval,
            blocks,
            items,
            n_extra: n_extra.min(i16::MAX as u64) as i16,
            wide: true,
        });
    }

    Ok(SonFile {
        path,
        system_id: 64,
        creator,
        comments,
        time_factor: tick,
        channels,
        file,
        son64: true,
    })
}
