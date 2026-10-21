//! Reader for CED Spike2 `.smr` (SON) files, versions 1 to 9.
//!
//! Layout recovered from the format as implemented by the open-source readers
//! (`neo.rawio.Spike2RawIO`, itself derived from sonpy and Malcolm Lidierth's SON
//! library) and checked against the example files on the Spike8 CD:
//!
//! ```text
//! file header   512 bytes
//! channel list  140 bytes per channel, starting at 512
//! data block    20-byte header, then `items` payload elements
//! ```
//!
//! All integers are little-endian.
//!
//! ADC values are converted to channel units as `count as f32 * scale / 65536.0 + offset`,
//! which is CED's documented conversion. Note that `neo` applies an extra factor of ten
//! (`gain = scale / 6553.6`); that is a neo convention, not the file's.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const FILE_HEADER_LEN: u64 = 512;
pub const CHANNEL_HEADER_LEN: u64 = 140;
pub const BLOCK_HEADER_LEN: u64 = 20;

/// Channel kinds, by the numeric code in the channel header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Empty,
    /// 16-bit sampled waveform.
    Adc,
    EventFall,
    EventRise,
    EventBoth,
    /// Timestamp plus a 32-bit marker code.
    Marker,
    /// Waveform attached to each event, 16-bit.
    AdcMark,
    /// Waveform attached to each event, 32-bit float.
    RealMark,
    /// Timestamp plus a text label.
    TextMark,
    /// 32-bit float sampled waveform.
    RealWave,
}

impl Kind {
    fn from_code(code: u8) -> Kind {
        match code {
            1 => Kind::Adc,
            2 => Kind::EventFall,
            3 => Kind::EventRise,
            4 => Kind::EventBoth,
            5 => Kind::Marker,
            6 => Kind::AdcMark,
            7 => Kind::RealMark,
            8 => Kind::TextMark,
            9 => Kind::RealWave,
            _ => Kind::Empty,
        }
    }

    /// Bytes per payload element inside a data block.
    pub fn item_bytes(self) -> usize {
        match self {
            Kind::Adc => 2,
            Kind::RealWave => 4,
            Kind::EventFall | Kind::EventRise | Kind::EventBoth => 4,
            Kind::Marker | Kind::AdcMark | Kind::RealMark | Kind::TextMark => 8,
            Kind::Empty => 1,
        }
    }

    pub fn is_waveform(self) -> bool {
        matches!(self, Kind::Adc | Kind::RealWave)
    }

    pub fn is_event(self) -> bool {
        matches!(
            self,
            Kind::EventFall | Kind::EventRise | Kind::EventBoth | Kind::Marker | Kind::TextMark
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Empty => "empty",
            Kind::Adc => "waveform",
            Kind::EventFall => "event (fall)",
            Kind::EventRise => "event (rise)",
            Kind::EventBoth => "event (both)",
            Kind::Marker => "marker",
            Kind::AdcMark => "waveform mark",
            Kind::RealMark => "waveform mark",
            Kind::TextMark => "text mark",
            Kind::RealWave => "waveform",
        }
    }
}

/// One data block belonging to a channel.
#[derive(Debug, Clone, Copy)]
pub struct Block {
    /// Absolute file offset of the first payload byte.
    pub data_pos: u64,
    /// Number of payload elements.
    pub items: i32,
    /// First tick covered by the block.
    pub start_tick: i32,
    /// Last tick covered by the block.
    pub end_tick: i32,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub index: usize,
    pub title: String,
    pub comment: String,
    pub kind: Kind,
    pub units: String,
    pub scale: f32,
    pub offset: f32,
    pub ideal_rate: f32,
    /// Seconds between samples; zero for event channels.
    pub sample_interval: f64,
    pub blocks: Vec<Block>,
    /// Total payload elements across all blocks.
    pub items: u64,
    /// Extra bytes per element (waveform marks); zero for plain channels.
    pub n_extra: i16,
}

impl Channel {
    pub fn sample_rate(&self) -> f64 {
        if self.sample_interval > 0.0 {
            1.0 / self.sample_interval
        } else {
            0.0
        }
    }
}

/// An event or marker timestamp.
#[derive(Debug, Clone)]
pub struct Event {
    /// Seconds from the start of the file.
    pub time: f64,
    /// Marker or text code, when the channel carries one.
    pub code: Option<i32>,
}

pub struct SonFile {
    pub path: PathBuf,
    pub system_id: i16,
    pub creator: String,
    pub comments: Vec<String>,
    /// Seconds per timestamp tick.
    pub time_factor: f64,
    pub channels: Vec<Channel>,
    file: File,
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn i16(&mut self) -> i16 {
        let v = i16::from_le_bytes([self.buf[self.at], self.buf[self.at + 1]]);
        self.at += 2;
        v
    }

    fn i32(&mut self) -> i32 {
        let v = i32::from_le_bytes([
            self.buf[self.at],
            self.buf[self.at + 1],
            self.buf[self.at + 2],
            self.buf[self.at + 3],
        ]);
        self.at += 4;
        v
    }

    fn f32(&mut self) -> f32 {
        f32::from_bits(self.i32() as u32)
    }

    fn f64(&mut self) -> f64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.at..self.at + 8]);
        self.at += 8;
        f64::from_le_bytes(b)
    }

    fn u8(&mut self) -> u8 {
        let v = self.buf[self.at];
        self.at += 1;
        v
    }

    fn fixed(&mut self, n: usize) -> String {
        let raw = &self.buf[self.at..self.at + n];
        self.at += n;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).trim_end().to_string()
    }

    /// Pascal-style string: one length byte, then that many characters, inside a fixed
    /// field of `n` bytes. SON stores channel titles, comments and units this way.
    fn pstr(&mut self, n: usize) -> String {
        let field = &self.buf[self.at..self.at + n];
        self.at += n;
        let len = (field[0] as usize).min(n.saturating_sub(1));
        let raw = &field[1..1 + len];
        let text: String = raw
            .iter()
            .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { ' ' })
            .collect();
        text.trim_end().to_string()
    }

    fn skip(&mut self, n: usize) {
        self.at += n;
    }
}

impl SonFile {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<SonFile> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;

        let mut header = [0u8; FILE_HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        let mut r = Reader::new(&header);

        let system_id = r.i16();
        let _copyright = r.fixed(10);
        let creator = r.fixed(8);
        let us_per_time = r.i16();
        let time_per_adc = r.i16();
        let _filestate = r.i16();
        let _first_data = r.i32();
        let n_channels = r.i16();
        let _chan_size = r.i16();
        let _extra_data = r.i16();
        let _buffersize = r.i16();
        let _os_format = r.i16();
        let _max_ftime = r.i32();
        let mut dtime_base = r.f64();
        let _datetime_detail = r.u8();
        let _datetime_year = r.i16();
        r.skip(52);
        let mut comments = Vec::new();
        for _ in 0..5 {
            let c = r.fixed(80);
            if !c.is_empty() {
                comments.push(c);
            }
        }

        // Before version 6 the header has no usable time base and no date fields.
        if system_id < 6 {
            dtime_base = 1e-6;
        }
        if !(dtime_base.is_finite() && dtime_base > 0.0) {
            dtime_base = 1e-6;
        }

        let time_factor = f64::from(us_per_time) * dtime_base;
        // Version 9 interleaves 512-byte disk blocks; the sample files are all older.
        let block_stride: u64 = if system_id == 9 { 512 } else { 1 };

        let mut channels = Vec::new();
        let mut chan_header = [0u8; CHANNEL_HEADER_LEN as usize];

        for idx in 0..n_channels.max(0) as usize {
            file.seek(SeekFrom::Start(FILE_HEADER_LEN + CHANNEL_HEADER_LEN * idx as u64))?;
            file.read_exact(&mut chan_header)?;
            let mut c = Reader::new(&chan_header);

            let _del_size = c.i16();
            let _next_del = c.i32();
            let firstblock = c.i32();
            let _lastblock = c.i32();
            let blocks_n = c.i16();
            let n_extra = c.i16();
            let _pre_trig = c.i16();
            let _free0 = c.i16();
            let _py_sz = c.i16();
            let _max_data = c.i16();
            let comment = c.pstr(72);
            let _max_chan_time = c.i32();
            let l_chan_dvd = c.i32();
            let _phy_chan = c.i16();
            let title = c.pstr(10);
            let ideal_rate = c.f32();
            let kind = Kind::from_code(c.u8());
            let _unused1 = c.u8();

            let mut units = String::new();
            let mut scale = 1.0f32;
            let mut offset = 0.0f32;

            match kind {
                Kind::Adc | Kind::AdcMark => {
                    scale = c.f32();
                    offset = c.f32();
                    units = c.pstr(6);
                }
                Kind::RealWave | Kind::RealMark => {
                    let _min = c.f32();
                    let _max = c.f32();
                    units = c.pstr(6);
                }
                _ => {}
            }

            let interleave = if kind.is_waveform() || kind == Kind::AdcMark {
                c.i16()
            } else {
                1
            };

            // Sample interval: before version 5 a divide factor multiplies both time
            // constants; later versions store it in l_chan_dvd.
            let sample_interval = if kind.is_waveform() || kind == Kind::AdcMark {
                if system_id < 5 {
                    f64::from(interleave.max(1)) * f64::from(us_per_time) * f64::from(time_per_adc) * 1e-6
                } else {
                    f64::from(l_chan_dvd) * time_factor
                }
            } else {
                0.0
            };

            // Walk the block chain.
            let mut blocks = Vec::new();
            let mut items = 0u64;
            let mut pos = if blocks_n > 0 {
                (firstblock as i64 * block_stride as i64).max(0) as u64
            } else {
                0
            };
            for _ in 0..blocks_n.max(0) {
                file.seek(SeekFrom::Start(pos))?;
                let mut bh = [0u8; BLOCK_HEADER_LEN as usize];
                if file.read_exact(&mut bh).is_err() {
                    break;
                }
                let mut b = Reader::new(&bh);
                let _pred = b.i32();
                let succ = b.i32();
                let start_tick = b.i32();
                let end_tick = b.i32();
                let _channel_num = b.i16();
                let block_items = b.i16();

                blocks.push(Block {
                    data_pos: pos + BLOCK_HEADER_LEN,
                    items: i32::from(block_items),
                    start_tick,
                    end_tick,
                });
                items += block_items.max(0) as u64;

                if succ <= 0 {
                    break;
                }
                pos = succ as u64 * block_stride;
            }

            // Unused/emptied channel slots are common: files reserve 32 headers and only
            // use a few. Spike2 does not show them, so neither do we.
            if kind == Kind::Empty {
                continue;
            }

            channels.push(Channel {
                index: idx,
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
                n_extra,
            });
        }

        Ok(SonFile {
            path,
            system_id,
            creator,
            comments,
            time_factor,
            channels,
            file,
        })
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// End of the recording, in seconds, taken from the last block of any channel.
    pub fn duration(&self) -> f64 {
        let mut last = 0.0f64;
        for ch in &self.channels {
            if let Some(b) = ch.blocks.last() {
                let t = f64::from(b.end_tick) * self.time_factor;
                if t > last {
                    last = t;
                }
            }
        }
        last
    }

    /// Read `count` samples starting at absolute sample index `first`, converted to the
    /// channel's units. Samples past the end come back as NaN.
    pub fn read_waveform(&mut self, ch: usize, first: u64, count: usize) -> io::Result<Vec<f32>> {
        let channel = &self.channels[ch];
        let mut out = vec![f32::NAN; count];
        if count == 0 || !channel.kind.is_waveform() {
            return Ok(out);
        }

        let is_adc = channel.kind == Kind::Adc;
        let elem = if is_adc { 2u64 } else { 4u64 };
        let scale = channel.scale;
        let offset = channel.offset;

        // Locate the block holding `first`.
        let mut base = 0u64;
        let mut cursor = first;
        let mut out_pos = 0usize;

        for block in &channel.blocks {
            let n = block.items.max(0) as u64;
            if cursor >= n {
                cursor -= n;
                base += n;
                continue;
            }

            let take = ((n - cursor) as usize).min(count - out_pos);
            let mut raw = vec![0u8; take * elem as usize];
            self.file.seek(SeekFrom::Start(block.data_pos + cursor * elem))?;
            self.file.read_exact(&mut raw)?;

            for (i, chunk) in raw.chunks_exact(elem as usize).enumerate() {
                let v = if is_adc {
                    let c = i16::from_le_bytes([chunk[0], chunk[1]]);
                    f32::from(c) * scale / 65536.0 + offset
                } else {
                    let bits = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    f32::from_bits(bits)
                };
                out[out_pos + i] = v;
            }

            out_pos += take;
            if out_pos >= count {
                break;
            }
            base += n;
            cursor = 0;
        }

        Ok(out)
    }

    /// Read every event on an event channel, with times in seconds.
    pub fn read_events(&mut self, ch: usize) -> io::Result<Vec<Event>> {
        let channel = &self.channels[ch];
        if !channel.kind.is_event() {
            return Ok(Vec::new());
        }
        let stride = channel.kind.item_bytes();
        let mut out = Vec::with_capacity(channel.items as usize);
        let tf = self.time_factor;

        for block in &channel.blocks {
            let n = block.items.max(0) as usize;
            let mut raw = vec![0u8; n * stride];
            self.file.seek(SeekFrom::Start(block.data_pos))?;
            self.file.read_exact(&mut raw)?;

            for chunk in raw.chunks_exact(stride) {
                let tick = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let code = if stride == 8 {
                    Some(i32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]))
                } else {
                    None
                };
                out.push(Event {
                    time: f64::from(tick) * tf,
                    code,
                });
            }
        }

        Ok(out)
    }

    /// Absolute sample index for a time, on a waveform channel.
    pub fn sample_at(&self, ch: usize, seconds: f64) -> u64 {
        let channel = &self.channels[ch];
        let tick = (seconds / self.time_factor) as i32;
        let mut base = 0u64;
        for block in &channel.blocks {
            if tick < block.start_tick {
                return base;
            }
            if tick <= block.end_tick {
                let offset = ((tick - block.start_tick) as f64 / channel.sample_interval.max(1e-12))
                    .round() as u64;
                return base + offset.min(block.items.max(0) as u64);
            }
            base += block.items.max(0) as u64;
        }
        base
    }

    /// Start time of a channel in seconds.
    pub fn start_time(&self, ch: usize) -> f64 {
        self.channels[ch]
            .blocks
            .first()
            .map(|b| f64::from(b.start_tick) * self.time_factor)
            .unwrap_or(0.0)
    }
}
