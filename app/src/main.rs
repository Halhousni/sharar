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

/// Spike2's default channel colours, in order.
const CHANNEL_COLORS: [Color32; 8] = [
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
const SELECT_TINT: Color32 = Color32::from_rgb(232, 240, 252);

/// Decimated envelope of one channel over the visible window.
struct Envelope {
    low: Vec<f32>,
    high: Vec<f32>,
}

struct Cache {
    from_sample: u64,
    to_sample: u64,
    width: usize,
    env: Envelope,
}

/// Channel facts the view needs, cloned so drawing can take `&mut self` for caching.
struct ChanMeta {
    title: String,
    units: String,
    kind: Kind,
    items: u64,
}

struct App {
    son: Option<SonFile>,
    visible: Vec<bool>,
    /// Vertical gain per channel, multiplied onto the automatic fit.
    gain: Vec<f32>,
    selected: Option<usize>,
    t_start: f64,
    t_end: f64,
    cursors: [f64; 2],
    dragging: Option<usize>,
    caches: Vec<Option<Cache>>,
    /// Value at cursor 1 and cursor 2, per channel.
    readouts: Vec<Option<(f32, f32)>>,
    status: String,
    open_dialog: bool,
    path_input: String,
    error: Option<String>,
}

impl App {
    fn new(path: Option<PathBuf>) -> Self {
        let mut app = Self {
            son: None,
            visible: Vec::new(),
            gain: Vec::new(),
            selected: None,
            t_start: 0.0,
            t_end: 1.0,
            cursors: [0.0, 0.1],
            dragging: None,
            caches: Vec::new(),
            readouts: Vec::new(),
            status: "No file open".to_string(),
            open_dialog: false,
            path_input: String::new(),
            error: None,
        };
        if let Some(p) = path {
            app.load(&p);
        }
        app
    }

    fn load(&mut self, path: &Path) {
        match SonFile::open(path) {
            Ok(f) => {
                let duration = f.duration().max(0.001);
                let n = f.channels.len();
                self.visible = vec![true; n];
                self.gain = vec![1.0; n];
                self.caches = (0..n).map(|_| None).collect();
                self.readouts = vec![None; n];
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
        self.caches.iter_mut().for_each(|c| *c = None);
    }

    fn pan(&mut self, seconds: f64) {
        self.t_start += seconds;
        self.t_end += seconds;
        self.caches.iter_mut().for_each(|c| *c = None);
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

        let hit = self.caches[channel]
            .as_ref()
            .map(|c| c.from_sample == from && c.to_sample == to && c.width == width)
            .unwrap_or(false);
        if hit {
            return true;
        }

        let n = (to - from) as usize;
        let data = match son.read_waveform(channel, from, n) {
            Ok(d) => d,
            Err(_) => return false,
        };

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
        self.refresh_readouts();

        // ── menu bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        self.open_dialog = true;
                    }
                    if ui.button("Close").clicked() {
                        self.son = None;
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
                    if ui.button("Reset channel gain").clicked() {
                        self.gain.iter_mut().for_each(|g| *g = 1.0);
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Fit to window").clicked() {
                        self.fit();
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
                    if ui.button("Cursors to view edges").clicked() {
                        self.cursors = [self.t_start, self.t_end];
                    }
                    if ui.button("Cursors to mid ±10%").clicked() {
                        let mid = (self.t_start + self.t_end) * 0.5;
                        let d = (self.t_end - self.t_start) * 0.1;
                        self.cursors = [mid - d, mid + d];
                    }
                });
                ui.menu_button("Analysis", |ui| {
                    ui.label("Measurements and averaging are not built yet.");
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Sharar — read and display CED Spike2 .smr files.");
                    ui.label("Wheel zooms, middle-drag pans, F fits, +/- channel gain.");
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
                    self.open_dialog = true;
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
                }
            });
        });

        // ── status bar ──────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                let Some(son) = &self.son else {
                    ui.label("No file open.");
                    return;
                };
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, ch) in son.channels.iter().enumerate() {
                        let color = CHANNEL_COLORS[i % CHANNEL_COLORS.len()];
                        let is_sel = self.selected == Some(i);
                        egui::Frame::none()
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

        // ── data view ───────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.son.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Open a .smr file: File → Open…");
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
                .filter(|i| meta[**i].kind.is_event())
                .count();
            let event_h = 26.0;
            let wave_h = ((plot.height() - event_rows as f32 * event_h).max(0.0)
                / (visible.len() - event_rows).max(1) as f32)
                .max(0.0);

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
                            let (lo, hi) = if lo.is_finite() && hi.is_finite() && hi > lo {
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
                        Pos2::new(plot.right(), (y + event_h - 2.0).min(plot.bottom())),
                    );
                    band_tops.push((ci, band.top(), band.bottom()));
                    y += event_h;

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

                    if let Ok(events) = self.son.as_mut().unwrap().read_events(ci) {
                        let base = band.bottom() - 3.0;
                        painter.line_segment(
                            [Pos2::new(band.left(), base), Pos2::new(band.right(), base)],
                            Stroke::new(1.0, GRID_STRONG),
                        );
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
                            // Mouse and keyboard markers carry a code; show it as a character.
                            if labelled < 12 {
                                if let Some(code) = ev.code {
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
            let step = nice_step(span, 10.0);
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
                let tag = Rect::from_min_size(
                    Pos2::new(x + 2.0, plot.top() + 1.0),
                    Vec2::new(14.0, 13.0),
                );
                painter.rect_filled(tag, 2.0, color);
                painter.text(
                    tag.center(),
                    Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    FontId::monospace(9.0),
                    Color32::WHITE,
                );
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

            let (fit, up, down, reset) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::F),
                    i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                    i.key_pressed(egui::Key::Minus),
                    i.key_pressed(egui::Key::Num0),
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

            painter.rect_stroke(
                outer_plot,
                0.0,
                Stroke::new(1.0, AXIS),
                egui::StrokeKind::Inside,
            );
        });

        // ── open dialog ─────────────────────────────────────────────────────
        if self.open_dialog {
            let mut open = self.open_dialog;
            egui::Window::new("Open .smr")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Path to a Spike2 data file:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .desired_width(520.0)
                            .hint_text("/path/to/file.smr"),
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
                                if p.extension().map(|e| e == "smr").unwrap_or(false) {
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

fn main() -> eframe::Result {
    let path = std::env::args().nth(1).map(PathBuf::from);

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
        Box::new(|_cc| Ok(Box::new(App::new(path)))),
    )
}
