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
                let ch = &self.view().unwrap().file.channels[idx];
                let kind = ch.kind;
                let interval = ch.sample_interval;
                let start = self.view().unwrap().file.start_time(idx);
                let next = name == "NextTime";
                let times: Vec<f64> = if kind.is_waveform() {
                    // The times of the samples after `start`, not materialised.
                    let f = &self.view().unwrap().file;
                    let chn = &f.channels[idx];
                    let total = chn.blocks.iter().map(|b| b.items.max(0) as u64).sum::<u64>();
                    let k = if next {
                        ((t - start) / interval).floor() + 1.0
                    } else {
                        ((t - start) / interval).ceil() - 1.0
                    };
                    let k = k.max(0.0);
                    if (k as u64) < total && (k >= 0.0) && ((next && t < start + interval * (total as f64 - 1.0)) || (!next && t > start)) {
                        return some(V::Flt(start + k * interval));
                    }
                    return some(V::Flt(-1.0));
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
            _ => Ok(None),
        }
    }
}
