//! Spike2's world for a script: the open files and which view is current.
//!
//! A [`Session`] is a [`Host`] built on the `.smr` reader. It answers the commands that ask
//! about a view and its channels (`MaxTime`, `ChanKind`, `ChanTitle$`, `NextTime`, …) and the
//! ones that open and close files. What a command returns is checked against Spike2
//! by `tools/run_oracle.py`, on the example recordings, in `tools/run_cases/d_*.s2s`.
//!
//! Handles are the numbers `FileOpen` returns. Spike2 numbers views from a counter that
//! other windows share, so the values are not reproducible between runs and scripts do not
//! depend on them; here the log is 11 and files count up from 14, as the first runs did.

use super::exec::RunError;
use super::extfile::{BinFile, BinVal, TextFile};
use super::host::{Handled, Host, Mem};
use super::vm::{Arg, V};
use crate::{Kind, SonFile};
use std::path::PathBuf;

/// A time view on a data file, with the state a script can change.
struct DataView {
    handle: i64,
    file: SonFile,
    /// Hidden channels, by channel number. A channel in use is shown until hidden.
    hidden: Vec<i64>,
    selected: Vec<i64>,
    /// Cursors 1 to 9, at a time in seconds.
    cursors: [Option<f64>; 9],
    cursor_visible: [bool; 9],
}

impl DataView {
    fn new(handle: i64, file: SonFile) -> DataView {
        DataView { handle, file, hidden: Vec::new(), selected: Vec::new(), cursors: [None; 9], cursor_visible: [true; 9] }
    }
}

/// A text or binary file a script has open.
enum ExtKind {
    Text(TextFile),
    Bin(BinFile),
}

struct Ext {
    handle: i64,
    kind: ExtKind,
}

pub struct Session {
    views: Vec<DataView>,
    exts: Vec<Ext>,
    log: i64,
    current: i64,
    next: i64,
    maps: Vec<(String, PathBuf)>,
}

/// The code Spike2's file library returns for a channel that does not exist.
const NO_CHANNEL: f64 = -1505.0;

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session { views: Vec::new(), exts: Vec::new(), log: 11, current: 11, next: 14, maps: Vec::new() }
    }

    /// Read files whose script path starts with `windows_prefix` from `dir` instead. A script
    /// written for Windows names `C:\\Users\\…\\data\\x.smr`; this lets it run elsewhere.
    pub fn map_path(&mut self, windows_prefix: &str, dir: impl Into<PathBuf>) {
        self.maps.push((windows_prefix.to_ascii_lowercase().replace('/', "\\"), dir.into()));
    }

    fn resolve(&self, name: &str) -> PathBuf {
        // Windows takes a doubled separator as one.
        let mut norm = name.replace('/', "\\");
        while norm.contains("\\\\") {
            norm = norm.replace("\\\\", "\\");
        }
        let lower = norm.to_ascii_lowercase();
        for (prefix, dir) in &self.maps {
            if lower.starts_with(prefix.as_str()) {
                let rest = norm[prefix.len()..].replace('\\', "/");
                return dir.join(rest);
            }
        }
        PathBuf::from(name.replace('\\', "/"))
    }

    fn view(&self) -> Option<&DataView> {
        self.views.iter().find(|v| v.handle == self.current)
    }

    fn ext_mut(&mut self) -> Option<&mut Ext> {
        let c = self.current;
        self.exts.iter_mut().find(|e| e.handle == c)
    }

    fn view_mut(&mut self) -> Option<&mut DataView> {
        let c = self.current;
        self.views.iter_mut().find(|v| v.handle == c)
    }

    /// Where the channel numbered `n` (the script counts from one) sits in the file's list,
    /// which holds only the channels in use.
    fn chan_pos(&self, n: i64) -> Option<usize> {
        let f = &self.view()?.file;
        if n < 1 {
            return None;
        }
        f.channels.iter().position(|c| c.index == n as usize - 1)
    }

    /// The ticks of the items of the channel at `pos` in the file's list.
    fn items(&mut self, pos: usize) -> Items {
        let file = &mut self.view_mut().unwrap().file;
        let tf = file.time_factor;
        let ch = &file.channels[pos];
        if ch.kind.is_waveform() {
            let dt = ticks_per_sample(ch);
            let blocks = ch.blocks.iter().map(|b| (b.start_tick, b.items.max(0) as i64)).collect();
            return Items::Waveform { blocks, dt };
        }
        let ticks: Vec<i64> = if ch.kind.is_wavemark() {
            file.read_wavemarks(pos).unwrap_or_default().iter().map(|m| (m.time / tf).round() as i64).collect()
        } else {
            file.read_events(pos).unwrap_or_default().iter().map(|e| (e.time / tf).round() as i64).collect()
        };
        Items::Events(ticks)
    }

    /// The values of waveform samples `lo..hi` in the channel's units, worked out in double
    /// precision from the raw counts the way Spike2 does.
    fn wave_values(&mut self, pos: usize, lo: i64, hi: i64) -> Vec<f64> {
        let file = &mut self.view_mut().unwrap().file;
        let items = Items::Waveform {
            blocks: file.channels[pos].blocks.iter().map(|b| (b.start_tick, b.items.max(0) as i64)).collect(),
            dt: 0,
        };
        let (scale, offset) = (file.channels[pos].scale as f64, file.channels[pos].offset as f64);
        if file.channels[pos].kind == Kind::RealWave {
            return file.read_waveform(pos, lo as u64, (hi - lo) as usize).unwrap_or_default().iter().map(|&v| v as f64).collect();
        }
        raw_samples(file, pos, &items, lo, hi).into_iter().map(|r| r as f64 * scale / 6553.6 + offset).collect()
    }

    fn chan(&self, n: i64) -> Option<&crate::Channel> {
        let p = self.chan_pos(n)?;
        self.view().map(|v| &v.file.channels[p])
    }
}

/// Ticks between samples of a waveform channel, read off its first block with more than
/// one sample, since the interval the reader keeps is not always set.
fn ticks_per_sample(ch: &crate::Channel) -> i64 {
    ch.blocks
        .iter()
        .find(|b| b.items > 1)
        .map(|b| ((b.end_tick - b.start_tick) as f64 / (b.items - 1) as f64).round() as i64)
        .unwrap_or(0)
}

/// The ticks of a channel's items, kept without listing the samples of a waveform. A recording
/// that was paused has gaps between blocks, so a waveform's sample times are block by block.
enum Items {
    Waveform { blocks: Vec<(i64, i64)>, dt: i64 },
    Events(Vec<i64>),
}

impl Items {
    fn len(&self) -> i64 {
        match self {
            Items::Waveform { blocks, .. } => blocks.iter().map(|b| b.1).sum(),
            Items::Events(v) => v.len() as i64,
        }
    }

    /// The tick of item `g`.
    fn tick(&self, g: i64) -> i64 {
        match self {
            Items::Waveform { blocks, dt } => {
                let mut base = 0;
                for &(start, n) in blocks {
                    if g < base + n {
                        return start + (g - base) * dt;
                    }
                    base += n;
                }
                0
            }
            Items::Events(v) => v[g as usize],
        }
    }

    /// The index of the first item whose tick is at least `x`, or the number of items.
    fn first_ge(&self, x: i64) -> i64 {
        match self {
            Items::Waveform { blocks, dt } => {
                let mut base = 0;
                for &(start, n) in blocks {
                    if n == 0 {
                        continue;
                    }
                    if x <= start {
                        return base;
                    }
                    let last = start + (n - 1) * dt;
                    if x <= last {
                        return base + (x - start + dt - 1).div_euclid(*dt);
                    }
                    base += n;
                }
                base
            }
            Items::Events(v) => v.partition_point(|&t| t < x) as i64,
        }
    }
}

/// Raw ADC counts of samples `lo..hi`.
fn raw_samples(file: &mut SonFile, pos: usize, _items: &Items, lo: i64, hi: i64) -> Vec<i16> {
    file.read_adc_raw(pos, lo as u64, (hi - lo) as usize).unwrap_or_default()
}

fn kind_code(k: Kind) -> i64 {
    match k {
        Kind::Empty => 0,
        Kind::Adc => 1,
        Kind::EventFall => 2,
        Kind::EventRise => 3,
        Kind::EventBoth => 4,
        Kind::Marker => 5,
        Kind::AdcMark => 6,
        Kind::RealMark => 7,
        Kind::TextMark => 8,
        Kind::RealWave => 9,
    }
}

fn int(mem: &dyn Mem, a: &Arg) -> Result<i64, RunError> {
    match mem.value(a)? {
        V::Int(i) => Ok(i),
        V::Flt(f) => Ok(f as i64),
        V::Str(_) => Err(RunError(super::vm::ERR_TYPE)),
    }
}

fn flt(mem: &dyn Mem, a: &Arg) -> Result<f64, RunError> {
    match mem.value(a)? {
        V::Int(i) => Ok(i as f64),
        V::Flt(f) => Ok(f),
        V::Str(_) => Err(RunError(super::vm::ERR_TYPE)),
    }
}

impl Session {
    /// The binary file commands, on the current view, which is a binary file.
    fn bin_command(&mut self, name: &str, args: &[Arg], mem: &mut dyn Mem) -> Result<Handled, RunError> {
        let some = |v: V| Ok(Some(Some(v)));
        let Some(Ext { kind: ExtKind::Bin(f), .. }) = self.ext_mut() else { return Ok(None) };
        match name {
            "BWrite" | "BWriteSize" => {
                let (size, rest) = if name == "BWriteSize" {
                    (Some(int(mem, &args[0])?), &args[1..])
                } else {
                    (None, args)
                };
                // It answers with how many arguments it wrote; an array is one.
                let mut count = 0;
                for a in rest {
                    let vals: Vec<V> = match mem.array(a) {
                        Some((data, _)) => data,
                        None => vec![mem.value(a)?],
                    };
                    let mut ok = true;
                    for v in vals {
                        let bv = match v {
                            V::Int(i) => BinVal::Int(i),
                            V::Flt(x) => BinVal::Real(x),
                            V::Str(s) => BinVal::Str(s),
                        };
                        ok &= f.write(&bv, size).is_ok();
                    }
                    if ok {
                        count += 1;
                    }
                }
                some(V::Int(count))
            }
            "BRead" | "BReadSize" => {
                let (size, rest) = if name == "BReadSize" {
                    (Some(int(mem, &args[0])?), &args[1..])
                } else {
                    (None, args)
                };
                let mut count = 0;
                let mut eof = false;
                for a in rest {
                    let Some(t) = mem.target(a) else { return Err(RunError(super::vm::ERR_TYPE)) };
                    for k in 0..t.array_len.unwrap_or(1) {
                        let v = if eof {
                            None
                        } else {
                            match t.ty {
                                super::compile::Ty::Int => f.read_int(size).map(V::Int),
                                super::compile::Ty::Float => f.read_real(size).map(V::Flt),
                                super::compile::Ty::Str => f.read_str(size).map(V::Str),
                            }
                        };
                        if v.is_none() {
                            eof = true;
                        } else {
                            count += 1;
                        }
                        let v = v.unwrap_or(match t.ty {
                            super::compile::Ty::Int => V::Int(0),
                            super::compile::Ty::Float => V::Flt(0.0),
                            super::compile::Ty::Str => V::Str(String::new()),
                        });
                        match a {
                            Arg::Ref(r) if t.array_len.is_none() => mem.store(*r, v)?,
                            _ => mem.store_element(a, k, v)?,
                        }
                    }
                }
                some(V::Int(count))
            }
            "BSeek" => {
                if args.is_empty() {
                    return some(V::Int(f.position()));
                }
                let off = int(mem, &args[0])?;
                let mode = if args.len() > 1 { int(mem, &args[1])? } else { 0 };
                some(V::Int(f.seek(off, mode)))
            }
            "BRWEndian" => {
                let was = f.big_endian as i64;
                if let Some(a) = args.first() {
                    f.big_endian = int(mem, a)? != 0;
                }
                some(V::Int(was))
            }
            _ => Ok(None),
        }
    }

    /// `ChanData(chan, array[], t0, t1 {, &tFirst})`: the items of a channel from the tick of
    /// `t0` to the tick of `t1` inclusive, as many as the array holds. A real array gets
    /// times in seconds (events) or values in the channel's units (waveforms); an integer
    /// array gets ticks or raw ADC counts. Returns how many, or -1506 for a channel it
    /// cannot read.
    fn chan_data(&mut self, args: &[Arg], mem: &mut dyn Mem) -> Result<Handled, RunError> {
        let some = |v: V| Ok(Some(Some(v)));
        let (n, t0, t1) = (int(mem, &args[0])?, flt(mem, &args[2])?, flt(mem, &args[3])?);
        let Some((cur, dims)) = mem.array(&args[1]) else { return Ok(None) };
        if dims.len() != 1 {
            return Ok(None);
        }
        let want_int = matches!(cur.first(), Some(V::Int(_)));
        let cap = dims[0];
        let Some(idx) = self.chan_pos(n) else { return some(V::Int(-1506)) };
        let (tf, kind) = {
            let f = &self.view().unwrap().file;
            (f.time_factor, f.channels[idx].kind)
        };
        let (a, b) = ((t0 / tf).round() as i64, (t1 / tf).round() as i64 + 1);
        let mut out: Vec<V> = Vec::new();
        let mut first_time = 0.0;
        let is_event = kind.is_event() || kind.is_wavemark();
        if !is_event && !kind.is_waveform() {
            return some(V::Int(-1506));
        }
        let items = self.items(idx);
        if b > a {
            let lo = items.first_ge(a);
            let hi = items.first_ge(b).min(lo + cap as i64);
            if hi > lo {
                first_time = items.tick(lo) as f64 * tf;
                if is_event {
                    for g in lo..hi {
                        let k = items.tick(g);
                        out.push(if want_int { V::Int(k) } else { V::Flt(k as f64 * tf) });
                    }
                } else if want_int {
                    let file = &mut self.view_mut().unwrap().file;
                    out.extend(raw_samples(file, idx, &items, lo, hi).into_iter().map(|r| V::Int(r as i64)));
                } else {
                    out.extend(self.wave_values(idx, lo, hi).into_iter().map(V::Flt));
                }
            }
        }
        let count = out.len();
        let mut full = cur;
        for (slot, v) in full.iter_mut().zip(out) {
            *slot = v;
        }
        mem.set_array(&args[1], full)?;
        if count > 0 {
            if let Some(Arg::Ref(r)) = args.get(4) {
                mem.store(*r, V::Flt(first_time))?;
            }
        }
        some(V::Int(count as i64))
    }

    /// The cursor commands. A cursor sits at a time between 0 and the end of the file.
    fn cursor_command(&mut self, name: &str, args: &[Arg], mem: &mut dyn Mem) -> Result<Handled, RunError> {
        let some = |v: V| Ok(Some(Some(v)));
        let max = self.view().unwrap().file.duration();
        let clamp = |t: f64| t.max(0.0).min(max);
        let slot = |n: i64| -> Option<usize> { if (1..=9).contains(&n) { Some(n as usize - 1) } else { None } };
        match name {
            "Cursor" => {
                let n = int(mem, &args[0])?;
                if n >= 10 {
                    return Err(RunError(super::vm::ERR_INDEX));
                }
                let v = self.view_mut().unwrap();
                let Some(i) = slot(n) else { return some(V::Flt(if n == 0 { 0.0 } else { -1.0 })) };
                let prev = v.cursors[i];
                if args.len() > 1 && prev.is_some() {
                    v.cursors[i] = Some(clamp(flt(mem, &args[1])?));
                }
                some(V::Flt(prev.unwrap_or(-1.0)))
            }
            "CursorNew" => {
                let pos = if args.is_empty() { 0.0 } else { flt(mem, &args[0])? };
                let want = if args.len() > 1 { Some(int(mem, &args[1])?) } else { None };
                let v = self.view_mut().unwrap();
                let i = match want {
                    Some(n) => match slot(n) {
                        // A number already in use is refused with -1, not replaced.
                        Some(i) if v.cursors[i].is_some() => return some(V::Int(-1)),
                        Some(i) => i,
                        None => return Err(RunError(super::vm::ERR_INDEX)),
                    },
                    None => match v.cursors.iter().position(|c| c.is_none()) {
                        Some(i) => i,
                        None => return some(V::Int(-1)),
                    },
                };
                v.cursors[i] = Some(clamp(pos));
                some(V::Int(i as i64 + 1))
            }
            "CursorExists" => {
                let n = int(mem, &args[0])?;
                if !(0..=9).contains(&n) {
                    return Err(RunError(super::vm::ERR_INDEX));
                }
                let v = self.view().unwrap();
                // Cursor 0 is the one that always exists.
                some(V::Int((n == 0 || slot(n).map(|i| v.cursors[i].is_some()).unwrap_or(false)) as i64))
            }
            "CursorValid" => {
                let n = int(mem, &args[0])?;
                let v = self.view().unwrap();
                some(V::Int(slot(n).map(|i| v.cursors[i].is_some()).unwrap_or(false) as i64))
            }
            "CursorDelete" => {
                let v = self.view_mut().unwrap();
                // With no number it removes the highest-numbered cursor.
                let n = if args.is_empty() {
                    v.cursors.iter().rposition(|c| c.is_some()).map(|i| i as i64 + 1).unwrap_or(0)
                } else {
                    int(mem, &args[0])?
                };
                let had = slot(n).map(|i| v.cursors[i].is_some()).unwrap_or(false);
                if let Some(i) = slot(n) {
                    v.cursors[i] = None;
                }
                // It answers with the number it removed, or 0.
                some(V::Int(if had { n } else { 0 }))
            }
            "CursorSet" => {
                // CursorSet(n, pos1, pos2 …): n cursors at the given times; any others go.
                let n = int(mem, &args[0])?;
                let mut set = [None; 9];
                for k in 0..(n.clamp(0, 9) as usize) {
                    let t = if k + 1 < args.len() { flt(mem, &args[k + 1])? } else { 0.0 };
                    set[k] = Some(clamp(t));
                }
                self.view_mut().unwrap().cursors = set;
                Ok(Some(None))
            }
            _ => Ok(None),
        }
    }
}

impl Host for Session {
    fn current_view(&self) -> i64 {
        self.current
    }

    fn set_view(&mut self, handle: i64) -> Result<(), RunError> {
        if handle == self.log
            || self.views.iter().any(|v| v.handle == handle)
            || self.exts.iter().any(|e| e.handle == handle)
        {
            self.current = handle;
            Ok(())
        } else {
            Err(RunError(super::vm::ERR_TYPE))
        }
    }

    fn print(&mut self, text: &str) -> bool {
        if let Some(Ext { kind: ExtKind::Text(t), .. }) = self.ext_mut() {
            if t.writing {
                t.print(text);
                return true;
            }
        }
        false
    }

    fn command(
        &mut self,
        name: &str,
        _variant: usize,
        args: &[Arg],
        mem: &mut dyn Mem,
    ) -> Result<Handled, RunError> {
        let some = |v: V| Ok(Some(Some(v)));
        match name {
            "LogHandle" => some(V::Int(self.log)),
            "ViewKind" => {
                let h = if args.is_empty() { self.current } else { int(mem, &args[0])? };
                if h == self.log {
                    some(V::Int(1))
                } else if self.views.iter().any(|v| v.handle == h) {
                    some(V::Int(0))
                } else if let Some(e) = self.exts.iter().find(|e| e.handle == h) {
                    some(V::Int(match e.kind {
                        ExtKind::Text(_) => 8,
                        ExtKind::Bin(_) => 9,
                    }))
                } else {
                    some(V::Int(-1))
                }
            }
            "FileOpen" => {
                let path = match mem.value(&args[0])? {
                    V::Str(s) => s,
                    _ => return Err(RunError(super::vm::ERR_TYPE)),
                };
                let kind = int(mem, &args[1])?;
                if kind == 8 || kind == 9 {
                    let mode = if args.len() > 2 { int(mem, &args[2])? } else { 0 };
                    let p = self.resolve(&path);
                    let opened = if kind == 8 {
                        if mode & 1 != 0 {
                            Ok(ExtKind::Text(TextFile::create(p)))
                        } else {
                            TextFile::open(p).map(ExtKind::Text)
                        }
                    } else {
                        BinFile::open(&p, mode).map(ExtKind::Bin)
                    };
                    return match opened {
                        Ok(kind) => {
                            let handle = self.next;
                            self.next += 1;
                            self.exts.push(Ext { handle, kind });
                            self.current = handle;
                            some(V::Int(handle))
                        }
                        Err(_) => some(V::Int(-1518)),
                    };
                }
                if kind != 0 {
                    return Ok(None);
                }
                match SonFile::open(self.resolve(&path)) {
                    Ok(file) => {
                        let handle = self.next;
                        self.next += 1;
                        self.views.push(DataView::new(handle, file));
                        self.current = handle;
                        some(V::Int(handle))
                    }
                    Err(_) => some(V::Int(-1)),
                }
            }
            "FileClose" => {
                let c = self.current;
                if let Some(at) = self.exts.iter().position(|e| e.handle == c) {
                    let e = self.exts.remove(at);
                    if let ExtKind::Text(t) = e.kind {
                        let _ = t.close();
                    }
                } else if let Some(at) = self.views.iter().position(|v| v.handle == c) {
                    self.views.remove(at);
                } else {
                    return Ok(None);
                }
                // The most recently opened view still open becomes the current one.
                let newest = self.views.iter().map(|v| v.handle).chain(self.exts.iter().map(|e| e.handle)).max();
                self.current = newest.unwrap_or(self.log);
                some(V::Int(0))
            }
            "Read" => {
                let line = match self.ext_mut() {
                    Some(Ext { kind: ExtKind::Text(t), .. }) if !t.writing => t.read_line(),
                    _ => return Ok(None),
                };
                let Some(line) = line else { return some(V::Int(-1530)) };
                let cfg = mem.read_cfg().clone();
                let n = super::text::read_values(&line, &cfg, args, mem)?;
                some(V::Int(n))
            }
            "BWrite" | "BWriteSize" | "BRead" | "BReadSize" | "BSeek" | "BRWEndian"
                if matches!(self.ext_mut(), Some(Ext { kind: ExtKind::Bin(_), .. })) =>
            {
                self.bin_command(name, args, mem)
            }
            "MaxTime" => match self.view() {
                Some(v) => some(V::Flt(v.file.duration())),
                None => Ok(None),
            },
            "FileTimeBase" => match self.view() {
                Some(v) => some(V::Flt(v.file.time_factor * 1e6)),
                None => Ok(None),
            },
            "ChanKind" => {
                let n = int(mem, &args[0])?;
                if self.view().is_none() {
                    return Ok(None);
                }
                some(V::Int(self.chan(n).map(|c| kind_code(c.kind)).unwrap_or(0)))
            }
            "ChanTitle$" | "ChanUnits$" | "ChanComment$" => {
                let n = int(mem, &args[0])?;
                if self.view().is_none() || args.len() > 1 {
                    return Ok(None);
                }
                let c = self.chan(n);
                let s = match (name, c) {
                    ("ChanComment$", None) if n >= 1 => "No comment".to_string(),
                    (_, None) => String::new(),
                    ("ChanTitle$", Some(c)) => c.title_raw.clone(),
                    ("ChanUnits$", Some(c)) => c.units_raw.clone(),
                    (_, Some(c)) => c.comment_raw.clone(),
                };
                some(V::Str(s))
            }
            "ChanScale" | "ChanOffset" if args.len() == 1 => {
                let n = int(mem, &args[0])?;
                if self.view().is_none() {
                    return Ok(None);
                }
                let v = self
                    .chan(n)
                    .map(|c| if c.kind.is_waveform() || c.kind.is_wavemark() { if name == "ChanScale" { c.scale } else { c.offset } } else { 0.0 })
                    .unwrap_or(0.0);
                some(V::Flt(v as f64))
            }
            "ChanVisible" => {
                let n = int(mem, &args[0])?;
                let Some(v) = self.view() else { return Ok(None) };
                some(V::Int((self.chan(n).is_some() && !v.hidden.contains(&n)) as i64))
            }
            "ChanShow" | "ChanHide" => {
                if self.view().is_none() {
                    return Ok(None);
                }
                let mut chans = Vec::new();
                for a in args {
                    match mem.array(a) {
                        // A channel list: its first element is how many follow.
                        Some((data, _)) => {
                            let nums: Vec<i64> = data
                                .iter()
                                .map(|d| match d {
                                    V::Int(i) => *i,
                                    V::Flt(f) => *f as i64,
                                    V::Str(_) => 0,
                                })
                                .collect();
                            let n = nums.first().copied().unwrap_or(0).max(0) as usize;
                            chans.extend(nums.iter().skip(1).take(n));
                        }
                        None => chans.push(int(mem, a)?),
                    }
                }
                let hide = name == "ChanHide";
                for c in chans {
                    if self.chan(c).is_none() {
                        continue;
                    }
                    let v = self.view_mut().unwrap();
                    v.hidden.retain(|&h| h != c);
                    if hide {
                        v.hidden.push(c);
                    }
                }
                Ok(Some(None))
            }
            "ChanSelect" => {
                let n = int(mem, &args[0])?;
                if self.view().is_none() {
                    return Ok(None);
                }
                let exists = self.chan(n).is_some();
                let v = self.view_mut().unwrap();
                let was = v.selected.contains(&n);
                if args.len() > 1 {
                    let on = int(mem, &args[1])? != 0;
                    v.selected.retain(|&c| c != n);
                    if on && exists {
                        v.selected.push(n);
                    }
                }
                some(V::Int(was as i64))
            }
            "ChanData" if args.len() >= 4 && self.view().is_some() => self.chan_data(args, mem),
            "Cursor" | "CursorNew" | "CursorExists" | "CursorDelete" | "CursorValid" | "CursorSet"
                if self.view().is_some() =>
            {
                self.cursor_command(name, args, mem)
            }
            "NextTime" | "LastTime" if args.len() == 2 => {
                let (n, t) = (int(mem, &args[0])?, flt(mem, &args[1])?);
                if self.view().is_none() {
                    return Ok(None);
                }
                let Some(pos) = self.chan_pos(n) else { return some(V::Flt(NO_CHANNEL)) };
                let tf = self.view().unwrap().file.time_factor;
                // A time past the largest tick a file can hold finds nothing.
                if (t / tf).abs() > i32::MAX as f64 {
                    return some(V::Flt(-1.0));
                }
                let x = (t / tf).round() as i64;
                let items = self.items(pos);
                let g = if name == "NextTime" { items.first_ge(x + 1) } else { items.first_ge(x) - 1 };
                if g < 0 || g >= items.len() {
                    return some(V::Flt(-1.0));
                }
                some(V::Flt(items.tick(g) as f64 * tf))
            }
            "Count" if args.len() == 3 => {
                let (n, t0, t1) = (int(mem, &args[0])?, flt(mem, &args[1])?, flt(mem, &args[2])?);
                if self.view().is_none() {
                    return Ok(None);
                }
                let Some(pos) = self.chan_pos(n) else { return some(V::Flt(0.0)) };
                let tf = self.view().unwrap().file.time_factor;
                // Times are whole ticks; a range runs from its first tick up to, not
                // including, its last.
                let (a, b) = ((t0 / tf).round() as i64, (t1 / tf).round() as i64);
                let items = self.items(pos);
                let (lo, hi) = (items.first_ge(a), items.first_ge(b));
                if b <= a || hi <= lo {
                    return some(V::Flt(0.0));
                }
                if !self.view().unwrap().file.channels[pos].kind.is_waveform() {
                    return some(V::Flt((hi - lo) as f64));
                }
                // On a waveform channel `Count` is the mean of the samples in the range.
                let (scale, offset, kind) = {
                    let c = &self.view().unwrap().file.channels[pos];
                    (c.scale as f64, c.offset as f64, c.kind)
                };
                if kind == Kind::Adc {
                    // The mean of the raw counts, then scaled: that is the order Spike2 works in,
                    // and it differs from averaging the scaled values in the last bit.
                    let file = &mut self.view_mut().unwrap().file;
                    let raw = file.read_adc_raw(pos, lo as u64, (hi - lo) as usize).unwrap_or_default();
                    let sum: i64 = raw.iter().map(|&r| r as i64).sum();
                    return some(V::Flt((sum as f64 / raw.len() as f64) * (scale / 6553.6) + offset));
                }
                let vals = self.wave_values(pos, lo, hi);
                let sum: f64 = vals.iter().sum();
                some(V::Flt(sum / vals.len() as f64))
            }
            _ => Ok(None),
        }
    }
}
