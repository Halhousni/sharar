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
use super::host::{Handled, Host, Mem};
use super::vm::{Arg, V};
use crate::{Kind, SonFile};
use std::path::PathBuf;

/// A time view on a data file.
struct DataView {
    handle: i64,
    file: SonFile,
}

pub struct Session {
    views: Vec<DataView>,
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
        Session { views: Vec::new(), log: 11, current: 11, next: 14, maps: Vec::new() }
    }

    /// Read files whose script path starts with `windows_prefix` from `dir` instead. A script
    /// written for Windows names `C:\\Users\\…\\data\\x.smr`; this lets it run elsewhere.
    pub fn map_path(&mut self, windows_prefix: &str, dir: impl Into<PathBuf>) {
        self.maps.push((windows_prefix.to_ascii_lowercase().replace('/', "\\"), dir.into()));
    }

    fn resolve(&self, name: &str) -> PathBuf {
        let norm = name.replace('/', "\\");
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

impl Host for Session {
    fn current_view(&self) -> i64 {
        self.current
    }

    fn set_view(&mut self, handle: i64) -> Result<(), RunError> {
        if handle == self.log || self.views.iter().any(|v| v.handle == handle) {
            self.current = handle;
            Ok(())
        } else {
            Err(RunError(super::vm::ERR_TYPE))
        }
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
                if kind != 0 {
                    return Ok(None);
                }
                match SonFile::open(self.resolve(&path)) {
                    Ok(file) => {
                        let handle = self.next;
                        self.next += 1;
                        self.views.push(DataView { handle, file });
                        self.current = handle;
                        some(V::Int(handle))
                    }
                    Err(_) => some(V::Int(-1)),
                }
            }
            "FileClose" => {
                let c = self.current;
                let Some(at) = self.views.iter().position(|v| v.handle == c) else {
                    return Ok(None);
                };
                self.views.remove(at);
                self.current = self.views.last().map(|v| v.handle).unwrap_or(self.log);
                some(V::Int(0))
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
                    ("ChanTitle$", Some(c)) => c.title.clone(),
                    ("ChanUnits$", Some(c)) => c.units.clone(),
                    (_, Some(c)) => c.comment.clone(),
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
                    .map(|c| if c.kind.is_waveform() { if name == "ChanScale" { c.scale } else { c.offset } } else { 0.0 })
                    .unwrap_or(0.0);
                some(V::Flt(v as f64))
            }
            "ChanVisible" => {
                let n = int(mem, &args[0])?;
                if self.view().is_none() {
                    return Ok(None);
                }
                some(V::Int(self.chan(n).is_some() as i64))
            }
            "NextTime" | "LastTime" if args.len() == 2 => {
                let (n, t) = (int(mem, &args[0])?, flt(mem, &args[1])?);
                if self.view().is_none() {
                    return Ok(None);
                }
                let Some(idx) = self.chan_pos(n) else { return some(V::Flt(NO_CHANNEL)) };
                // A time past the largest tick a file can hold finds nothing.
                if (t / self.view().unwrap().file.time_factor).abs() > i32::MAX as f64 {
                    return some(V::Flt(-1.0));
                }
                let kind = self.view().unwrap().file.channels[idx].kind;
                let tf = self.view().unwrap().file.time_factor;
                let next = name == "NextTime";
                let times: Vec<f64> = if kind.is_waveform() {
                    // Samples sit at start + k * interval ticks; find the first after (or last
                    // before) the time without listing them.
                    let ch = &self.view().unwrap().file.channels[idx];
                    let dt = ticks_per_sample(ch);
                    let first = ch.blocks.first().map(|b| b.start_tick).unwrap_or(0);
                    let total: i64 = ch.blocks.iter().map(|b| b.items.max(0) as i64).sum();
                    if dt <= 0 || total == 0 {
                        return some(V::Flt(-1.0));
                    }
                    let tick = t / tf;
                    let k = if next {
                        ((tick - first as f64) / dt as f64).floor() as i64 + 1
                    } else {
                        ((tick - first as f64) / dt as f64).ceil() as i64 - 1
                    };
                    let k = if next { k.max(0) } else { k.min(total - 1) };
                    if k < 0 || k >= total {
                        return some(V::Flt(-1.0));
                    }
                    return some(V::Flt((first + k * dt) as f64 * tf));
                } else {
                    let file = &mut self.view_mut().unwrap().file;
                    file.read_events(idx).map(|e| e.into_iter().map(|e| e.time).collect()).unwrap_or_default()
                };
                let hit = if next {
                    times.iter().copied().find(|&x| x > t)
                } else {
                    times.iter().copied().rev().find(|&x| x < t)
                };
                some(V::Flt(hit.unwrap_or(-1.0)))
            }
            "Count" if args.len() == 3 => {
                let (n, t0, t1) = (int(mem, &args[0])?, flt(mem, &args[1])?, flt(mem, &args[2])?);
                if self.view().is_none() {
                    return Ok(None);
                }
                let Some(idx) = self.chan_pos(n) else { return some(V::Flt(0.0)) };
                let tf = self.view().unwrap().file.time_factor;
                // Times are whole ticks; a range runs from its first tick up to, not
                // including, its last.
                let (a, b) = ((t0 / tf).round() as i64, (t1 / tf).round() as i64);
                let kind = self.view().unwrap().file.channels[idx].kind;
                if kind.is_waveform() {
                    // On a waveform channel `Count` is the mean of the samples whose ticks
                    // fall in the range.
                    let file = &mut self.view_mut().unwrap().file;
                    let ch = &file.channels[idx];
                    let dt = ticks_per_sample(ch);
                    let first = ch.blocks.first().map(|b| b.start_tick).unwrap_or(0);
                    let total: i64 = ch.blocks.iter().map(|b| b.items.max(0) as i64).sum();
                    if dt <= 0 || b <= a {
                        return some(V::Flt(0.0));
                    }
                    let ceil_div = |x: i64| -> i64 { (x + dt - 1).div_euclid(dt) };
                    let lo = ceil_div(a - first).max(0);
                    let hi = ceil_div(b - first).min(total);
                    if hi <= lo {
                        return some(V::Flt(0.0));
                    }
                    let data = file.read_waveform(idx, lo as u64, (hi - lo) as usize).unwrap_or_default();
                    let sum: f64 = data.iter().map(|&v| v as f64).sum();
                    return some(V::Flt(sum / data.len() as f64));
                }
                let file = &mut self.view_mut().unwrap().file;
                let events = file.read_events(idx).unwrap_or_default();
                let n = events.iter().filter(|e| {
                    let k = (e.time / tf).round() as i64;
                    k >= a && k < b
                }).count();
                some(V::Flt(n as f64))
            }
            _ => Ok(None),
        }
    }
}
