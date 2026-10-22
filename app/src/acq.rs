//! Acquisition: a source of samples, and a live strip-chart view for them.
//!
//! The 1401 backend is deliberately a stub. The protocol it needs is recovered (see the
//! `ced1401` crate and its FINDINGS): open the device, register transfer areas, poll the
//! character channel, and service the `Esc`-framed DMA requests the 1401 sends. None of
//! that can be exercised here — no 1401 is attached — so rather than ship code that has
//! never moved a byte, the hardware path returns a clear error and the sampling pipeline
//! is verified against a synthetic source instead.

use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};

use crate::CHANNEL_COLORS;

/// Where samples come from.
pub trait Source {
    /// Channel titles, fixed for the life of the source.
    fn channels(&self) -> Vec<String>;
    /// Samples per second per channel.
    fn sample_rate(&self) -> f64;
    /// Produce the next block of samples, one `Vec` per channel, all the same length.
    fn next_block(&mut self) -> Vec<Vec<f32>>;
    /// Human-readable name for the status bar.
    fn describe(&self) -> String;
}

/// A synthetic source: a slow oscillation, a faster one, and a pulse train, so the live
/// path can be exercised end to end without hardware.
pub struct SyntheticSource {
    rate: f64,
    t: f64,
}

impl SyntheticSource {
    pub fn new(rate: f64) -> Self {
        Self { rate, t: 0.0 }
    }
}

impl Source for SyntheticSource {
    fn channels(&self) -> Vec<String> {
        vec!["sine 1 Hz".into(), "sine 7 Hz".into(), "pulses".into()]
    }

    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn next_block(&mut self) -> Vec<Vec<f32>> {
        let n = (self.rate / 60.0).max(1.0) as usize; // ~60 blocks a second
        let mut out = vec![Vec::with_capacity(n); 3];
        for _ in 0..n {
            let phase = 2.0 * std::f64::consts::PI * self.t;
            out[0].push((phase * 1.0).sin() as f32 * 0.8);
            out[1].push((phase * 7.0).sin() as f32 * 0.3);
            out[2].push(if (self.t * 2.0).fract() < 0.1 { 0.9 } else { -0.9 });
            self.t += 1.0 / self.rate;
        }
        out
    }

    fn describe(&self) -> String {
        format!("synthetic source at {:.0} Hz", self.rate)
    }
}

/// The 1401 backend, not yet implemented.
pub struct Ced1401Source;

impl Ced1401Source {
    pub fn open() -> Result<Self, String> {
        Err("no 1401 attached: the hardware path is not wired up yet".to_string())
    }
}

impl Source for Ced1401Source {
    fn channels(&self) -> Vec<String> {
        Vec::new()
    }
    fn sample_rate(&self) -> f64 {
        0.0
    }
    fn next_block(&mut self) -> Vec<Vec<f32>> {
        Vec::new()
    }
    fn describe(&self) -> String {
        "1401".to_string()
    }
}

/// Ring buffer of acquired samples plus a strip-chart drawing of it.
pub struct Live {
    source: Box<dyn Source>,
    titles: Vec<String>,
    rate: f64,
    /// Seconds of history kept, and drawn.
    window: f64,
    /// One buffer per channel.
    data: Vec<Vec<f32>>,
    last_step: Instant,
    last_error: Option<String>,
}

impl Live {
    pub fn new(source: Box<dyn Source>) -> Self {
        let titles = source.channels();
        let rate = source.sample_rate();
        let cap = (rate * 10.0) as usize;
        Self {
            data: titles.iter().map(|_| Vec::with_capacity(cap)).collect(),
            titles,
            rate,
            window: 5.0,
            last_step: Instant::now(),
            last_error: None,
            source,
        }
    }

    pub fn describe(&self) -> String {
        self.source.describe()
    }

    pub fn channels(&self) -> &[String] {
        &self.titles
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Pull one block from the source, at whatever rate the GUI is running.
    pub fn step(&mut self) {
        let block = self.source.next_block();
        let keep = (self.rate * self.window) as usize;
        for (ch, samples) in block.into_iter().enumerate() {
            if let Some(buf) = self.data.get_mut(ch) {
                buf.extend_from_slice(&samples);
                if buf.len() > keep {
                    let drop = buf.len() - keep;
                    buf.drain(..drop);
                }
            }
        }
        self.last_step = Instant::now();
    }

    pub fn seconds_since_step(&self) -> Duration {
        self.last_step.elapsed()
    }

    /// Latest value per channel, for the read-out.
    pub fn latest(&self) -> Vec<f32> {
        self.data
            .iter()
            .map(|b| b.last().copied().unwrap_or(f32::NAN))
            .collect()
    }

    pub fn draw(&self, ui: &mut egui::Ui) {
        let full = ui.available_rect_before_wrap();
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::WHITE);

        let gutter = 132.0;
        let plot = Rect::from_min_max(
            Pos2::new(full.left() + gutter, full.top()),
            Pos2::new(full.right(), full.bottom() - 26.0),
        );

        let rows = self.data.len().max(1);
        let row_h = plot.height() / rows as f32;

        for (i, buf) in self.data.iter().enumerate() {
            let top = plot.top() + row_h * i as f32;
            let band = Rect::from_min_max(
                Pos2::new(plot.left(), top),
                Pos2::new(plot.right(), top + row_h - 6.0),
            );
            let color = CHANNEL_COLORS[i % CHANNEL_COLORS.len()];

            // zero line
            painter.line_segment(
                [
                    Pos2::new(band.left(), band.center().y),
                    Pos2::new(band.right(), band.center().y),
                ],
                Stroke::new(1.0, Color32::from_rgb(226, 229, 233)),
            );

            if buf.len() > 1 {
                let n = buf.len();
                let points: Vec<Pos2> = buf
                    .iter()
                    .enumerate()
                    .map(|(k, v)| {
                        let x = band.left() + band.width() * k as f32 / (n - 1) as f32;
                        let y = band.center().y - v.clamp(-1.0, 1.0) * band.height() * 0.45;
                        Pos2::new(x, y)
                    })
                    .collect();
                painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
            }

            painter.text(
                Pos2::new(full.left() + 8.0, band.top() + 2.0),
                Align2::LEFT_TOP,
                self.titles.get(i).cloned().unwrap_or_default(),
                FontId::proportional(11.0),
                color,
            );
            painter.text(
                Pos2::new(full.left() + 8.0, band.top() + 15.0),
                Align2::LEFT_TOP,
                format!("{:.4}", self.latest().get(i).copied().unwrap_or(f32::NAN)),
                FontId::monospace(9.0),
                Color32::from_rgb(70, 70, 70),
            );
        }

        painter.text(
            Pos2::new(plot.left() + 4.0, plot.bottom() + 6.0),
            Align2::LEFT_TOP,
            format!(
                "live · {:.0} Hz · {:.1} s window · {} samples",
                self.rate,
                self.window,
                self.data.first().map(|b| b.len()).unwrap_or(0)
            ),
            FontId::monospace(9.0),
            Color32::from_rgb(70, 70, 70),
        );

        painter.rect_stroke(
            Rect::from_min_max(full.min, Pos2::new(full.right(), plot.bottom())),
            0.0,
            Stroke::new(1.0, Color32::from_rgb(70, 70, 70)),
            egui::StrokeKind::Inside,
        );

        let _ = ui.allocate_rect(full, egui::Sense::hover());
        let _ = Vec2::ZERO;
    }
}
