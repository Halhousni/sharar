//! Sharar — a reader and display for CED Spike2 `.smr` files.
//!
//! Milestone 1: open a `.smr` and draw it the way Spike2 does — stacked channel bands
//! against a shared time axis, two draggable cursors with read-outs, event and marker
//! rows, zoom and pan.
//!
//! Milestone 2: per-channel gain and a scale bar, horizontal grid with a zero line,
//! cursor values per channel in the gutter, and channel selection.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};

use smr::{Kind, SonFile};

mod acq;

/// Spike2's default channel colours, in order.
pub(crate) const CHANNEL_COLORS: [Color32; 8] = [
    Color32::from_rgb(0, 0, 0),
    Color32::from_rgb(200, 0, 0),
    Color32::from_rgb(0, 110, 0),
    Color32::from_rgb(0, 0, 200),
    Color32::from_rgb(160, 0, 160),
    Color32::from_rgb(0, 140, 140),
    Color32::from_rgb(150, 90, 0),
    Color32::from_rgb(90, 90, 90),
];

const CURSOR_1: Color32 = Color32::from_rgb(220, 30, 30);
const CURSOR_2: Color32 = Color32::from_rgb(30, 60, 220);
const AXIS: Color32 = Color32::from_rgb(70, 70, 70);
const GRID: Color32 = Color32::from_rgb(226, 229, 233);
const GRID_STRONG: Color32 = Color32::from_rgb(198, 203, 210);
const PAPER: Color32 = Color32::from_rgb(255, 255, 255);
const SELECT_TINT: Color32 = Color32::from_rgb(204, 228, 247);

/// Decimated envelope of one channel over the visible window.
struct Envelope {
    low: Vec<f32>,
    high: Vec<f32>,
}

struct Cache {
    from_sample: u64,
    to_sample: u64,
    width: usize,
    dc_window: usize,
    filter_tag: u64,
    env: Envelope,
}

/// Window statistics between the two cursors.
#[derive(Clone, Copy)]
struct WindowStats {
    n: u64,
    mean: f64,
    rms: f64,
    min: f32,
    max: f32,
}

/// Channel facts the view needs, cloned so drawing can take `&mut self` for caching.
struct ChanMeta {
    title: String,
    units: String,
    kind: Kind,
    items: u64,
}

/// One entry in a channel's process chain, in the order Analysis > Process…
/// applies them. Spike2's own chain is the same idea: a named list of
/// operations a channel runs through before it is drawn.
#[derive(Clone, PartialEq)]
enum Process {
    /// A running-mean DC removal over a window of samples.
    DcRemove(usize),
    /// A filter described by parameters, resolved when the data is read.
    Fir(smr::FilterSpec),
    /// A finished coefficient set, from the filter dialog's band table.
    Coeffs(Vec<f64>),
}

impl Process {
    fn label(&self) -> String {
        match self {
            Process::DcRemove(n) => format!("DC remove, {n} samples"),
            Process::Fir(s) => format!(
                "{} {:.2} Hz, width {:.2} Hz, {:.0} dB{}",
                s.kind.label(),
                s.cutoff,
                s.width,
                s.atten,
                if s.iir { ", IIR" } else { "" }
            ),
            Process::Coeffs(c) => format!("band table design, {} taps", c.len()),
        }
    }

    /// Run this process over a channel's samples.
    fn apply(&self, data: &mut [f32], rate: f64) {
        match self {
            Process::DcRemove(n) => {
                if *n > 1 {
                    smr::dc_remove(data, *n)
                }
            }
            Process::Fir(s) => smr::apply_filter(data, s, rate),
            Process::Coeffs(coef) => {
                let mut wide: Vec<f64> = data.iter().map(|v| *v as f64).collect();
                smr::arr_fir(&mut wide, coef);
                for (d, w) in data.iter_mut().zip(wide.iter()) {
                    *d = *w as f32;
                }
            }
        }
    }

    /// Bumped into the cache key so a chain edit invalidates cached envelopes.
    fn tag(&self) -> u64 {
        match self {
            Process::DcRemove(n) => *n as u64,
            Process::Fir(s) => s.tag(),
            Process::Coeffs(c) => c.len() as u64 ^ c.first().map(|v| v.to_bits()).unwrap_or(0),
        }
    }
}

/// Where a frequency sits across the response plot, 0 to 1. Linear is `f/0.5`;
/// logarithmic runs from 1 mHz to Nyquist, which is `CChangeFilter::FreqToPixel`
/// with its floor of 1e-3 in normalised frequency.
fn freq_to_frac(f: f64, log: bool) -> f64 {
    if log {
        let lo = 1e-3f64;
        (f.max(lo).ln() - lo.ln()) / (0.5f64.ln() - lo.ln())
    } else {
        f / 0.5
    }
}

/// The inverse of `freq_to_frac`, for turning a dragged pointer position back
/// into a frequency.
fn frac_to_freq(t: f64, log: bool) -> f64 {
    if log {
        let lo = 1e-3f64;
        (lo.ln() + t * (0.5f64.ln() - lo.ln())).exp()
    } else {
        t * 0.5
    }
}

/// How a cursor is tagged at the top of the plot, Spike2's Cursor > Label Mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorLabel {
    Number,
    Position,
    Both,
    None,
}

impl CursorLabel {
    fn tag(self, i: usize, t: f64) -> Option<String> {
        match self {
            CursorLabel::Number => Some(format!("{}", i + 1)),
            CursorLabel::Position => Some(format!("{t:.4}")),
            CursorLabel::Both => Some(format!("{} {t:.4}", i + 1)),
            CursorLabel::None => None,
        }
    }
}

struct App {
    son: Option<SonFile>,
    visible: Vec<bool>,
    /// Vertical gain per channel, multiplied onto the automatic fit.
    gain: Vec<f32>,
    /// View > Y Axis Range…: an explicit displayed range in channel units.
    /// `None` means the automatic fit, which is the default.
    y_range: Vec<Option<(f32, f32)>>,
    yaxis_dialog: bool,
    /// The process chain per channel, applied in order before drawing.
    processes: Vec<Vec<Process>>,
    /// The chain count at the time the Measurements were computed, so an edit
    /// to any chain invalidates them.
    chain_tag: u64,
    /// Analysis > Process… dialog.
    process_dialog: bool,
    /// View > Show/Hide Channel….
    showhide_dialog: bool,
    /// File > Recent Files, newest first, persisted under the config directory.
    recent: Vec<PathBuf>,
    /// Working values for the Y Axis Range dialog.
    y_min: f32,
    y_max: f32,
    selected: Option<usize>,
    t_start: f64,
    t_end: f64,
    cursors: [f64; 2],
    /// Cursor > Label Mode.
    cursor_label: CursorLabel,
    /// Cursor > Display Y Values.
    show_y: bool,
    /// View > X Axis Range… and the values it edits.
    xaxis_dialog: bool,
    x_left: f64,
    x_right: f64,
    /// Explicit large-tick spacing in seconds; zero means automatic.
    x_tick: f64,
    x_subs: u32,
    dragging: Option<usize>,
    caches: Vec<Option<Cache>>,
    /// Value at cursor 1 and cursor 2, per channel.
    readouts: Vec<Option<(f32, f32)>>,
    /// DC-remove window in samples per channel; 0 means the process is off.
    dc_window: Vec<usize>,
    filter_dialog: bool,
    /// View > Channel Information…, Spike2's read-out for the selected channel.
    chan_info: bool,
    /// The design the filter dialog is previewing, keyed by its parameters so a
    /// Remez run per frame does not happen.
    f_preview: Option<(u64, smr::Design)>,
    /// An explicit band table, which is what Spike2's dialog builds internally
    /// from its edges. Empty means "derive bands from the quick parameters".
    f_bands: Vec<smr::Band>,
    /// Tap count for the band table path; zero asks the designer's own rule.
    f_taps: usize,
    /// Why the last preview attempt failed, so the plot can say so instead of
    /// showing an empty box.
    f_err: Option<String>,
    /// The "Log frequency" switch dialog 129 carries. Sharar's plot is linear
    /// unless this is ticked.
    f_log: bool,
    f_kind: smr::FilterKind,
    f_cutoff: f64,
    f_width: f64,
    f_atten: f64,
    f_iir: bool,
    /// Statistics between the cursors, per channel.
    stats: Vec<Option<WindowStats>>,
    /// Cursor pair the statistics were computed for, so they are not recomputed per frame.
    stats_key: Option<(u64, u64)>,
    status: String,
    /// Live acquisition, when running with --record.
    live: Option<acq::Live>,
    /// Whether the measurements panel is open.
    show_measure: bool,
    /// Result of the last peri-stimulus average.
    avg: Option<smr::Average>,
    avg_label: String,
    avg_dialog: bool,
    avg_event_ch: usize,
    avg_pre: f64,
    avg_post: f64,
    avg_bins: usize,
    open_dialog: bool,
    path_input: String,
    error: Option<String>,
    /// Receives the file chosen in the native picker, which runs on its own thread so the
    /// window keeps drawing while it is open.
    picker: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            son: None,
            visible: Vec::new(),
            gain: Vec::new(),
            y_range: Vec::new(),
            processes: Vec::new(),
            chain_tag: 0,
            process_dialog: false,
            yaxis_dialog: false,
            showhide_dialog: false,
            recent: Vec::new(),
            y_min: -1.0,
            y_max: 1.0,
            selected: None,
            t_start: 0.0,
            t_end: 1.0,
            cursors: [0.0, 0.0],
            cursor_label: CursorLabel::Number,
            show_y: true,
            xaxis_dialog: false,
            x_left: 0.0,
            x_right: 1.0,
            x_tick: 0.0,
            x_subs: 1,
            dragging: None,
            caches: Vec::new(),
            readouts: Vec::new(),
            dc_window: Vec::new(),
            filter_dialog: false,
            chan_info: false,
            f_preview: None,
            f_bands: Vec::new(),
            f_taps: 0,
            f_err: None,
            f_log: false,
            f_kind: smr::FilterKind::LowPass,
            f_cutoff: 150.0,
            f_width: 100.0,
            f_atten: 50.0,
            f_iir: false,
            stats: Vec::new(),
            stats_key: None,
            status: "No file open".to_string(),
            live: None,
            show_measure: true,
            avg: None,
            avg_label: String::new(),
            avg_dialog: false,
            avg_event_ch: 0,
            avg_pre: 0.05,
            avg_post: 0.2,
            avg_bins: 200,
            open_dialog: false,
            path_input: String::new(),
            error: None,
            picker: None,
        };
        if let Some(p) = path {
            // The recent list is read before the file is opened so the file
            // being opened lands at the top of it.
            app.load_recent();
            app.load(&p);
        }
        app
    }

    /// Show the system file picker. Does nothing while one is already open.
    fn pick_file(&mut self, ctx: &egui::Context) {
        if self.picker.is_some() {
            return;
        }
        let start = self
            .son
            .as_ref()
            .and_then(|s| s.path.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok());
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .set_title("Open Spike2 data file")
                .add_filter("Spike2 data (.smr, .smrx)", &["smr", "smrx"])
                .add_filter("All files", &["*"]);
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_file());
            ctx.request_repaint();
        });
        self.picker = Some(rx);
    }

    /// Load the file the picker returned, once it has.
    fn poll_picker(&mut self) {
        let Some(rx) = &self.picker else { return };
        match rx.try_recv() {
            Ok(choice) => {
                self.picker = None;
                if let Some(path) = choice {
                    self.load(&path);
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picker = None,
        }
    }

    fn load(&mut self, path: &Path) {
        match SonFile::open(path) {
            Ok(f) => {
                self.note_recent(path);
                let duration = f.duration().max(0.001);
                let n = f.channels.len();
                self.visible = vec![true; n];
                self.gain = vec![1.0; n];
                self.y_range = vec![None; n];
                self.processes = vec![Vec::new(); n];
                self.caches = (0..n).map(|_| None).collect();
                self.readouts = vec![None; n];
                self.stats = vec![None; n];
                self.stats_key = None;
                self.dc_window = vec![0; n];
                self.processes = vec![Vec::new(); n];
                // A corner an eighth of the fastest rate, with a transition a fifth of
                // that, which is the scale FIRQuick's own defaults sit at.
                let fastest = f.channels.iter().map(|c| c.sample_rate()).fold(0.0, f64::max);
                self.f_cutoff = (fastest / 8.0).max(1.0);
                self.f_width = (self.f_cutoff * 0.2).clamp(0.01, (fastest * 0.1).max(0.01));
                self.avg = None;
                self.avg_label.clear();
                self.avg_event_ch = f
                    .channels
                    .iter()
                    .position(|c| c.kind.is_event())
                    .unwrap_or(0);
                self.selected = f.channels.iter().position(|c| c.kind.is_waveform());
                self.t_start = 0.0;
                self.t_end = duration;
                self.cursors = [duration * 0.25, duration * 0.5];
                self.status = format!(
                    "{}  ·  {} channels  ·  {:.3} s",
                    f.file_name(),
                    n,
                    duration
                );
                self.error = None;
                self.son = Some(f);
                self.refresh_readouts();
            }
            Err(e) => {
                self.error = Some(format!("{}: {e}", path.display()));
                self.status = "Open failed".to_string();
            }
        }
    }

    /// Sample both cursors on every waveform channel. One sample per cursor, so this is
    /// cheap enough to redo each frame.
    fn refresh_readouts(&mut self) {
        let Some(son) = self.son.as_ref() else {
            return;
        };
        let n = son.channels.len();
        let duration = son.duration();
        let waveform: Vec<bool> = son.channels.iter().map(|c| c.kind.is_waveform()).collect();
        let mut out = vec![None; n];

        for i in 0..n {
            if !waveform[i] {
                continue;
            }
            let mut at = |t: f64| -> f32 {
                let son = self.son.as_mut().unwrap();
                let idx = son.sample_at(i, t.clamp(0.0, duration));
                son.read_waveform(i, idx, 1)
                    .ok()
                    .and_then(|v| v.first().copied())
                    .unwrap_or(f32::NAN)
            };
            let a = at(self.cursors[0]);
            let b = at(self.cursors[1]);
            out[i] = Some((a, b));
        }

        self.readouts = out;
    }

    /// Mean, RMS and extremes between the cursors, per waveform channel.
    fn refresh_stats(&mut self) {
        let Some(son) = self.son.as_ref() else {
            return;
        };
        let key = (self.cursors[0].to_bits(), self.cursors[1].to_bits());
        if self.stats_key == Some(key) && self.stats.len() == son.channels.len() {
            return;
        }

        let n_channels = son.channels.len();
        let waveform: Vec<bool> = son.channels.iter().map(|c| c.kind.is_waveform()).collect();
        let (lo_t, hi_t) = if self.cursors[0] <= self.cursors[1] {
            (self.cursors[0], self.cursors[1])
        } else {
            (self.cursors[1], self.cursors[0])
        };

        let mut out = vec![None; n_channels];
        for i in 0..n_channels {
            if !waveform[i] {
                continue;
            }
            let son = self.son.as_mut().unwrap();
            let a = son.sample_at(i, lo_t);
            let b = son.sample_at(i, hi_t);
            if b <= a {
                continue;
            }
            // Cap the work: a cursor pair spanning a long recording should not freeze the UI.
            let count = ((b - a) as usize).min(4_000_000);
            let Ok(mut data) = son.read_waveform(i, a, count) else {
                continue;
            };
            let dc = self.dc_window.get(i).copied().unwrap_or(0);
            if dc > 1 {
                smr::dc_remove(&mut data, dc);
            }
            // The channel's whole chain, in the order the user set it.
            if let Some(chain) = self.processes.get(i) {
                let rate = self
                    .son
                    .as_ref()
                    .and_then(|s| s.channels.get(i).map(|c| c.sample_rate()))
                    .unwrap_or(0.0);
                for step in chain {
                    step.apply(&mut data, rate);
                }
            }

            let mut n = 0u64;
            let mut sum = 0f64;
            let mut sumsq = 0f64;
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for v in data.iter().filter(|v| v.is_finite()) {
                n += 1;
                sum += f64::from(*v);
                sumsq += f64::from(*v) * f64::from(*v);
                mn = mn.min(*v);
                mx = mx.max(*v);
            }
            if n > 0 {
                let mean = sum / n as f64;
                out[i] = Some(WindowStats {
                    n,
                    mean,
                    rms: (sumsq / n as f64).sqrt(),
                    min: mn,
                    max: mx,
                });
            }
        }

        self.stats = out;
        self.stats_key = Some(key);
    }

    fn fit(&mut self) {
        if let Some(son) = &self.son {
            self.t_start = 0.0;
            self.t_end = son.duration().max(0.001);
            self.caches.iter_mut().for_each(|c| *c = None);
        }
    }

    fn zoom(&mut self, factor: f64, anchor: f64) {
        if self.son.is_none() {
            return;
        }
        let span = (self.t_end - self.t_start).max(1e-6);
        let new_span = (span * factor).clamp(1e-6, 1e7);
        let frac = ((anchor - self.t_start) / span).clamp(0.0, 1.0);
        self.t_start = anchor - new_span * frac;
        self.t_end = self.t_start + new_span;
        self.clamp_view();
        self.caches.iter_mut().for_each(|c| *c = None);
    }

    fn pan(&mut self, seconds: f64) {
        self.t_start += seconds;
        self.t_end += seconds;
        self.clamp_view();
        self.caches.iter_mut().for_each(|c| *c = None);
    }

    /// Keep the view inside the recording: Spike2 never shows time before the first
    /// sample or past the last one.
    fn clamp_view(&mut self) {
        let duration = self.son.as_ref().map(|s| s.duration()).unwrap_or(0.0);
        if duration <= 0.0 {
            return;
        }
        let mut span = (self.t_end - self.t_start).max(1e-9).min(duration);
        if self.t_start < 0.0 {
            self.t_start = 0.0;
            self.t_end = span;
        }
        if self.t_end > duration {
            self.t_end = duration;
            self.t_start = (duration - span).max(0.0);
        }
        span = self.t_end - self.t_start;
        if span <= 0.0 {
            self.t_start = 0.0;
            self.t_end = duration;
        }
    }

    /// Open the digital-filter dialog (used by the launch flag).
    fn open_filter_dialog(&mut self) {
        if self.son.is_some() {
            self.filter_dialog = true;
        }
    }

    /// Append a step to the selected channel's chain (used by the launch flag).
    fn apply_filter_to_selected(&mut self, spec: smr::FilterSpec) {
        if let Some(i) = self.selected {
            if let Some(chain) = self.processes.get_mut(i) {
                chain.push(Process::Fir(spec));
            }
            self.caches[i] = None;
            self.stats_key = None;
            self.chain_tag = self.chain_tag.wrapping_add(1);
        }
    }

    /// Open the average dialog with a result already computed.
    fn enable_average(&mut self) {
        if self.son.is_none() {
            return;
        }
        self.avg_dialog = true;
        self.compute_average();
    }

    /// Turn DC remove on for the selected channel, with a 0.2 s window.
    fn enable_dc(&mut self) {
        let Some(i) = self.selected else { return };
        let rate = self
            .son
            .as_ref()
            .and_then(|s| s.channels.get(i).map(|c| c.sample_rate()))
            .unwrap_or(0.0);
        if rate <= 0.0 {
            return;
        }
        if let Some(w) = self.dc_window.get_mut(i) {
            *w = ((rate * 0.2).round() as usize).max(3) | 1;
        }
        self.caches[i] = None;
        self.stats_key = None;
    }

    /// Average the selected waveform channel around every event on `avg_event_ch`.
    fn compute_average(&mut self) {
        let Some(wave_ch) = self.selected else { return };
        let Some(son) = self.son.as_ref() else { return };
        if !son.channels[wave_ch].kind.is_waveform() {
            return;
        }
        let ev_ch = self.avg_event_ch;
        if ev_ch >= son.channels.len() || !son.channels[ev_ch].kind.is_event() {
            return;
        }
        let interval = son.channels[wave_ch].sample_interval;
        if interval <= 0.0 {
            return;
        }
        let total = son.channels[wave_ch].items.min(8_000_000) as usize;
        let start_time = son.start_time(wave_ch);
        let wave_title = son.channels[wave_ch].title.clone();

        let Ok(events) = self.son.as_mut().unwrap().read_events(ev_ch) else {
            return;
        };
        let times: Vec<f64> = events.iter().map(|e| e.time).collect();
        let Ok(samples) = self.son.as_mut().unwrap().read_waveform(wave_ch, 0, total) else {
            return;
        };

        let avg = smr::peri_stimulus_average(
            &samples,
            start_time,
            interval,
            &times,
            self.avg_pre,
            self.avg_post,
            self.avg_bins,
        );
        let used = avg.counts.first().copied().unwrap_or(0);
        self.avg_label = format!(
            "{wave_title} around {} events on '{}' ({} trials)",
            times.len(),
            self.son.as_ref().unwrap().channels[ev_ch].title,
            used
        );
        self.avg = Some(avg);
    }

    /// Write the selected channel over the visible window as `time,value` rows.
    /// The config file holding the recent list, `~/.config/sharar/recent`.
    fn recent_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("sharar").join("recent"))
    }

    fn load_recent(&mut self) {
        let Some(p) = Self::recent_path() else { return };
        if let Ok(text) = std::fs::read_to_string(p) {
            self.recent = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .take(8)
                .collect();
        }
    }

    fn save_recent(&self) {
        let Some(p) = Self::recent_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = self
            .recent
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(p, text);
    }

    /// Move `path` to the front of the recent list, drop duplicates and bound
    /// the length. Spike2's recent list behaves the same way.
    fn note_recent(&mut self, path: &Path) {
        let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(8);
        self.save_recent();
    }

    /// File > Revert: reopen from disk, keeping the view where it was.
    fn revert(&mut self) {
        let Some(path) = self.son.as_ref().map(|s| s.path.clone()) else { return };
        let (a, b, sel) = (self.t_start, self.t_end, self.selected);
        match SonFile::open(&path) {
            Ok(f) => {
                let n = f.channels.len();
                self.son = Some(f);
                self.caches = (0..n).map(|_| None).collect();
                self.readouts = vec![None; n];
                self.processes = vec![Vec::new(); n];
                self.stats = vec![None; n];
                self.stats_key = None;
                self.y_range = vec![None; n];
                self.processes = vec![Vec::new(); n];
                self.gain = vec![1.0; n];
                self.selected = sel.filter(|s| *s < n);
                self.t_start = a;
                self.t_end = b;
                self.clamp_view();
                self.status = format!("reverted {}", path.display());
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Edit > Copy: the measurement read-out as text, the way Spike2 copies
    /// cursor values to the clipboard.
    fn copy_measurements(&mut self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "cursors\t{:.4}\t{:.4}\tdt\t{:.4}\n",
            self.cursors[0],
            self.cursors[1],
            self.cursors[1] - self.cursors[0]
        ));
        let names: Vec<String> = self
            .son
            .as_ref()
            .map(|s| s.channels.iter().map(|c| c.title.clone()).collect())
            .unwrap_or_default();
        for (i, n) in names.iter().enumerate() {
            if let Some(Some((a, b))) = self.readouts.get(i) {
                out.push_str(&format!("{n}\t{a:.6}\t{b:.6}\n"));
            }
        }
        out
    }

    /// Edit > Copy for Spreadsheet: the visible window of the selected channel
    /// as tab separated time and value columns.
    fn copy_spreadsheet(&mut self) -> Option<String> {
        let ch = self.selected?;
        let son = self.son.as_ref()?;
        if !son.channels[ch].kind.is_waveform() {
            return None;
        }
        let a = son.sample_at(ch, self.t_start.min(self.t_end));
        let b = son.sample_at(ch, self.t_start.max(self.t_end));
        let interval = son.channels[ch].sample_interval;
        let start = son.start_time(ch) + a as f64 * interval;
        let count = (b.saturating_sub(a) as usize).min(200_000);
        let data = self.son.as_mut()?.read_waveform(ch, a, count).ok()?;
        let mut out = String::from("time_s\tvalue\n");
        for (i, v) in data.iter().enumerate() {
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("{:.6}\t{:.6}\n", start + i as f64 * interval, v),
            );
        }
        Some(out)
    }

    fn export_csv(&mut self) {
        let Some(ch) = self.selected else { return };
        let Some(son) = self.son.as_ref() else { return };
        if !son.channels[ch].kind.is_waveform() {
            self.error = Some("select a waveform channel first".to_string());
            return;
        }
        let a = son.sample_at(ch, self.t_start.min(self.t_end));
        let b = son.sample_at(ch, self.t_start.max(self.t_end));
        if b <= a {
            self.error = Some("no samples in the visible window".to_string());
            return;
        }
        let interval = son.channels[ch].sample_interval;
        let start = son.start_time(ch) + a as f64 * interval;
        let title = if son.channels[ch].title.is_empty() {
            format!("channel{}", ch + 1)
        } else {
            son.channels[ch].title.replace(' ', "_")
        };
        let stem = son.path.with_extension("");
        let out_path = stem.with_file_name(format!(
            "{}_{}.csv",
            stem.file_name().unwrap_or_default().to_string_lossy(),
            title
        ));

        let count = ((b - a) as usize).min(20_000_000);
        let Ok(data) = self.son.as_mut().unwrap().read_waveform(ch, a, count) else {
            return;
        };
        let mut text = String::with_capacity(count * 16);
        text.push_str("time_s,value\n");
        for (i, v) in data.iter().enumerate() {
            let t = start + i as f64 * interval;
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{t:.6},{v:.6}\n"));
        }
        match std::fs::write(&out_path, text) {
            Ok(()) => self.status = format!("wrote {} ({} samples)", out_path.display(), count),
            Err(e) => self.error = Some(format!("{}: {e}", out_path.display())),
        }
    }

    /// The status line, for the `--export` path to report on the console.
    fn status_message(&self) -> &str {
        &self.status
    }

    fn set_gain(&mut self, factor: f32) {
        if let Some(i) = self.selected {
            if let Some(g) = self.gain.get_mut(i) {
                *g = (*g * factor).clamp(0.01, 1000.0);
            }
        }
    }

    fn build_envelope(&mut self, channel: usize, from: u64, to: u64, width: usize) -> bool {
        let Some(son) = self.son.as_mut() else {
            return false;
        };
        if width == 0 || to <= from {
            return false;
        }

        let dc = self.dc_window.get(channel).copied().unwrap_or(0);
        let chain: Vec<Process> = self
            .processes
            .get(channel)
            .cloned()
            .unwrap_or_default();
        let filter_tag = chain
            .iter()
            .fold(0u64, |a, p| a.wrapping_mul(31).wrapping_add(p.tag()));
        let hit = self.caches[channel]
            .as_ref()
            .map(|c| {
                c.from_sample == from
                    && c.to_sample == to
                    && c.width == width
                    && c.dc_window == dc
                    && c.filter_tag == filter_tag
            })
            .unwrap_or(false);
        if hit {
            return true;
        }

        let n = (to - from) as usize;
        let mut data = match son.read_waveform(channel, from, n) {
            Ok(d) => d,
            Err(_) => return false,
        };
        if dc > 1 {
            // Edge windows are truncated to the visible range, so the very first and last
            // few pixels of a processed trace can differ slightly from Spike2's.
            smr::dc_remove(&mut data, dc);
        }
        if !chain.is_empty() {
            let rate = son
                .channels
                .get(channel)
                .map(|c| c.sample_rate())
                .unwrap_or(0.0);
            for step in &chain {
                step.apply(&mut data, rate);
            }
        }

        let mut low = vec![f32::INFINITY; width];
        let mut high = vec![f32::NEG_INFINITY; width];
        for (i, v) in data.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            let col = (i * width / n.max(1)).min(width - 1);
            if *v < low[col] {
                low[col] = *v;
            }
            if *v > high[col] {
                high[col] = *v;
            }
        }
        for col in 0..width {
            if !low[col].is_finite() {
                low[col] = 0.0;
                high[col] = 0.0;
            }
        }

        self.caches[channel] = Some(Cache {
            from_sample: from,
            to_sample: to,
            width,
            dc_window: dc,
            filter_tag,
            env: Envelope { low, high },
        });
        true
    }

    fn x_of(&self, rect: Rect, t: f64) -> f32 {
        let span = (self.t_end - self.t_start).max(1e-12);
        rect.left() + ((t - self.t_start) / span) as f32 * rect.width()
    }

    fn t_at(&self, rect: Rect, x: f32) -> f64 {
        let span = (self.t_end - self.t_start).max(1e-12);
        self.t_start + f64::from((x - rect.left()) / rect.width().max(1.0)) * span
    }
}

/// A "round" number near `raw`, for axis and scale-bar steps.
fn nice_number(raw: f64) -> f64 {
    if raw <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    step * mag
}

fn nice_step(span: f64, target_ticks: f64) -> f64 {
    nice_number(span / target_ticks.max(1.0))
}

fn format_value(v: f32) -> String {
    if !v.is_finite() {
        return "-".to_string();
    }
    let a = v.abs();
    if a >= 1000.0 || (a > 0.0 && a < 0.001) {
        format!("{v:.3e}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.4}")
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(live) = &mut self.live {
            live.step();
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }

        self.poll_picker();
        self.refresh_readouts();
        self.refresh_stats();

        // ── menu bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        self.pick_file(ui.ctx());
                        ui.close();
                    }
                    if ui.button("Open by path…").clicked() {
                        self.open_dialog = true;
                        ui.close();
                    }
                    if ui.button("Close").clicked() {
                        self.son = None;
                    }
                    if ui
                        .add_enabled(self.son.is_some(), egui::Button::new("Revert to Saved"))
                        .clicked()
                    {
                        self.revert();
                    }
                    ui.menu_button("Recent Files", |ui| {
                        if self.recent.is_empty() {
                            ui.label(egui::RichText::new("(none)").weak());
                        }
                        let mut pick: Option<PathBuf> = None;
                        for path in self.recent.clone() {
                            let label = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string());
                            if ui
                                .button(label)
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                pick = Some(path);
                            }
                        }
                        if let Some(path) = pick {
                            self.load(&path);
                            ui.close();
                        }
                    });
                    ui.separator();
                    if ui.button("Export channel (visible window) as CSV…").clicked() {
                        self.export_csv();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Select all channels").clicked() {
                        self.visible.iter_mut().for_each(|v| *v = true);
                    }
                    if ui.button("Clear channel selection").clicked() {
                        self.visible.iter_mut().for_each(|v| *v = false);
                    }
                    ui.separator();
                    if ui
                        .add_enabled(self.son.is_some(), egui::Button::new("Copy"))
                        .clicked()
                    {
                        let text = self.copy_measurements();
                        ui.ctx().copy_text(text);
                        self.status = "copied measurements".to_string();
                    }
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Copy for Spreadsheet"))
                        .clicked()
                    {
                        match self.copy_spreadsheet() {
                            Some(text) => {
                                ui.ctx().copy_text(text);
                                self.status = "copied window as columns".to_string();
                            }
                            None => self.error = Some("select a waveform channel first".to_string()),
                        }
                    }
                    ui.separator();
                    if ui.button("Reset channel gain").clicked() {
                        self.gain.iter_mut().for_each(|g| *g = 1.0);
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Fit to window").clicked() {
                        self.fit();
                    }
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Channel Information…"))
                        .clicked()
                    {
                        self.chan_info = true;
                    }
                    if ui.button("Zoom in").clicked() {
                        let a = self.cursors[0];
                        self.zoom(0.5, a);
                    }
                    if ui.button("Zoom out").clicked() {
                        let a = self.cursors[0];
                        self.zoom(2.0, a);
                    }
                    ui.separator();
                    if ui.button("X Axis Range…").clicked() {
                        self.x_left = self.t_start;
                        self.x_right = self.t_end;
                        self.xaxis_dialog = true;
                    }
                    if ui.button("Y Axis Range…").clicked() {
                        // Seed the dialog from whatever the channel shows now, so
                        // opening it never jumps the trace.
                        if let Some(ci) = self.selected {
                            let (a, b) = self.y_range[ci].unwrap_or_else(|| {
                                match self.stats.get(ci).and_then(|s| *s) {
                                    Some(s) => (s.min, s.max),
                                    None => (-1.0, 1.0),
                                }
                            });
                            self.y_min = a;
                            self.y_max = b;
                            self.yaxis_dialog = true;
                        }
                    }
                    if ui.button("Show/Hide Channel…").clicked() {
                        self.showhide_dialog = true;
                    }
                });
                ui.menu_button("Analysis", |ui| {
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("Process…"))
                        .clicked()
                    {
                        self.process_dialog = true;
                    }
                    ui.separator();
                    ui.checkbox(&mut self.show_measure, "Show measurements");
                    ui.separator();
                    if let Some(i) = self.selected {
                        let rate = self
                            .son
                            .as_ref()
                            .and_then(|s| s.channels.get(i).map(|c| c.sample_rate()))
                            .unwrap_or(0.0);
                        let mut on = self.dc_window.get(i).copied().unwrap_or(0) > 1;
                        if ui.checkbox(&mut on, "DC remove on selected channel").changed() {
                            let window = if on {
                                ((rate * 0.2).round() as usize).max(3) | 1
                            } else {
                                0
                            };
                            if let Some(w) = self.dc_window.get_mut(i) {
                                *w = window;
                            }
                            self.caches[i] = None;
                            self.stats_key = None;
                        }
                        if on && rate > 0.0 {
                            let w = self.dc_window.get(i).copied().unwrap_or(0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "window {w} samples ({:.1} ms)",
                                    w as f64 / rate * 1000.0
                                ))
                                .small()
                                .weak(),
                            );
                        }
                    }
                    ui.separator();
                    if ui.button("Digital filters…").clicked() {
                        self.filter_dialog = true;
                    }
                    if ui
                        .button("Average around events…")
                        .clicked()
                    {
                        self.avg_dialog = true;
                    }
                });
                ui.menu_button("Cursor", |ui| {
                    // Spike2's Cursor menu is the largest in the program; these
                    // are the entries that act on the two cursors this view has.
                    if ui.button("Display All").clicked() {
                        self.cursors = [self.t_start, self.t_end];
                    }
                    ui.menu_button("Label Mode", |ui| {
                        for (m, name) in [
                            (CursorLabel::Position, "Position"),
                            (CursorLabel::Number, "Number"),
                            (CursorLabel::Both, "Position and Number"),
                            (CursorLabel::None, "None"),
                        ] {
                            ui.selectable_value(&mut self.cursor_label, m, name);
                        }
                    });
                    ui.checkbox(&mut self.show_y, "Display Y Values");
                    ui.menu_button("Move to", |ui| {
                        let mid = (self.t_start + self.t_end) * 0.5;
                        for (name, t) in [
                            ("View Start", self.t_start),
                            ("View Mid", mid),
                            ("View End", self.t_end),
                        ] {
                            if ui.button(name).clicked() {
                                self.cursors[0] = t;
                            }
                        }
                    });
                    // Search steps through the selected channel's event times,
                    // which is what Spike2's Search Right/Left walk over.
                    // read_events needs a mutable reader, so the menu borrows it
                    // for the one call rather than for the whole closure.
                    let events: Vec<f64> = match (self.selected, self.son.as_mut()) {
                        (Some(i), Some(s)) => s
                            .read_events(i)
                            .map(|ev| ev.iter().map(|e| e.time).collect())
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!events.is_empty(), egui::Button::new("Search Left"))
                            .clicked()
                        {
                            let t = self.cursors[0];
                            if let Some(prev) = events.iter().filter(|e| **e < t - 1e-9).next_back() {
                                self.cursors[0] = *prev;
                            }
                        }
                        if ui
                            .add_enabled(!events.is_empty(), egui::Button::new("Search Right"))
                            .clicked()
                        {
                            let t = self.cursors[0];
                            if let Some(next) = events.iter().find(|e| **e > t + 1e-9) {
                                self.cursors[0] = *next;
                            }
                        }
                    });
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Cursor modes (free, vertical, measurement) are not built.")
                            .small()
                            .weak(),
                    );
                });
                ui.menu_button("Sample", |ui| {
                    ui.label(
                        egui::RichText::new("Sampling configuration needs the 1401; not built.")
                            .small()
                            .weak(),
                    );
                });
                ui.menu_button("Script", |ui| {
                    ui.label(
                        egui::RichText::new("The .s2s script language is not built.").small().weak(),
                    );
                });
                ui.menu_button("Window", |ui| {
                    if ui.button("Fit to window").clicked() {
                        self.fit();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("One data window per file at present.").small().weak());
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Sharar — read and display CED Spike2 .smr and .smrx files.");
                    ui.label("Wheel zooms, middle-drag pans, F fits, +/- gain, D DC remove.");
                });
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        self.son.as_ref().map(|s| s.file_name()).unwrap_or_default(),
                    )
                    .strong(),
                );
            });
        });

        // ── toolbar ─────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open").clicked() {
                    self.pick_file(ui.ctx());
                }
                ui.separator();
                if ui.button("Fit").clicked() {
                    self.fit();
                }
                if ui.button("+").clicked() {
                    let a = self.cursors[0];
                    self.zoom(0.5, a);
                }
                if ui.button("−").clicked() {
                    let a = self.cursors[0];
                    self.zoom(2.0, a);
                }
                ui.separator();
                ui.label(format!(
                    "view {:.3} – {:.3} s  ({:.3} s)",
                    self.t_start,
                    self.t_end,
                    self.t_end - self.t_start
                ));
                if let Some(i) = self.selected {
                    ui.separator();
                    let g = self.gain.get(i).copied().unwrap_or(1.0);
                    ui.label(format!("ch {} gain ×{g:.2}", i + 1));
                    let w = self.dc_window.get(i).copied().unwrap_or(0);
                    if w > 1 {
                        ui.separator();
                        ui.label(format!("DC remove {w} samples"));
                    }
                    if let Some(chain) = self.processes.get(i).filter(|c| !c.is_empty()) {
                        ui.separator();
                        for step in chain {
                            ui.label(egui::RichText::new(step.label()).small());
                        }
                    }
                }
            });
        });

        // ── status bar ──────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if self.live.is_some() {
                        "Recording"
                    } else if self.son.is_some() {
                        "For Help, press F1"
                    } else {
                        "Open a .smr or .smrx file to begin"
                    })
                    .weak(),
                );
                ui.separator();
                ui.label(&self.status);
                ui.separator();
                let dt = self.cursors[1] - self.cursors[0];
                ui.label(format!(
                    "Cursor 1: {:.4} s   Cursor 2: {:.4} s   Δt: {:.4} s   {:.2} Hz",
                    self.cursors[0],
                    self.cursors[1],
                    dt,
                    if dt.abs() > 1e-9 { 1.0 / dt.abs() } else { 0.0 }
                ));
            });
        });

        // ── channel list ────────────────────────────────────────────────────
        let mut toggle: Option<usize> = None;
        let mut select: Option<usize> = None;
        egui::SidePanel::left("channels")
            .default_width(232.0)
            .show(ctx, |ui| {
                ui.heading("Channels");
                ui.separator();
                if let Some(live) = &self.live {
                    ui.label(
                        egui::RichText::new(format!("recording · {}", live.describe()))
                            .small()
                            .weak(),
                    );
                    for (i, title) in live.channels().iter().enumerate() {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(14.0, 14.0),
                                egui::Sense::hover(),
                            );
                            ui.painter()
                                .rect_filled(rect, 2.0, CHANNEL_COLORS[i % CHANNEL_COLORS.len()]);
                            ui.label(title);
                        });
                    }
                    return;
                }
                let Some(son) = &self.son else {
                    ui.label("No file open.");
                    return;
                };
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, ch) in son.channels.iter().enumerate() {
                        let color = CHANNEL_COLORS[i % CHANNEL_COLORS.len()];
                        let is_sel = self.selected == Some(i);
                        egui::Frame::NONE
                            .fill(if is_sel { SELECT_TINT } else { Color32::TRANSPARENT })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let mut on = self.visible.get(i).copied().unwrap_or(true);
                                    if ui.checkbox(&mut on, "").changed() {
                                        toggle = Some(i);
                                    }
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(14.0, 14.0),
                                        egui::Sense::click(),
                                    );
                                    if resp.clicked() {
                                        select = Some(i);
                                    }
                                    ui.painter().rect_filled(rect, 2.0, color);
                                    ui.vertical(|ui| {
                                        let title = if ch.title.is_empty() {
                                            format!("Channel {}", i + 1)
                                        } else {
                                            ch.title.clone()
                                        };
                                        if ui
                                            .label(
                                                egui::RichText::new(title).color(color).strong(),
                                            )
                                            .clicked()
                                        {
                                            select = Some(i);
                                        }
                                        let mut line = format!(
                                            "{} · {}",
                                            ch.kind.label(),
                                            if ch.units.is_empty() { "-" } else { &ch.units }
                                        );
                                        if ch.sample_rate() > 0.0 {
                                            line.push_str(&format!(" · {:.6} Hz", ch.sample_rate()));
                                        }
                                        ui.label(egui::RichText::new(line).small().weak());
                                        if let Some(Some((a, b))) = self.readouts.get(i) {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "1: {}   2: {}",
                                                    format_value(*a),
                                                    format_value(*b)
                                                ))
                                                .monospace()
                                                .size(10.0),
                                            );
                                        }
                                    });
                                });
                            });
                        ui.separator();
                    }
                });
            });
        if let Some(i) = toggle {
            if let Some(v) = self.visible.get_mut(i) {
                *v = !*v;
            }
        }
        if let Some(i) = select {
            self.selected = Some(i);
        }

        // ── measurements between the cursors ────────────────────────────────
        if self.show_measure && self.son.is_some() {
            egui::SidePanel::right("measure")
                .default_width(230.0)
                .show(ctx, |ui| {
                    ui.heading("Measurements");
                    ui.label(
                        egui::RichText::new(format!(
                            "cursors {:.4} – {:.4} s ({:.4} s)",
                            self.cursors[0].min(self.cursors[1]),
                            self.cursors[0].max(self.cursors[1]),
                            (self.cursors[1] - self.cursors[0]).abs()
                        ))
                        .small()
                        .weak(),
                    );
                    ui.separator();

                    let Some(son) = &self.son else { return };
                    if !son.channels.iter().any(|c| c.kind.is_waveform()) {
                        ui.label(
                            egui::RichText::new("No waveform channels in this file.").weak(),
                        );
                    }
                    for (i, ch) in son.channels.iter().enumerate() {
                        if !ch.kind.is_waveform() {
                            continue;
                        }
                        let color = CHANNEL_COLORS[i % CHANNEL_COLORS.len()];
                        let title = if ch.title.is_empty() {
                            format!("Channel {}", i + 1)
                        } else {
                            ch.title.clone()
                        };
                        ui.label(egui::RichText::new(title).color(color).strong());
                        match self.stats.get(i).and_then(|s| *s) {
                            Some(st) => {
                                egui::Grid::new(format!("stats{i}"))
                                    .num_columns(2)
                                    .spacing([10.0, 2.0])
                                    .show(ui, |ui| {
                                        let units = if ch.units.is_empty() {
                                            ""
                                        } else {
                                            ch.units.as_str()
                                        };
                                        for (name, value) in [
                                            ("samples", format!("{}", st.n)),
                                            ("mean", format!("{} {units}", format_value(st.mean as f32))),
                                            ("RMS", format!("{} {units}", format_value(st.rms as f32))),
                                            ("min", format!("{} {units}", format_value(st.min))),
                                            ("max", format!("{} {units}", format_value(st.max))),
                                            ("peak-peak", format!("{} {units}", format_value(st.max - st.min))),
                                        ] {
                                            ui.label(egui::RichText::new(name).small().weak());
                                            ui.label(egui::RichText::new(value).monospace().size(10.0));
                                            ui.end_row();
                                        }
                                    });
                            }
                            None => {
                                ui.label(egui::RichText::new("no data between cursors").weak());
                            }
                        }
                        ui.separator();
                    }
                });
        }

        // ── data view ───────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(live) = &self.live {
                live.draw(ui);
                return;
            }
            if self.son.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a .smr or .smrx file: File → Open…");
                });
                return;
            }

            let duration = self.son.as_ref().unwrap().duration();
            let meta: Vec<ChanMeta> = self
                .son
                .as_ref()
                .unwrap()
                .channels
                .iter()
                .map(|c| ChanMeta {
                    title: c.title.clone(),
                    units: c.units.clone(),
                    kind: c.kind,
                    items: c.items,
                })
                .collect();

            let full = ui.available_rect_before_wrap();
            let axis_h = 26.0;
            let outer_plot =
                Rect::from_min_max(full.min, Pos2::new(full.right(), full.bottom() - axis_h));
            let axis_rect =
                Rect::from_min_max(Pos2::new(full.left(), outer_plot.bottom()), full.max);

            // Spike2 keeps channel names in a left margin rather than over the data.
            let gutter = 132.0;
            let plot = Rect::from_min_max(
                Pos2::new(outer_plot.left() + gutter, outer_plot.top()),
                outer_plot.max,
            );

            let response = ui.allocate_rect(full, egui::Sense::click_and_drag());
            let painter = ui.painter_at(full);
            painter.rect_filled(full, 0.0, PAPER);

            let visible: Vec<usize> = (0..meta.len())
                .filter(|i| self.visible.get(*i).copied().unwrap_or(false))
                .collect();
            if visible.is_empty() {
                painter.text(
                    plot.center(),
                    Align2::CENTER_CENTER,
                    "No channels selected",
                    FontId::proportional(14.0),
                    AXIS,
                );
            }

            // waveform bands share the space left over by the thin event rows
            let event_rows = visible
                .iter()
                .filter(|i| !meta[**i].kind.is_waveform())
                .count();
            let event_h = 26.0;
            let wave_channels = visible.len() - event_rows;
            let wave_h = ((plot.height() - event_rows as f32 * event_h).max(0.0)
                / wave_channels.max(1) as f32)
                .max(0.0);
            // A file with only event channels gets taller rows instead of an empty pane.
            let row_h = if wave_channels == 0 {
                ((plot.height() - 4.0) / event_rows.max(1) as f32).min(96.0)
            } else {
                event_h
            };

            let mut band_tops: Vec<(usize, f32, f32)> = Vec::new();
            let mut y = plot.top() + 4.0;

            for &ci in &visible {
                let is_sel = self.selected == Some(ci);
                let color = CHANNEL_COLORS[ci % CHANNEL_COLORS.len()];
                let title = if meta[ci].title.is_empty() {
                    format!("Ch {}", ci + 1)
                } else {
                    meta[ci].title.clone()
                };

                if meta[ci].kind.is_waveform() {
                    let band = Rect::from_min_max(
                        Pos2::new(plot.left(), y),
                        Pos2::new(plot.right(), (y + wave_h - 6.0).min(plot.bottom())),
                    );
                    band_tops.push((ci, band.top(), band.bottom()));
                    y += wave_h;

                    if is_sel {
                        painter.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(outer_plot.left(), band.top()),
                                Pos2::new(plot.left() - 6.0, band.bottom()),
                            ),
                            0.0,
                            SELECT_TINT,
                        );
                    }

                    let from = self.son.as_ref().unwrap().sample_at(ci, self.t_start);
                    let to = self
                        .son
                        .as_ref()
                        .unwrap()
                        .sample_at(ci, self.t_end)
                        .max(from + 1);
                    let width = band.width().max(1.0) as usize;

                    if self.build_envelope(ci, from, to, width) {
                        if let Some(cache) = &self.caches[ci] {
                            let env = &cache.env;
                            let mut lo = f32::INFINITY;
                            let mut hi = f32::NEG_INFINITY;
                            for (a, b) in env.low.iter().zip(env.high.iter()) {
                                lo = lo.min(*a);
                                hi = hi.max(*b);
                            }
                            let gain = self.gain.get(ci).copied().unwrap_or(1.0);
                            let fixed = self.y_range.get(ci).copied().flatten();
                            let (lo, hi) = if let Some((a, b)) = fixed {
                                // An explicit Y range overrides both the fit and
                                // the gain, as Spike2's Y Axis Range does.
                                (a.min(b), a.max(b))
                            } else if lo.is_finite() && hi.is_finite() && hi > lo {
                                let mid0 = (lo + hi) * 0.5;
                                let half0 = (hi - lo) * 0.54 / gain;
                                (mid0 - half0, mid0 + half0)
                            } else {
                                (-1.0, 1.0)
                            };
                            let mid = band.center().y;
                            let half = band.height() * 0.5;

                            // horizontal grid at quarters, plus the zero line when visible
                            for k in 1..4 {
                                let gy = band.top() + band.height() * k as f32 / 4.0;
                                painter.line_segment(
                                    [Pos2::new(band.left(), gy), Pos2::new(band.right(), gy)],
                                    Stroke::new(1.0, GRID),
                                );
                            }
                            if lo < 0.0 && hi > 0.0 {
                                let zy = mid - ((0.0 - lo) / (hi - lo) - 0.5) * 2.0 * half;
                                painter.line_segment(
                                    [Pos2::new(band.left(), zy), Pos2::new(band.right(), zy)],
                                    Stroke::new(1.0, GRID_STRONG),
                                );
                            }

                            let stroke = Stroke::new(1.0, color);
                            for (col, (l, h)) in env.low.iter().zip(env.high.iter()).enumerate()
                            {
                                let x = band.left() + col as f32 + 0.5;
                                let y1 = mid - ((*h - lo) / (hi - lo) - 0.5) * 2.0 * half;
                                let y2 = mid - ((*l - lo) / (hi - lo) - 0.5) * 2.0 * half;
                                painter.line_segment(
                                    [Pos2::new(x, y1), Pos2::new(x, y2.max(y1 + 1.0))],
                                    stroke,
                                );
                            }

                            // numeric y axis at the left edge of the band. When zero is
                            // in range it replaces the nearest tick, so the axis reads
                            // 0 at the zero line the way Spike2's does.
                            let zero_y = if lo < 0.0 && hi > 0.0 {
                                Some(mid - ((0.0 - lo) / (hi - lo) - 0.5) * 2.0 * half)
                            } else {
                                None
                            };
                            let mut ticks: Vec<(f32, f32)> = (0..=4)
                                .map(|k| {
                                    (
                                        hi - (hi - lo) * k as f32 / 4.0,
                                        band.top() + band.height() * k as f32 / 4.0,
                                    )
                                })
                                .collect();
                            if let Some(zy) = zero_y {
                                let nearest = ticks
                                    .iter()
                                    .enumerate()
                                    .min_by(|a, b| {
                                        (a.1 .1 - zy)
                                            .abs()
                                            .partial_cmp(&(b.1 .1 - zy).abs())
                                            .unwrap_or(std::cmp::Ordering::Equal)
                                    })
                                    .map(|(i, _)| i);
                                if let Some(i) = nearest {
                                    ticks[i] = (0.0, zy);
                                }
                            }
                            for (v, yy) in ticks {
                                painter.line_segment(
                                    [
                                        Pos2::new(band.left(), yy),
                                        Pos2::new(band.left() + 4.0, yy),
                                    ],
                                    Stroke::new(1.0, AXIS),
                                );
                                // Keep the number clear of the tick and inside the band,
                                // otherwise the tick reads as a minus sign.
                                let ty = yy.clamp(band.top() + 6.0, band.bottom() - 6.0);
                                let label = if v.abs() < 1e-9 {
                                    "0".to_string()
                                } else {
                                    format_value(v)
                                };
                                painter.text(
                                    Pos2::new(band.left() + 9.0, ty),
                                    Align2::LEFT_CENTER,
                                    label,
                                    FontId::monospace(8.5),
                                    AXIS,
                                );
                            }

                            // vertical scale bar at the right edge, labelled with a round value
                            let nice = nice_number(f64::from((hi - lo) / 2.0));
                            let bar_h = (nice / f64::from(hi - lo)) as f32 * band.height();
                            let bx = band.right() - 14.0;
                            let by = mid;
                            painter.line_segment(
                                [Pos2::new(bx, by - bar_h), Pos2::new(bx, by)],
                                Stroke::new(1.5, AXIS),
                            );
                            for cap_y in [by - bar_h, by] {
                                painter.line_segment(
                                    [
                                        Pos2::new(bx - 4.0, cap_y),
                                        Pos2::new(bx + 4.0, cap_y),
                                    ],
                                    Stroke::new(1.0, AXIS),
                                );
                            }
                            painter.text(
                                Pos2::new(bx - 7.0, by - bar_h),
                                Align2::RIGHT_TOP,
                                format!("{} {}", format_value(nice as f32), meta[ci].units),
                                FontId::monospace(9.0),
                                AXIS,
                            );
                        }
                    }

                    // gutter: title, units, and the value under each cursor
                    painter.text(
                        Pos2::new(outer_plot.left() + 8.0, band.top() + 1.0),
                        Align2::LEFT_TOP,
                        &title,
                        FontId::proportional(if is_sel { 11.5 } else { 11.0 }),
                        color,
                    );
                    painter.text(
                        Pos2::new(outer_plot.left() + 8.0, band.top() + 14.0),
                        Align2::LEFT_TOP,
                        &meta[ci].units,
                        FontId::proportional(9.0),
                        AXIS,
                    );
                    if let Some(Some((a, b))) = self.readouts.get(ci) {
                        painter.text(
                            Pos2::new(outer_plot.left() + 8.0, band.top() + 26.0),
                            Align2::LEFT_TOP,
                            format!("1 {}", format_value(*a)),
                            FontId::monospace(9.0),
                            CURSOR_1,
                        );
                        painter.text(
                            Pos2::new(outer_plot.left() + 8.0, band.top() + 37.0),
                            Align2::LEFT_TOP,
                            format!("2 {}", format_value(*b)),
                            FontId::monospace(9.0),
                            CURSOR_2,
                        );
                    }
                } else {
                    // event / marker row
                    let band = Rect::from_min_max(
                        Pos2::new(plot.left(), y),
                        Pos2::new(plot.right(), (y + row_h - 2.0).min(plot.bottom())),
                    );
                    band_tops.push((ci, band.top(), band.bottom()));
                    y += row_h;

                    if is_sel {
                        painter.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(outer_plot.left(), band.top()),
                                Pos2::new(plot.left() - 6.0, band.bottom()),
                            ),
                            0.0,
                            SELECT_TINT,
                        );
                    }

                    painter.text(
                        Pos2::new(outer_plot.left() + 8.0, band.top() + 1.0),
                        Align2::LEFT_TOP,
                        &title,
                        FontId::proportional(10.0),
                        color,
                    );
                    painter.text(
                        Pos2::new(outer_plot.left() + 8.0, band.top() + 12.0),
                        Align2::LEFT_TOP,
                        format!("{} items", meta[ci].items),
                        FontId::proportional(9.0),
                        AXIS,
                    );

                    let base = band.bottom() - 3.0;
                    painter.line_segment(
                        [Pos2::new(band.left(), base), Pos2::new(band.right(), base)],
                        Stroke::new(1.0, GRID_STRONG),
                    );

                    if meta[ci].kind.is_wavemark() {
                        if let Ok(marks) = self.son.as_mut().unwrap().read_wavemarks(ci) {
                            for m in marks.iter() {
                                if m.time < self.t_start || m.time > self.t_end {
                                    continue;
                                }
                                let x = self.x_of(plot, m.time);
                                painter.line_segment(
                                    [Pos2::new(x, base - 9.0), Pos2::new(x, base)],
                                    Stroke::new(1.0, color),
                                );
                            }

                            // Mean snippet drawn at the right of the row.
                            let mut acc: Vec<f32> = Vec::new();
                            let mut used = 0usize;
                            for m in marks.iter().take(200) {
                                if m.samples.is_empty() {
                                    continue;
                                }
                                if acc.is_empty() {
                                    acc = vec![0.0; m.samples.len()];
                                }
                                if m.samples.len() != acc.len() {
                                    continue;
                                }
                                for (a, v) in acc.iter_mut().zip(m.samples.iter()) {
                                    *a += *v;
                                }
                                used += 1;
                            }
                            if used > 0 && acc.len() > 1 {
                                for a in acc.iter_mut() {
                                    *a /= used as f32;
                                }
                                let (lo, hi) = acc.iter().fold(
                                    (f32::INFINITY, f32::NEG_INFINITY),
                                    |(l, h), v| (l.min(*v), h.max(*v)),
                                );
                                if hi > lo {
                                    let w = 110.0f32;
                                    let x0 = band.right() - w - 6.0;
                                    let mid = band.center().y;
                                    let scale = band.height() * 0.36 / (hi - lo);
                                    let points: Vec<Pos2> = acc
                                        .iter()
                                        .enumerate()
                                        .map(|(i, v)| {
                                            let x = x0
                                                + w * i as f32 / (acc.len().max(2) - 1) as f32;
                                            Pos2::new(x, mid - (v - (lo + hi) * 0.5) * scale)
                                        })
                                        .collect();
                                    painter.add(egui::Shape::line(
                                        points,
                                        Stroke::new(1.0, color),
                                    ));
                                }
                            }
                        }
                    } else if let Ok(events) = self.son.as_mut().unwrap().read_events(ci) {
                        let mut labelled = 0;
                        for ev in &events {
                            if ev.time < self.t_start || ev.time > self.t_end {
                                continue;
                            }
                            let x = self.x_of(plot, ev.time);
                            painter.line_segment(
                                [Pos2::new(x, base - 9.0), Pos2::new(x, base)],
                                Stroke::new(1.0, color),
                            );
                            // Text marks carry a label; other markers carry an ASCII code.
                            if labelled < 12 {
                                if let Some(text) = &ev.text {
                                    painter.text(
                                        Pos2::new(x + 2.0, base - 10.0),
                                        Align2::LEFT_BOTTOM,
                                        text.chars().take(14).collect::<String>(),
                                        FontId::monospace(9.0),
                                        color,
                                    );
                                    labelled += 1;
                                } else if let Some(code) = ev.code {
                                    if (32..127).contains(&code) {
                                        painter.text(
                                            Pos2::new(x + 1.0, base - 10.0),
                                            Align2::LEFT_BOTTOM,
                                            char::from_u32(code as u32).unwrap_or('?').to_string(),
                                            FontId::monospace(9.0),
                                            color,
                                        );
                                        labelled += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── time axis ───────────────────────────────────────────────────
            painter.rect_filled(axis_rect, 0.0, Color32::from_rgb(246, 247, 249));
            painter.line_segment(
                [
                    Pos2::new(plot.left(), plot.bottom()),
                    Pos2::new(plot.right(), plot.bottom()),
                ],
                Stroke::new(1.0, AXIS),
            );

            let span = (self.t_end - self.t_start).max(1e-12);
            // View > X Axis Range… can pin the large-tick spacing; zero means
            // automatic, which is what Spike2's dialog shows as blank.
            let step = if self.x_tick > 0.0 {
                self.x_tick
            } else {
                nice_step(span, 10.0)
            };
            let first = (self.t_start / step).ceil() * step;
            let mut t = first;
            while t <= self.t_end {
                let x = self.x_of(plot, t);
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                    Stroke::new(1.0, GRID),
                );
                painter.line_segment(
                    [Pos2::new(x, plot.bottom()), Pos2::new(x, plot.bottom() + 5.0)],
                    Stroke::new(1.0, AXIS),
                );
                // Minor ticks at the subdivision interval, if asked for.
                if self.x_subs > 1 {
                    let minor = step / self.x_subs as f64;
                    for k in 1..self.x_subs {
                        let tm = t + minor * k as f64;
                        if tm > self.t_end {
                            break;
                        }
                        let xm = self.x_of(plot, tm);
                        painter.line_segment(
                            [Pos2::new(xm, plot.bottom()), Pos2::new(xm, plot.bottom() + 2.5)],
                            Stroke::new(1.0, AXIS),
                        );
                    }
                }
                let label = if t.abs() < step * 1e-6 {
                    "0".to_string()
                } else if step >= 1.0 {
                    format!("{t:.0}")
                } else {
                    format!("{t:.3}")
                };
                painter.text(
                    Pos2::new(x + 2.0, plot.bottom() + 7.0),
                    Align2::LEFT_TOP,
                    label,
                    FontId::monospace(9.0),
                    AXIS,
                );
                t += step;
            }
            painter.text(
                Pos2::new(plot.right() - 4.0, plot.bottom() + 7.0),
                Align2::RIGHT_TOP,
                "s",
                FontId::monospace(9.0),
                AXIS,
            );

            // ── cursors and selection ───────────────────────────────────────
            let pointer = response.interact_pointer_pos();
            if response.drag_started() {
                if let Some(p) = pointer {
                    let mut best = (f32::INFINITY, 0usize);
                    for (i, c) in self.cursors.iter().enumerate() {
                        let d = (self.x_of(plot, *c) - p.x).abs();
                        if d < best.0 {
                            best = (d, i);
                        }
                    }
                    if best.0 < 10.0 {
                        self.dragging = Some(best.1);
                    } else if plot.contains(p) {
                        if let Some((ci, _, _)) =
                            band_tops.iter().find(|(_, top, bottom)| p.y >= *top && p.y < *bottom)
                        {
                            self.selected = Some(*ci);
                        }
                    }
                }
            }
            if response.dragged() {
                if let (Some(i), Some(p)) = (self.dragging, pointer) {
                    let t = self.t_at(plot, p.x).clamp(0.0, duration);
                    self.cursors[i] = t;
                }
            }
            if response.drag_stopped() {
                self.dragging = None;
            }

            for (i, c) in self.cursors.iter().enumerate() {
                if *c < self.t_start || *c > self.t_end {
                    continue;
                }
                let x = self.x_of(plot, *c);
                let color = if i == 0 { CURSOR_1 } else { CURSOR_2 };
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, axis_rect.bottom())],
                    Stroke::new(1.0, color),
                );
                // The tag follows Cursor > Label Mode, and carries the selected
                // channel's value when Display Y Values is on.
                let mut text = self.cursor_label.tag(i, *c);
                if self.show_y {
                    if let Some(Some((a, b))) =
                        self.selected.and_then(|ci| self.readouts.get(ci))
                    {
                        let v = if i == 0 { *a } else { *b };
                        let t = text.take().unwrap_or_default();
                        text = Some(if t.is_empty() {
                            format!("{v:.3}")
                        } else {
                            format!("{t}  {v:.3}")
                        });
                    }
                }
                if let Some(text) = text {
                    let width = 4.0 + 5.2 * text.chars().count() as f32;
                    let tag = Rect::from_min_size(
                        Pos2::new(x + 2.0, plot.top() + 1.0),
                        Vec2::new(width, 13.0),
                    );
                    painter.rect_filled(tag, 2.0, color);
                    painter.text(
                        tag.center(),
                        Align2::CENTER_CENTER,
                        text,
                        FontId::monospace(9.0),
                        Color32::WHITE,
                    );
                }
            }

            // ── input ───────────────────────────────────────────────────────
            if response.hovered() {
                let scroll = ui.input(|i| i.raw_scroll_delta.y);
                if scroll.abs() > 0.1 {
                    let anchor = pointer
                        .map(|p| self.t_at(plot, p.x))
                        .unwrap_or(self.cursors[0]);
                    self.zoom(if scroll > 0.0 { 0.85 } else { 1.0 / 0.85 }, anchor);
                }
            }
            if response.dragged_by(egui::PointerButton::Middle) {
                let dx = response.drag_delta().x;
                let span = (self.t_end - self.t_start).max(1e-12);
                self.pan(-f64::from(dx) / f64::from(plot.width().max(1.0)) * span);
            }

            let (fit, up, down, reset, dc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::F),
                    i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                    i.key_pressed(egui::Key::Minus),
                    i.key_pressed(egui::Key::Num0),
                    i.key_pressed(egui::Key::D),
                )
            });
            if fit {
                self.fit();
            }
            if up {
                self.set_gain(1.25);
            }
            if down {
                self.set_gain(1.0 / 1.25);
            }
            if reset {
                if let Some(i) = self.selected {
                    if let Some(g) = self.gain.get_mut(i) {
                        *g = 1.0;
                    }
                }
            }
            if dc {
                if let Some(i) = self.selected {
                    let rate = self
                        .son
                        .as_ref()
                        .and_then(|s| s.channels.get(i).map(|c| c.sample_rate()))
                        .unwrap_or(0.0);
                    if rate > 0.0 {
                        let on = self.dc_window.get(i).copied().unwrap_or(0) > 1;
                        if let Some(w) = self.dc_window.get_mut(i) {
                            *w = if on {
                                0
                            } else {
                                ((rate * 0.2).round() as usize).max(3) | 1
                            };
                        }
                        self.caches[i] = None;
                        self.stats_key = None;
                    }
                }
            }

            painter.rect_stroke(
                outer_plot,
                0.0,
                Stroke::new(1.0, AXIS),
                egui::StrokeKind::Inside,
            );
        });

        // ── process chain ───────────────────────────────────────────────────
        // Spike2's Analysis > Process… is one template per channel and source;
        // this is the chain itself, in the order the data runs through it.
        if self.process_dialog {
            let mut open = self.process_dialog;
            egui::Window::new("Process")
                .collapsible(false)
                .resizable(true)
                .default_width(420.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let Some(ci) = self.selected else {
                        ui.label("Select a channel.");
                        return;
                    };
                    let name = self
                        .son
                        .as_ref()
                        .and_then(|s| s.channels.get(ci).map(|c| c.title.clone()))
                        .unwrap_or_default();
                    ui.label(egui::RichText::new(name).strong());
                    ui.separator();
                    let mut chain = self.processes.get(ci).cloned().unwrap_or_default();
                    if chain.is_empty() {
                        ui.label(egui::RichText::new("no processes").weak());
                    }
                    let mut changed = false;
                    let mut move_from: Option<(usize, isize)> = None;
                    for (k, step) in chain.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}.", k + 1));
                            ui.label(step.label());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("x").clicked() {
                                        move_from = Some((k, isize::MIN));
                                    }
                                    if ui.small_button("v").clicked() && k + 1 < chain.len() {
                                        move_from = Some((k, 1));
                                    }
                                    if ui.small_button("^").clicked() && k > 0 {
                                        move_from = Some((k, -1));
                                    }
                                },
                            );
                        });
                    }
                    if let Some((k, dir)) = move_from {
                        changed = true;
                        if dir == isize::MIN {
                            chain.remove(k);
                        } else {
                            let j = (k as isize + dir) as usize;
                            chain.swap(k, j);
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Add DC remove").clicked() {
                            let w = self.dc_window.get(ci).copied().unwrap_or(0);
                            chain.push(Process::DcRemove(if w > 1 { w } else { 101 }));
                            changed = true;
                        }
                        if ui.button("Add filter…").clicked() {
                            self.filter_dialog = true;
                        }
                        if ui.button("Clear").clicked() {
                            chain.clear();
                            changed = true;
                        }
                        if ui.button("Close").clicked() {
                            self.process_dialog = false;
                        }
                    });
                    if changed {
                        if let Some(slot) = self.processes.get_mut(ci) {
                            *slot = chain;
                        }
                        self.caches[ci] = None;
                        self.stats_key = None;
                        self.chain_tag = self.chain_tag.wrapping_add(1);
                    }
                });
            if !open {
                self.process_dialog = false;
            }
        }

        // ── X axis range ────────────────────────────────────────────────────
        // Spike2's dialog 1800 is 23 controls; these are the ones this view can
        // honour: left, right, width, and an explicit tick spacing. Its
        // logarithmic and auto-units options need an axis model this view does
        // not have, so they are shown disabled rather than faked.
        if self.xaxis_dialog {
            let mut open = self.xaxis_dialog;
            egui::Window::new("X Axis Range")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    let dur = self.son.as_ref().map(|s| s.duration()).unwrap_or(0.0);
                    egui::Grid::new("xrange").num_columns(2).show(ui, |ui| {
                        ui.label("Left (s)");
                        ui.add(egui::DragValue::new(&mut self.x_left).speed(0.01).range(0.0..=dur));
                        ui.end_row();
                        ui.label("Right (s)");
                        ui.add(egui::DragValue::new(&mut self.x_right).speed(0.01).range(0.0..=dur));
                        ui.end_row();
                        ui.label("Width (s)");
                        let mut w = self.x_right - self.x_left;
                        if ui
                            .add(egui::DragValue::new(&mut w).speed(0.01).range(0.001..=dur))
                            .changed()
                        {
                            self.x_right = (self.x_left + w).min(dur);
                        }
                        ui.end_row();
                        ui.label("Large tick (s)");
                        ui.add(
                            egui::DragValue::new(&mut self.x_tick)
                                .speed(0.001)
                                .range(0.0..=dur)
                                .custom_formatter(|v, _| {
                                    if v <= 0.0 {
                                        "auto".to_string()
                                    } else {
                                        format!("{v}")
                                    }
                                }),
                        );
                        ui.end_row();
                        ui.label("Subdivisions");
                        ui.add(egui::DragValue::new(&mut self.x_subs).range(1..=20));
                        ui.end_row();
                    });
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, "Logarithmic"));
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Show All").clicked() {
                            self.fit();
                            self.x_left = self.t_start;
                            self.x_right = self.t_end;
                        }
                        if ui.button("Draw").clicked() {
                            let (a, b) = (self.x_left.min(self.x_right), self.x_left.max(self.x_right));
                            if b - a > 1e-9 {
                                self.t_start = a.max(0.0);
                                self.t_end = b.min(dur);
                                self.clamp_view();
                                self.caches.iter_mut().for_each(|c| *c = None);
                            }
                        }
                        if ui.button("Close").clicked() {
                            self.xaxis_dialog = false;
                        }
                    });
                });
            if !open {
                self.xaxis_dialog = false;
            }
        }

        // ── Y axis range ────────────────────────────────────────────────────
        // Spike2 keeps this per channel; so does this, over `y_range`. An
        // explicit range replaces the automatic fit and the gain together.
        if self.yaxis_dialog {
            let mut open = self.yaxis_dialog;
            egui::Window::new("Y Axis Range")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    match self.selected {
                        None => {
                            ui.label("Select a channel.");
                        }
                        Some(ci) => {
                            let name = self
                                .son
                                .as_ref()
                                .and_then(|s| s.channels.get(ci).map(|c| c.title.clone()))
                                .unwrap_or_default();
                            let units = self
                                .son
                                .as_ref()
                                .and_then(|s| s.channels.get(ci).map(|c| c.units.clone()))
                                .unwrap_or_default();
                            ui.label(if units.is_empty() {
                                name
                            } else {
                                format!("{name} ({units})")
                            });
                            // With Automatic ticked the dialog shows the range
                            // the channel is actually drawn over, so turning the
                            // tick off starts from what is on screen.
                            if self.y_range.get(ci).copied().flatten().is_none() {
                                if let Some(s) = self.stats.get(ci).and_then(|s| *s) {
                                    self.y_min = s.min;
                                    self.y_max = s.max;
                                }
                            }
                            egui::Grid::new("yrange").num_columns(2).show(ui, |ui| {
                                ui.label("Bottom");
                                ui.add(egui::DragValue::new(&mut self.y_min).speed(0.01));
                                ui.end_row();
                                ui.label("Top");
                                ui.add(egui::DragValue::new(&mut self.y_max).speed(0.01));
                                ui.end_row();
                            });
                            let mut auto = self.y_range.get(ci).copied().flatten().is_none();
                            if ui.checkbox(&mut auto, "Automatic").changed() {
                                self.y_range[ci] = if auto {
                                    None
                                } else {
                                    Some((self.y_min, self.y_max))
                                };
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                if ui.button("Apply").clicked() {
                                    self.y_range[ci] = Some((
                                        self.y_min.min(self.y_max),
                                        self.y_min.max(self.y_max),
                                    ));
                                }
                                if ui.button("Close").clicked() {
                                    self.yaxis_dialog = false;
                                }
                            });
                        }
                    }
                });
            if !open {
                self.yaxis_dialog = false;
            }
        }

        // ── show/hide channel ───────────────────────────────────────────────
        if self.showhide_dialog {
            let mut open = self.showhide_dialog;
            egui::Window::new("Show/Hide Channel")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    let titles: Vec<String> = self
                        .son
                        .as_ref()
                        .map(|s| s.channels.iter().map(|c| c.title.clone()).collect())
                        .unwrap_or_default();
                    for (i, t) in titles.iter().enumerate() {
                        if let Some(v) = self.visible.get_mut(i) {
                            ui.checkbox(v, t);
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("All").clicked() {
                            self.visible.iter_mut().for_each(|v| *v = true);
                        }
                        if ui.button("None").clicked() {
                            self.visible.iter_mut().for_each(|v| *v = false);
                        }
                        if ui.button("Close").clicked() {
                            self.showhide_dialog = false;
                        }
                    });
                });
            if !open {
                self.showhide_dialog = false;
            }
        }

        // ── channel information ─────────────────────────────────────────────
        // Spike2's View > Channel Information… is the read-out its users reach
        // for when a scale or a unit is in dispute, so it shows the same fields
        // in the same order, plus the range that follows from the scale.
        if self.chan_info {
            let mut open = self.chan_info;
            egui::Window::new("Channel Information")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    let ch = self
                        .selected
                        .and_then(|i| self.son.as_ref().and_then(|s| s.channels.get(i)));
                    let Some(ch) = ch else {
                        ui.label("No channel selected.");
                        return;
                    };
                    egui::Grid::new("chan_info").num_columns(2).show(ui, |ui| {
                        let mut row = |k: &str, v: String| {
                            ui.label(k);
                            ui.label(v);
                            ui.end_row();
                        };
                        row("Title", ch.title.clone());
                        if !ch.comment.is_empty() {
                            row("Comment", ch.comment.clone());
                        }
                        row("Kind", format!("{:?}", ch.kind));
                        row("Units", ch.units.clone());
                        let r = ch.sample_rate();
                        row(
                            "Rate",
                            if (r - r.round()).abs() < 1e-9 {
                                format!("{} Hz", r.round() as i64)
                            } else {
                                format!("{r:.6} Hz")
                            },
                        );
                        row("Scale", format!("{}", ch.scale));
                        row("Offset", format!("{}", ch.offset));
                        // The ADC spans +-5 V over +-32768 counts, so the scale is
                        // in units per volt and the range is scale * 5.
                        if matches!(ch.kind, smr::Kind::Adc) {
                            row("Range", format!("±{} {}", ch.scale * 5.0, ch.units.trim()));
                        }
                        row("Items", format!("{}", ch.items));
                    });
                });
            if !open {
                self.chan_info = false;
            }
        }

        // ── filter dialog ───────────────────────────────────────────────────
        if self.filter_dialog {
            let mut open = self.filter_dialog;
            egui::Window::new("Digital filters")
                .collapsible(false)
                .resizable(false)
                .default_width(360.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let rate = self
                        .selected
                        .and_then(|i| self.son.as_ref().and_then(|s| s.channels.get(i)))
                        .map(|c| c.sample_rate())
                        .unwrap_or(0.0);

                    ui.horizontal(|ui| {
                        ui.label("Type");
                        ui.selectable_value(&mut self.f_kind, smr::FilterKind::LowPass, "Low pass");
                        ui.selectable_value(&mut self.f_kind, smr::FilterKind::HighPass, "High pass");
                        ui.selectable_value(&mut self.f_kind, smr::FilterKind::BandPass, "Band pass");
                        ui.selectable_value(&mut self.f_kind, smr::FilterKind::BandStop, "Band stop");
                    });
                    ui.horizontal(|ui| {
                        let band = matches!(
                            self.f_kind,
                            smr::FilterKind::BandPass | smr::FilterKind::BandStop
                        );
                        ui.label(if band { "Centre (Hz)" } else { "Cutoff (Hz)" });
                        ui.add(egui::DragValue::new(&mut self.f_cutoff).speed(1.0).range(0.01..=1.0e6));
                        ui.label(if rate > 0.0 {
                            format!("Nyquist {:.0} Hz", rate / 2.0)
                        } else {
                            String::new()
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Width (Hz)");
                        // FIRQuick refuses a transition wider than a tenth of the rate.
                        let wmax = if rate > 0.0 { rate * 0.1 } else { 1.0e6 };
                        let w = self.f_width.clamp(0.01, wmax);
                        self.f_width = w;
                        ui.add(egui::DragValue::new(&mut self.f_width).speed(1.0).range(0.01..=wmax));
                        ui.label("Attenuation (dB)");
                        ui.add(egui::DragValue::new(&mut self.f_atten).speed(1.0).range(10.0..=160.0));
                    });
                    if !self.f_iir && rate > 0.0 {
                        let w = (self.f_width / rate).clamp(1e-4, 0.1);
                        let t = match self.f_kind {
                            smr::FilterKind::LowPass => smr::DesignType::LowPass,
                            smr::FilterKind::HighPass => smr::DesignType::HighPass,
                            smr::FilterKind::BandPass => smr::DesignType::BandPass,
                            smr::FilterKind::BandStop => smr::DesignType::BandStop,
                        };
                        let n = smr::quick_tap_count(t, self.f_cutoff / rate, w, self.f_atten);
                        ui.label(format!("{n} coefficients"));
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.f_iir, "IIR (two-pole Butterworth)");
                    });

                    if rate > 0.0 && self.f_cutoff >= rate / 2.0 {
                        ui.colored_label(
                            Color32::from_rgb(170, 90, 0),
                            "Cutoff is at or above Nyquist for this channel.",
                        );
                    }

                    // ── band table ──────────────────────────────────────
                    // The same four fields CFIR reads: low, high, desired
                    // amplitude and weight. Spike2's dialog calls these edges,
                    // response and -3 dB points; these are the designer's own.
                    egui::CollapsingHeader::new("Bands (CFIR band table)")
                        .default_open(!self.f_bands.is_empty())
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("taps");
                                let mut t = self.f_taps as i32;
                                if ui
                                    .add(egui::DragValue::new(&mut t).range(0..=2047))
                                    .changed()
                                {
                                    self.f_taps = t.max(0) as usize;
                                }
                                ui.label(if self.f_taps == 0 { "(auto)" } else { "" });
                                ui.separator();
                                if ui.button("From parameters").clicked() {
                                    let t = match self.f_kind {
                                        smr::FilterKind::LowPass => smr::DesignType::LowPass,
                                        smr::FilterKind::HighPass => smr::DesignType::HighPass,
                                        smr::FilterKind::BandPass => smr::DesignType::BandPass,
                                        smr::FilterKind::BandStop => smr::DesignType::BandStop,
                                    };
                                    let w = (self.f_width / rate).clamp(1e-4, 0.1);
                                    self.f_bands =
                                        smr::quick_bands(t, self.f_cutoff / rate, w);
                                }
                                if ui.button("Clear").clicked() {
                                    self.f_bands.clear();
                                }
                            });
                            if self.f_bands.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "empty: the design comes from the parameters above",
                                    )
                                    .weak()
                                    .small(),
                                );
                            } else {
                                egui::Grid::new("bands")
                                    .num_columns(5)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        // Spike2's dialog edits a gain per edge
                                        // and derives the weight; the derived
                                        // column is shown so the numbers are
                                        // never hidden.
                                        for h in ["low", "high", "gain", "dB", "weight"] {
                                            ui.label(egui::RichText::new(h).small().weak());
                                        }
                                        ui.end_row();
                                        let mut drop = None;
                                        for (i, b) in self.f_bands.iter_mut().enumerate() {
                                            ui.add(
                                                egui::DragValue::new(&mut b.low)
                                                    .speed(0.001)
                                                    .range(0.0..=0.5),
                                            );
                                            ui.add(
                                                egui::DragValue::new(&mut b.high)
                                                    .speed(0.001)
                                                    .range(0.0..=0.5),
                                            );
                                            ui.add(
                                                egui::DragValue::new(&mut b.fx)
                                                    .speed(0.01)
                                                    .range(0.0..=10.0),
                                            );
                                            // -3 dB is where a pass band is
                                            // usually wanted, so show it.
                                            let db = if b.fx > 0.0 {
                                                20.0 * b.fx.log10()
                                            } else {
                                                f64::NEG_INFINITY
                                            };
                                            ui.label(if db.is_finite() {
                                                format!("{db:.1}")
                                            } else {
                                                "-inf".to_string()
                                            });
                                            b.wt = smr::edge_weight(b.fx);
                                            ui.label(format!("{:.2}", b.wt));
                                            if ui.small_button("x").clicked() {
                                                drop = Some(i);
                                            }
                                            ui.end_row();
                                        }
                                        if let Some(i) = drop {
                                            self.f_bands.remove(i);
                                        }
                                    });
                                if ui.button("+ band").clicked() {
                                    let lo = self
                                        .f_bands
                                        .last()
                                        .map(|b| b.high)
                                        .unwrap_or(0.0);
                                    self.f_bands
                                        .push(smr::Band::new(lo, (lo + 0.05).min(0.5), 0.0, 1.0));
                                }
                            }
                        });

                    ui.checkbox(&mut self.f_log, "Log frequency");
                    // ── response plot ──────────────────────────────────
                    // Spike2's dialog is a band editor around a response plot;
                    // this is the plot half, drawn from CFIR::FreqR.
                    if !self.f_iir && rate > 0.0 {
                        let key = {
                            let t = self.f_kind as u64;
                            let q = |v: f64| (v * 100.0).round() as u64;
                            let mut k = t
                                ^ (q(self.f_cutoff) << 8)
                                ^ (q(self.f_width) << 24)
                                ^ (q(self.f_atten) << 40)
                                ^ (q(rate) << 52);
                            // Every band field participates, or an edit to one
                            // would not trigger a redraw.
                            for b in &self.f_bands {
                                k = k
                                    .rotate_left(5)
                                    ^ q(b.low)
                                    ^ q(b.high).rotate_left(13)
                                    ^ q(b.fx).rotate_left(23)
                                    ^ q(b.wt).rotate_left(31);
                            }
                            k ^ (self.f_taps as u64) << 3
                        };
                        let stale = self.f_preview.as_ref().map(|(k, _)| *k) != Some(key);
                        if stale {
                            let t = match self.f_kind {
                                smr::FilterKind::LowPass => smr::DesignType::LowPass,
                                smr::FilterKind::HighPass => smr::DesignType::HighPass,
                                smr::FilterKind::BandPass => smr::DesignType::BandPass,
                                smr::FilterKind::BandStop => smr::DesignType::BandStop,
                            };
                            let w = (self.f_width / rate).clamp(1e-4, 0.1);
                            // An explicit band table goes straight to CFIR's
                            // own entry point, which is the same call the Wine
                            // harness makes against Filters.dll.
                            self.f_err = None;
                            self.f_preview = if self.f_bands.is_empty() {
                                smr::fir_quick(t, self.f_cutoff / rate, w, self.f_atten)
                                    .ok()
                                    .map(|d| (key, d))
                            } else {
                                let n = if self.f_taps > 0 {
                                    self.f_taps
                                } else {
                                    smr::quick_tap_count(t, self.f_cutoff / rate, w, self.f_atten)
                                };
                                match smr::design(n, t, &self.f_bands, 16, true) {
                                    Ok(d) => Some((key, d)),
                                    Err(e) => {
                                        self.f_err = Some(format!("{e:?}"));
                                        None
                                    }
                                }
                            };
                        }
                        if self.f_preview.as_ref().map(|(k, _)| *k) != Some(key) {
                            // No design: say why, rather than draw an empty box.
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(340.0, 110.0),
                                egui::Sense::hover(),
                            );
                            let p = ui.painter_at(rect);
                            p.rect_filled(rect, 2.0, Color32::WHITE);
                            p.rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, AXIS),
                                egui::StrokeKind::Inside,
                            );
                            let msg = self
                                .f_err
                                .clone()
                                .unwrap_or_else(|| "no design".to_string());
                            p.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                format!("the exchange did not converge: {msg}"),
                                FontId::proportional(11.0),
                                Color32::from_gray(120),
                            );
                        }
                        if let Some((_, d)) = self.f_preview.as_ref() {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(340.0, 110.0),
                                egui::Sense::hover(),
                            );
                            let p = ui.painter_at(rect);
                            p.rect_filled(rect, 2.0, Color32::WHITE);
                            p.rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, AXIS),
                                egui::StrokeKind::Inside,
                            );
                            // +10 dB at the top, -80 at the bottom, 0..Nyquist across.
                            // t runs 0 at +10 dB to 1 at -80, and y grows downwards.
                            let to_y = |db: f64| {
                                let t = ((10.0 - db.clamp(-80.0, 10.0)) / 90.0) as f32;
                                rect.top() + t * rect.height()
                            };
                            let zero = to_y(0.0);
                            p.line_segment(
                                [egui::pos2(rect.left(), zero), egui::pos2(rect.right(), zero)],
                                egui::Stroke::new(1.0, Color32::from_gray(200)),
                            );
                            // Frequency across the plot, linear or logarithmic.
                            // Spike2's dialog has the same choice, as "Log
                            // frequency"; it maps through CChangeFilter's
                            // FreqToPixel.
                            let x_of = |f: f64| -> f32 {
                                rect.left()
                                    + rect.width() * (freq_to_frac(f, self.f_log) as f32).clamp(0.0, 1.0)
                            };
                            let f_of = |x: f32| -> f64 {
                                let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
                                frac_to_freq(t, self.f_log)
                            };
                            let mut pts = Vec::with_capacity(220);
                            for i in 0..=220 {
                                let f = frac_to_freq(i as f64 / 220.0, self.f_log);
                                pts.push(egui::pos2(x_of(f), to_y(d.freq_db(f))));
                            }
                            p.add(egui::Shape::line(
                                pts,
                                egui::Stroke::new(1.2, Color32::from_rgb(0, 90, 180)),
                            ));

                            // ── band display ─────────────────────────────
                            // Each internal edge is a handle: dragging it moves
                            // the boundary between two bands, which is what
                            // CChangeFilter does when a corner is picked up.
                            if !self.f_bands.is_empty() {
                                let mut moved: Option<(usize, f64)> = None;
                                for k in 1..self.f_bands.len() {
                                    let f = self.f_bands[k].low;
                                    let x = x_of(f);
                                    let grip = egui::Rect::from_center_size(
                                        egui::pos2(x, rect.center().y),
                                        egui::vec2(7.0, rect.height()),
                                    );
                                    let resp = ui.interact(
                                        grip,
                                        egui::Id::new(("bandedge", k)),
                                        egui::Sense::drag(),
                                    );
                                    let col = if resp.hovered() || resp.dragged() {
                                        Color32::from_rgb(200, 120, 0)
                                    } else {
                                        Color32::from_gray(90)
                                    };
                                    p.line_segment(
                                        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                        egui::Stroke::new(1.4, col),
                                    );
                                    // A grip mark at the bottom so the handle
                                    // reads as draggable rather than as grid.
                                    p.rect_filled(
                                        egui::Rect::from_center_size(
                                            egui::pos2(x, rect.bottom() - 3.0),
                                            egui::vec2(7.0, 6.0),
                                        ),
                                        1.0,
                                        col,
                                    );
                                    if resp.dragged() {
                                        if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                                            moved = Some((k, f_of(ptr.x)));
                                        }
                                    }
                                }
                                // Gain handles: a plateau per band at its own
                                // level, dragged vertically. Spike2 splits the
                                // same two gestures - CChangeGap moves a gap,
                                // the basic moves change a level.
                                let mut gmove: Option<(usize, f64)> = None;
                                for (k, b) in self.f_bands.iter().enumerate() {
                                    let x0 = x_of(b.low.max(0.0));
                                    let x1 = x_of(b.high);
                                    let db = if b.fx > 0.0 { 20.0 * b.fx.log10() } else { -80.0 };
                                    let y = to_y(db);
                                    p.line_segment(
                                        [egui::pos2(x0, y), egui::pos2(x1, y)],
                                        egui::Stroke::new(1.0, Color32::from_rgb(120, 160, 200)),
                                    );
                                    let grip = egui::Rect::from_center_size(
                                        egui::pos2((x0 + x1) * 0.5, y),
                                        egui::vec2(9.0, 9.0),
                                    );
                                    let resp = ui.interact(
                                        grip,
                                        egui::Id::new(("bandgain", k)),
                                        egui::Sense::drag(),
                                    );
                                    p.rect_filled(
                                        grip,
                                        2.0,
                                        if resp.hovered() || resp.dragged() {
                                            Color32::from_rgb(200, 120, 0)
                                        } else {
                                            Color32::from_rgb(120, 160, 200)
                                        },
                                    );
                                    if resp.dragged() {
                                        if let Some(ptr) = ui.ctx().pointer_interact_pos() {
                                            let t = ((ptr.y - rect.top()) / rect.height())
                                                .clamp(0.0, 1.0) as f64;
                                            let db = 10.0 - t * 90.0;
                                            gmove = Some((k, 10f64.powf(db / 20.0)));
                                        }
                                    }
                                }
                                if let Some((k, g)) = gmove {
                                    self.f_bands[k].fx = g.clamp(1e-4, 1.5);
                                }
                                if let Some((k, f)) = moved {
                                    // Keep both neighbours ordered and non-empty.
                                    let lo = self.f_bands[k - 1].low + 1e-4;
                                    let hi = self.f_bands[k].high - 1e-4;
                                    let f = f.clamp(lo, hi);
                                    self.f_bands[k].low = f;
                                    self.f_bands[k - 1].high = f;
                                }
                            }
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        // Two kinds of filter reach a channel: a FilterSpec,
                        // resolved at draw time, and a band table's finished
                        // coefficients, which are applied directly.
                        let bands_set = !self.f_bands.is_empty();
                        if ui.button("Apply to selected channel").clicked() {
                            let step = if bands_set {
                                self.f_preview
                                    .as_ref()
                                    .map(|(_, d)| Process::Coeffs(d.coeffs.clone()))
                            } else {
                                Some(Process::Fir(smr::FilterSpec {
                                    kind: self.f_kind,
                                    cutoff: self.f_cutoff,
                                    width: self.f_width,
                                    atten: self.f_atten,
                                    iir: self.f_iir,
                                }))
                            };
                            if let (Some(i), Some(step)) = (self.selected, step) {
                                if let Some(chain) = self.processes.get_mut(i) {
                                    // Applied steps accumulate, which is what
                                    // Analysis > Process… is for; the dialog
                                    // shows the chain in the channel list.
                                    chain.push(step);
                                }
                                self.caches[i] = None;
                                self.stats_key = None;
                                self.chain_tag = self.chain_tag.wrapping_add(1);
                                self.status = "added a step to the process chain".to_string();
                            }
                        }
                        if ui.button("Remove last").clicked() {
                            if let Some(i) = self.selected {
                                if let Some(chain) = self.processes.get_mut(i) {
                                    chain.pop();
                                }
                                self.caches[i] = None;
                                self.stats_key = None;
                                self.chain_tag = self.chain_tag.wrapping_add(1);
                            }
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "FIR: Spike2's Parks-McClellan exchange; the count follows\n                        from the attenuation and the width.\n                        IIR: a two-pole Butterworth section, not Spike2's.",
                        )
                        .small()
                        .weak(),
                    );
                });
            self.filter_dialog = open;
        }

        // ── average dialog ──────────────────────────────────────────────────
        if self.avg_dialog {
            let events: Vec<(usize, String)> = self
                .son
                .as_ref()
                .map(|son| {
                    son.channels
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.kind.is_event())
                        .map(|(i, c)| {
                            (
                                i,
                                if c.title.is_empty() {
                                    format!("Channel {}", i + 1)
                                } else {
                                    c.title.clone()
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut open = self.avg_dialog;
            egui::Window::new("Average around events")
                .collapsible(false)
                .resizable(true)
                .default_width(480.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    if events.is_empty() {
                        ui.label("This file has no event channels to average around.");
                        return;
                    }

                    let current = events
                        .iter()
                        .find(|(i, _)| *i == self.avg_event_ch)
                        .map(|(_, t)| t.clone())
                        .unwrap_or_else(|| events[0].1.clone());
                    egui::ComboBox::from_label("Event channel")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, title) in &events {
                                ui.selectable_value(&mut self.avg_event_ch, *i, title);
                            }
                        });

                    ui.horizontal(|ui| {
                        ui.label("before (s)");
                        ui.add(egui::DragValue::new(&mut self.avg_pre).speed(0.005).range(0.0..=10.0));
                        ui.label("after (s)");
                        ui.add(egui::DragValue::new(&mut self.avg_post).speed(0.005).range(0.001..=10.0));
                        ui.label("bins");
                        ui.add(egui::DragValue::new(&mut self.avg_bins).speed(1).range(2..=2000));
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Compute").clicked() {
                            self.compute_average();
                        }
                        if let Some(i) = self.selected {
                            if let Some(son) = &self.son {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "waveform: {}",
                                        if son.channels[i].title.is_empty() {
                                            format!("channel {}", i + 1)
                                        } else {
                                            son.channels[i].title.clone()
                                        }
                                    ))
                                    .small()
                                    .weak(),
                                );
                            }
                        }
                    });

                    if let Some(avg) = &self.avg {
                        ui.separator();
                        ui.label(&self.avg_label);

                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 190.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 0.0, PAPER);
                        painter.rect_stroke(
                            rect,
                            0.0,
                            Stroke::new(1.0, AXIS),
                            egui::StrokeKind::Inside,
                        );

                        let vals: Vec<f64> = avg.bins.iter().copied().filter(|v| v.is_finite()).collect();
                        if vals.len() > 1 {
                            let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
                            let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                            let span = (hi - lo).max(1e-12);
                            let n = avg.bins.len();

                            let points: Vec<Pos2> = avg
                                .bins
                                .iter()
                                .enumerate()
                                .filter(|(_, v)| v.is_finite())
                                .map(|(i, v)| {
                                    let x = rect.left()
                                        + rect.width() * i as f32 / (n.max(2) - 1) as f32;
                                    let y = rect.bottom()
                                        - rect.height() * ((*v - lo) / span) as f32;
                                    Pos2::new(x, y)
                                })
                                .collect();
                            painter.add(egui::Shape::line(points, Stroke::new(1.0, CURSOR_1)));

                            // event time marker at t = 0
                            let zero = (0.0 - avg.start) / avg.bin_width;
                            if zero >= 0.0 && zero <= n as f64 {
                                let x = rect.left()
                                    + rect.width() * zero as f32 / n as f32;
                                painter.line_segment(
                                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                                    Stroke::new(1.0, GRID_STRONG),
                                );
                            }

                            painter.text(
                                Pos2::new(rect.left() + 4.0, rect.top() + 2.0),
                                Align2::LEFT_TOP,
                                format!("{:.4} … {:.4}", lo, hi),
                                FontId::monospace(9.0),
                                AXIS,
                            );
                            painter.text(
                                Pos2::new(rect.left() + 4.0, rect.bottom() - 12.0),
                                Align2::LEFT_TOP,
                                format!("{:.3} s", avg.start),
                                FontId::monospace(9.0),
                                AXIS,
                            );
                            painter.text(
                                Pos2::new(rect.right() - 4.0, rect.bottom() - 12.0),
                                Align2::RIGHT_TOP,
                                format!("{:.3} s", avg.start + n as f64 * avg.bin_width),
                                FontId::monospace(9.0),
                                AXIS,
                            );
                        }
                    }
                });
            self.avg_dialog = open;
        }

        // ── open dialog ─────────────────────────────────────────────────────
        if self.open_dialog {
            let mut open = self.open_dialog;
            egui::Window::new("Open by path")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Path to a Spike2 data file (.smr or .smrx):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .desired_width(520.0)
                            .hint_text("/path/to/file.smrx"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            let path = PathBuf::from(self.path_input.trim());
                            if path.is_file() {
                                self.load(&path);
                                self.open_dialog = false;
                            } else {
                                self.error = Some(format!("not a file: {}", path.display()));
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.open_dialog = false;
                        }
                        if ui.button("Fit").clicked() {
                            self.fit();
                        }
                    });
                    if let Some(e) = &self.error {
                        ui.colored_label(Color32::from_rgb(180, 0, 0), e);
                    }
                    ui.separator();
                    ui.label("Files here:");
                    if let Ok(dir) = std::env::current_dir() {
                        if let Ok(entries) = std::fs::read_dir(&dir) {
                            for entry in entries.flatten().take(40) {
                                let p = entry.path();
                                if p.extension().map(|e| e == "smr" || e == "smrx").unwrap_or(false) {
                                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                                    if ui.button(name).clicked() {
                                        self.path_input = p.display().to_string();
                                    }
                                }
                            }
                        }
                    }
                });
            self.open_dialog = open;
        }
    }
}

/// Spike2's look: light grey chrome, white data area, black text, classic 3D borders.
fn spike2_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::light();
    v.panel_fill = Color32::from_rgb(240, 240, 240);
    v.window_fill = Color32::from_rgb(240, 240, 240);
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_rgb(246, 246, 246);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(150, 150, 150));
    v.widgets.noninteractive.bg_fill = Color32::from_rgb(240, 240, 240);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(170, 170, 170));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(20, 20, 20));
    v.widgets.inactive.bg_fill = Color32::from_rgb(232, 232, 232);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(20, 20, 20));
    v.widgets.hovered.bg_fill = Color32::from_rgb(219, 233, 246);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(10, 10, 10));
    v.widgets.active.bg_fill = Color32::from_rgb(204, 228, 247);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0, 0, 0));
    v.selection.bg_fill = Color32::from_rgb(0, 120, 215);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 3.0);
    style.spacing.button_padding = egui::vec2(6.0, 2.0);
    ctx.set_style(style);
}

#[cfg(test)]
mod axis_tests {
    use super::{frac_to_freq, freq_to_frac};

    #[test]
    fn linear_and_log_mappings_are_inverses() {
        for log in [false, true] {
            for i in 1..50 {
                let f = 0.5 * i as f64 / 50.0;
                let back = frac_to_freq(freq_to_frac(f, log), log);
                assert!((back - f).abs() < 1e-9, "log={log} f={f} back={back}");
            }
        }
    }

    #[test]
    fn the_ends_are_the_ends() {
        // Nyquist sits at the right edge in both mappings.
        assert!((freq_to_frac(0.5, false) - 1.0).abs() < 1e-12);
        assert!((freq_to_frac(0.5, true) - 1.0).abs() < 1e-12);
        assert!(freq_to_frac(0.0, false).abs() < 1e-12);
        // Log clamps at its 1 mHz floor rather than going to -infinity.
        assert!(freq_to_frac(0.0, true).abs() < 1e-12);
        // Monotonic, which is what dragging depends on.
        let mut prev = -1.0;
        for i in 0..50 {
            let t = freq_to_frac(0.5 * (i + 1) as f64 / 50.0, true);
            assert!(t > prev);
            prev = t;
        }
    }
}

fn main() -> eframe::Result {
    let mut path: Option<PathBuf> = None;
    let mut dc_on_start = false;
    let mut avg_on_start = false;
    let mut record_on_start = false;
    let mut export_on_start = false;
    let mut lowpass_on_start: Option<f64> = None;
    let mut filter_dialog_on_start = false;
    let mut chan_info_on_start = false;
    let mut yaxis_on_start = false;
    let mut showhide_on_start = false;
    let mut yrange_on_start: Option<(f32, f32)> = None;
    let mut copy_dump = false;
    let mut bands_notch = false;
    let mut bands_kill = false;
    let mut bands_apply = false;
    let mut process_on_start = false;
    let mut process_chain = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            // Start with DC remove applied to the first waveform channel.
            "--dc" => dc_on_start = true,
            // Open the peri-stimulus average for the first event channel.
            "--average" => avg_on_start = true,
            // Live acquisition from the synthetic source (no hardware attached).
            "--record" => record_on_start = true,
            // Open the digital-filter dialog at startup.
            "--filters" => filter_dialog_on_start = true,
            // Open View > Channel Information… at startup.
            "--chan-info" => chan_info_on_start = true,
            "--yaxis" => yaxis_on_start = true,
            "--showhide" => showhide_on_start = true,
            // --copy-dump prints Edit > Copy's text and exits, which is also how
            // it can be used from a script.
            "--copy-dump" => copy_dump = true,
            // --bands=notch seeds a stop band in the middle, for exercising the
            // band table without clicking.
            "--bands=notch" => bands_notch = true,
            // A table that removes everything below 20 Hz, which the demo file's
            // 12.5 Hz sine wave lives in: applying it should flatten the trace.
            "--bands=kill" => bands_kill = true,
            "--bands-apply" => bands_apply = true,
            "--process" => process_on_start = true,
            // Two steps in the chain, to show that they compose in order.
            "--process-chain" => process_chain = true,
            // --yrange=<lo>,<hi> pins the first channel's Y range at startup.
            other if other.starts_with("--yrange=") => {
                let v = &other["--yrange=".len()..];
                if let Some((a, b)) = v.split_once(',') {
                    if let (Ok(a), Ok(b)) = (a.parse::<f32>(), b.parse::<f32>()) {
                        yrange_on_start = Some((a, b));
                    }
                }
            }
            // --lowpass=<hz> applies a FIR low pass to the selected channel at startup.
            other if other.starts_with("--lowpass=") => {
                if let Ok(hz) = other["--lowpass=".len()..].parse::<f64>() {
                    lowpass_on_start = Some(hz);
                }
            }
            // Write the visible window of the selected channel to CSV, then keep running.
            "--export" => export_on_start = true,
            // An unknown flag is not a file name; ignoring it silently used to
            // turn "--typo" into a failed open instead of a usage note.
            other if other.starts_with("--") => {
                eprintln!("sharar: unknown option {other}");
            }
            _ => path = Some(PathBuf::from(arg)),
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([900.0, 500.0])
            .with_title("Sharar"),
        ..Default::default()
    };

    eframe::run_native(
        "Sharar",
        options,
        Box::new(move |cc| {
            spike2_style(&cc.egui_ctx);
            let mut app = App::new(path.clone());
            if record_on_start {
                app.live = Some(acq::Live::new(Box::new(acq::SyntheticSource::new(1000.0))));
            }
            if dc_on_start {
                app.enable_dc();
            }
            if avg_on_start {
                app.enable_average();
            }
            if filter_dialog_on_start {
                app.open_filter_dialog();
            }
            if chan_info_on_start {
                app.chan_info = true;
            }
            if yaxis_on_start {
                app.selected = Some(0);
                app.yaxis_dialog = true;
            }
            if showhide_on_start {
                app.showhide_dialog = true;
            }
            if copy_dump {
                println!("{}", app.copy_measurements());
                std::process::exit(0);
            }
            if process_chain {
                let rate = 100.0;
                if let Some(chain) = app.processes.get_mut(0) {
                    chain.push(Process::DcRemove(101));
                    chain.push(Process::Fir(smr::FilterSpec {
                        kind: smr::FilterKind::LowPass,
                        cutoff: 20.0,
                        width: 5.0,
                        atten: 50.0,
                        iir: false,
                    }));
                }
                app.selected = Some(0);
                app.chain_tag = app.chain_tag.wrapping_add(1);
                app.status = format!("chain: two steps at {rate} Hz");
            }
            if process_on_start {
                app.selected = Some(0);
                app.process_dialog = true;
            }
            if bands_kill {
                app.f_bands = vec![
                    smr::Band::new(0.0, 0.20, 0.0, 10.0),
                    smr::Band::new(0.25, 0.5, 1.0, 1.0),
                ];
                app.filter_dialog = true;
            }
            if bands_apply && !app.f_bands.is_empty() {
                let n = app.f_taps.max(103);
                if let Ok(d) = smr::design(n, smr::DesignType::LowPass, &app.f_bands, 16, true) {
                    if let Some(chain) = app.processes.get_mut(0) {
                        chain.push(Process::Coeffs(d.coeffs.clone()));
                    }
                    app.status = format!("applied band table design ({} taps)", d.coeffs.len());
                }
            }
            if bands_notch {
                app.f_bands = vec![
                    smr::Band::new(0.0, 0.20, 1.0, 1.0),
                    smr::Band::new(0.22, 0.30, 0.0, 10.0),
                    smr::Band::new(0.32, 0.5, 1.0, 1.0),
                ];
                app.filter_dialog = true;
            }
            if let Some((a, b)) = yrange_on_start {
                if !app.y_range.is_empty() {
                    app.y_range[0] = Some((a, b));
                }
            }
            if let Some(hz) = lowpass_on_start {
                app.apply_filter_to_selected(smr::FilterSpec {
                    kind: smr::FilterKind::LowPass,
                    cutoff: hz,
                    // The scale of Spike2's own default: a transition a fifth of
                    // the corner, at its 50 dB default attenuation.
                    width: (hz * 0.2).max(0.01),
                    atten: 50.0,
                    iir: false,
                });
            }
            if export_on_start {
                app.export_csv();
                eprintln!("export: {}", app.status_message());
            }
            Ok(Box::new(app))
        }),
    )
}
