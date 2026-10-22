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

    pub fn is_wavemark(self) -> bool {
        matches!(self, Kind::AdcMark | Kind::RealMark)
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
    /// Bytes per payload element, including any per-event waveform or text payload.
    pub fn item_bytes(&self) -> usize {
        match self.kind {
            Kind::Adc => 2,
            Kind::RealWave => 4,
            Kind::EventFall | Kind::EventRise | Kind::EventBoth => 4,
            Kind::Marker => 8,
            Kind::AdcMark | Kind::RealMark | Kind::TextMark => {
                8 + self.n_extra.max(0) as usize
            }
            Kind::Empty => 1,
        }
    }

    pub fn is_wavemark(&self) -> bool {
        matches!(self.kind, Kind::AdcMark | Kind::RealMark)
    }

    pub fn sample_rate(&self) -> f64 {
        if self.sample_interval > 0.0 {
            1.0 / self.sample_interval
        } else {
            0.0
        }
    }
}

/// Remove the local mean from a signal with a centred boxcar of `window` samples.
///
/// This is the operation Spike2 calls *DC Remove*: each sample loses the mean of the
/// window around it, so the baseline is flattened while the waveform rides on top. Near
/// the ends the window shrinks rather than wrapping, which keeps the edges from jumping.
pub fn dc_remove(samples: &mut [f32], window: usize) {
    let n = samples.len();
    if n == 0 || window < 2 {
        return;
    }
    let half = (window / 2).max(1);

    // Prefix sums keep the boxcar linear in the sample count.
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for v in samples.iter() {
        let last = *prefix.last().unwrap();
        prefix.push(last + f64::from(*v));
    }

    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(n);
        let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
        out[i] = samples[i] - mean as f32;
    }
    samples.copy_from_slice(&out);
}

/// Which side of the spectrum a filter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    LowPass,
    HighPass,
}

/// A filter request, in the units the caller works in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSpec {
    pub kind: FilterKind,
    /// Corner (-6 dB) frequency in Hz.
    pub cutoff: f64,
    /// Number of FIR coefficients. Ignored by the IIR path.
    pub taps: usize,
    /// True for the two-pole IIR, false for the windowed-sinc FIR.
    pub iir: bool,
}

impl FilterSpec {
    /// An identity tag, so caches can tell one filter setting from another.
    ///
    /// The cutoff is quantised to 0.1 Hz rather than hashed from its f64 bits: round
    /// values like 100.0 and 200.0 share their low mantissa bits, so the raw bits would
    /// collide.
    pub fn tag(&self) -> u64 {
        let kind = match self.kind {
            FilterKind::LowPass => 1u64,
            FilterKind::HighPass => 2u64,
        };
        let cutoff = ((self.cutoff.max(0.0) * 10.0).round() as u64) & 0x3f_ffff;
        kind | (cutoff << 2) | ((self.taps as u64 & 0xfff) << 24) | ((self.iir as u64) << 36)
    }
}

/// Windowed-sinc low-pass coefficients with unity gain at DC.
///
/// `cutoff` is normalised to the sample rate (0.5 = Nyquist). The Hamming window trades
/// a wider transition for lower ripple than a rectangular one.
///
/// **This is not how Spike2 designs its filters.** CED's designer is `FIRWin.s2l`, whose
/// symbols are the Parks-McClellan exchange (`CFIR::fircalc` with a `CBand`, 
/// `CFIR::GetExtremal`, `CFIR::InIteration`), so its filters are equiripple against a
/// requested deviation rather than windowed. Results here agree in broad behaviour and
/// will differ in detail; treat this as a working approximation, not a match.
pub fn fir_lowpass(cutoff: f64, taps: usize) -> Vec<f64> {
    let n = taps.max(3) | 1; // odd, so the filter is symmetric about a sample
    let fc = cutoff.clamp(1e-6, 0.4999);
    let m = (n - 1) as f64;
    let mid = m / 2.0;

    let mut h = Vec::with_capacity(n);
    for i in 0..n {
        let k = i as f64 - mid;
        let sinc = if k.abs() < 1e-12 {
            2.0 * fc
        } else {
            (2.0 * std::f64::consts::PI * fc * k).sin() / (std::f64::consts::PI * k)
        };
        // Hamming window
        let w = 0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / m).cos();
        h.push(sinc * w);
    }

    let sum: f64 = h.iter().sum();
    if sum.abs() > 1e-12 {
        for c in h.iter_mut() {
            *c /= sum;
        }
    }
    h
}

/// High-pass by spectral inversion of a low-pass, offsetting the centre tap so the sum
/// of coefficients stays zero.
pub fn fir_highpass(cutoff: f64, taps: usize) -> Vec<f64> {
    let mut h = fir_lowpass(cutoff, taps);
    let mid = h.len() / 2;
    for c in h.iter_mut() {
        *c = -*c;
    }
    h[mid] += 1.0;
    h
}

/// One biquad section (direct form 1).
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    /// Two-pole (Butterworth, Q = 1/sqrt 2) low-pass at `cutoff` normalised to the rate.
    pub fn lowpass(cutoff: f64) -> Self {
        let fc = cutoff.clamp(1e-6, 0.4999);
        let w0 = 2.0 * std::f64::consts::PI * fc;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;

        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Two-pole high-pass at `cutoff` normalised to the rate.
    pub fn highpass(cutoff: f64) -> Self {
        let fc = cutoff.clamp(1e-6, 0.4999);
        let w0 = 2.0 * std::f64::consts::PI * fc;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;

        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    pub fn apply(&self, samples: &mut [f32]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for v in samples.iter_mut() {
            let x0 = f64::from(*v);
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            *v = y0 as f32;
        }
    }
}

/// Apply a filter in place. FIR runs as a centred convolution, so the output is not
/// delayed relative to the input; the IIR is causal, as Spike2's own filters are.
pub fn apply_filter(samples: &mut [f32], spec: &FilterSpec, sample_rate: f64) {
    if samples.is_empty() || sample_rate <= 0.0 || spec.cutoff <= 0.0 {
        return;
    }
    let nyquist = sample_rate / 2.0;
    if spec.cutoff >= nyquist {
        // Nothing to remove: a low pass above Nyquist is the identity, and a high pass
        // above Nyquist would delete the signal.
        if spec.kind == FilterKind::HighPass {
            for v in samples.iter_mut() {
                *v = 0.0;
            }
        }
        return;
    }
    let norm = spec.cutoff / sample_rate;

    if spec.iir {
        let bq = match spec.kind {
            FilterKind::LowPass => Biquad::lowpass(norm),
            FilterKind::HighPass => Biquad::highpass(norm),
        };
        bq.apply(samples);
        return;
    }

    let coeffs = match spec.kind {
        FilterKind::LowPass => fir_lowpass(norm, spec.taps),
        FilterKind::HighPass => fir_highpass(norm, spec.taps),
    };
    let mid = coeffs.len() / 2;
    let input: Vec<f32> = samples.to_vec();
    let n = input.len();
    for i in 0..n {
        let mut acc = 0.0f64;
        for (k, c) in coeffs.iter().enumerate() {
            let idx = i as isize + k as isize - mid as isize;
            if idx >= 0 && (idx as usize) < n {
                acc += c * f64::from(input[idx as usize]);
            }
        }
        samples[i] = acc as f32;
    }
}

/// Peri-stimulus average of a waveform around a set of event times.
///
/// Each event contributes one sample per bin, taken at the bin centre; bins with no
/// samples come back as `NaN`. Times are seconds from the start of the file.
#[derive(Debug, Clone)]
pub struct Average {
    /// Mean value per bin, in channel units.
    pub bins: Vec<f64>,
    /// How many events contributed to each bin.
    pub counts: Vec<usize>,
    /// Time of the first bin relative to the event, in seconds (negative = before).
    pub start: f64,
    /// Width of one bin, in seconds.
    pub bin_width: f64,
}

/// Average `samples` around `events`.
///
/// `samples` must begin at `start_time` and be evenly spaced by `sample_interval`.
/// Events outside the available data are skipped rather than clamped, so a stimulus at
/// the very start does not produce a half window.
pub fn peri_stimulus_average(
    samples: &[f32],
    start_time: f64,
    sample_interval: f64,
    events: &[f64],
    pre: f64,
    post: f64,
    nbins: usize,
) -> Average {
    let nbins = nbins.max(1);
    let bin_width = (pre + post) / nbins as f64;
    let mut sums = vec![0.0f64; nbins];
    let mut counts = vec![0usize; nbins];

    if sample_interval <= 0.0 || samples.is_empty() || events.is_empty() {
        return Average {
            bins: vec![f64::NAN; nbins],
            counts,
            start: -pre,
            bin_width,
        };
    }

    let end_time = start_time + samples.len() as f64 * sample_interval;
    for &t0 in events {
        if t0 - pre < start_time || t0 + post > end_time {
            continue;
        }
        for (b, (sum, count)) in sums.iter_mut().zip(counts.iter_mut()).enumerate() {
            let centre = t0 - pre + (b as f64 + 0.5) * bin_width;
            let idx = ((centre - start_time) / sample_interval).round() as i64;
            if idx < 0 || idx as usize >= samples.len() {
                continue;
            }
            let v = samples[idx as usize];
            if v.is_finite() {
                *sum += f64::from(v);
                *count += 1;
            }
        }
    }

    Average {
        bins: sums
            .iter()
            .zip(counts.iter())
            .map(|(s, c)| if *c > 0 { s / *c as f64 } else { f64::NAN })
            .collect(),
        counts,
        start: -pre,
        bin_width,
    }
}

/// An event or marker timestamp.
#[derive(Debug, Clone)]
pub struct Event {
    /// Seconds from the start of the file.
    pub time: f64,
    /// Marker or text code, when the channel carries one.
    pub code: Option<i32>,
    /// Text label for text-mark channels.
    pub text: Option<String>,
}

/// One event carrying a waveform snippet (`AdcMark` or `RealMark` channel).
#[derive(Debug, Clone)]
pub struct WaveMark {
    /// Seconds from the start of the file.
    pub time: f64,
    /// Sort or marker code attached to the event.
    pub code: i32,
    /// The snippet, in channel units.
    pub samples: Vec<f32>,
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
        let mut cursor = first;
        let mut out_pos = 0usize;

        for block in &channel.blocks {
            let n = block.items.max(0) as u64;
            if cursor >= n {
                cursor -= n;
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
        let stride = channel.item_bytes();
        let mut out = Vec::with_capacity(channel.items as usize);
        let tf = self.time_factor;

        for block in &channel.blocks {
            let n = block.items.max(0) as usize;
            let mut raw = vec![0u8; n * stride];
            self.file.seek(SeekFrom::Start(block.data_pos))?;
            self.file.read_exact(&mut raw)?;

            for chunk in raw.chunks_exact(stride) {
                let tick = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let (code, text) = if stride >= 8 {
                    let code = i32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                    let label = if channel.kind == Kind::TextMark && stride > 8 {
                        let raw = &chunk[8..];
                        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                        let s: String = raw[..end]
                            .iter()
                            .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { ' ' })
                            .collect();
                        let s = s.trim().to_string();
                        if s.is_empty() { None } else { Some(s) }
                    } else {
                        None
                    };
                    (Some(code), label)
                } else {
                    (None, None)
                };
                out.push(Event {
                    time: f64::from(tick) * tf,
                    code,
                    text,
                });
            }
        }

        Ok(out)
    }

    /// Read every waveform-mark event on an `AdcMark` or `RealMark` channel.
    pub fn read_wavemarks(&mut self, ch: usize) -> io::Result<Vec<WaveMark>> {
        let channel = &self.channels[ch];
        if !channel.is_wavemark() {
            return Ok(Vec::new());
        }
        let stride = channel.item_bytes();
        let is_adc = channel.kind == Kind::AdcMark;
        let width = channel.n_extra.max(0) as usize;
        let elem = if is_adc { 2 } else { 4 };
        let points = width / elem;
        let scale = channel.scale;
        let offset = channel.offset;
        let tf = self.time_factor;

        let mut out = Vec::with_capacity(channel.items as usize);
        for block in &channel.blocks {
            let n = block.items.max(0) as usize;
            let mut raw = vec![0u8; n * stride];
            self.file.seek(SeekFrom::Start(block.data_pos))?;
            self.file.read_exact(&mut raw)?;

            for chunk in raw.chunks_exact(stride) {
                let tick = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let code = i32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                let mut samples = Vec::with_capacity(points);
                for p in 0..points {
                    let at = 8 + p * elem;
                    if at + elem > chunk.len() {
                        break;
                    }
                    samples.push(if is_adc {
                        let c = i16::from_le_bytes([chunk[at], chunk[at + 1]]);
                        f32::from(c) * scale / 65536.0 + offset
                    } else {
                        f32::from_bits(u32::from_le_bytes([
                            chunk[at],
                            chunk[at + 1],
                            chunk[at + 2],
                            chunk[at + 3],
                        ]))
                    });
                }
                out.push(WaveMark {
                    time: f64::from(tick) * tf,
                    code,
                    samples,
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

#[cfg(test)]
mod process_tests {
    use super::dc_remove;

    #[test]
    fn constant_signal_becomes_flat() {
        let mut data = vec![1.25f32; 512];
        dc_remove(&mut data, 201);
        assert!(data.iter().all(|v| v.abs() < 1e-6), "residual {:?}", &data[..4]);
    }

    #[test]
    fn slow_drift_is_removed_but_a_spike_survives() {
        // Ramp drift plus one sharp excursion in the middle.
        let mut data: Vec<f32> = (0..2000).map(|i| i as f32 * 0.01).collect();
        data[1000] += 5.0;
        let mut processed = data.clone();
        dc_remove(&mut processed, 201);

        // The drift is gone: the far ends are near zero.
        assert!(processed[0].abs() < 0.6, "start {}", processed[0]);
        assert!(processed[1999].abs() < 0.6, "end {}", processed[1999]);

        // The excursion is still there, close to its original height above the drift.
        let peak = processed.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak > 4.5, "peak {peak}");
    }

    #[test]
    fn short_and_degenerate_inputs_are_ignored() {
        let mut empty: Vec<f32> = Vec::new();
        dc_remove(&mut empty, 101);
        let mut one = vec![3.0f32];
        dc_remove(&mut one, 101);
        assert_eq!(one, vec![0.0], "a single sample loses its own mean");
    }

    #[test]
    fn window_of_one_is_a_no_op() {
        let mut data = vec![1.0f32, 2.0, 3.0, 4.0];
        let copy = data.clone();
        dc_remove(&mut data, 1);
        assert_eq!(data, copy);
    }
}

#[cfg(test)]
mod average_tests {
    use super::peri_stimulus_average;

    #[test]
    fn average_recovers_a_known_waveform() {
        // 1 kHz sine at 1 Hz, four events exactly one period apart.
        let n = 2000;
        let interval = 0.001f64;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * i as f32 * interval as f32).sin())
            .collect();
        let events = vec![0.5, 1.5];
        let avg = peri_stimulus_average(&samples, 0.0, interval, &events, 0.2, 0.2, 40);

        assert_eq!(avg.bins.len(), 40);
        assert_eq!(avg.counts.iter().sum::<usize>(), 80, "two events x 40 bins");
        // The average of a periodic signal at the same phase is the signal itself. The
        // sample time is the event time plus the bin's offset from the event.
        let mid = 20;
        let rel = avg.start + (mid as f64 + 0.5) * avg.bin_width;
        let expect = (2.0 * std::f64::consts::PI * (events[0] + rel)).sin();
        assert!((avg.bins[mid] - expect).abs() < 1e-3, "{} vs {}", avg.bins[mid], expect);
    }

    #[test]
    fn events_without_a_full_window_are_skipped() {
        let samples = vec![1.0f32; 100];
        let avg = peri_stimulus_average(&samples, 0.0, 0.001, &[0.0], 0.5, 0.5, 10);
        assert_eq!(avg.counts.iter().sum::<usize>(), 0);
        assert!(avg.bins.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn empty_event_list_yields_nans() {
        let samples = vec![1.0f32; 100];
        let avg = peri_stimulus_average(&samples, 0.0, 0.001, &[], 0.01, 0.01, 5);
        assert!(avg.bins.iter().all(|v| v.is_nan()));
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    /// Amplitude of a sinusoid at `freq` after filtering, measured by peak of the middle
    /// third (away from the FIR edges).
    fn gain(spec: &FilterSpec, freq: f64, rate: f64) -> f64 {
        let n = 4000usize;
        let mut data: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                (2.0 * std::f64::consts::PI * freq * t).sin() as f32
            })
            .collect();
        apply_filter(&mut data, spec, rate);
        let third = n / 3;
        let peak = data[third..2 * third]
            .iter()
            .cloned()
            .fold(0.0f32, |a, v| a.max(v.abs()));
        f64::from(peak)
    }

    #[test]
    fn fir_lowpass_keeps_low_and_rejects_high() {
        let spec = FilterSpec { kind: FilterKind::LowPass, cutoff: 100.0, taps: 129, iir: false };
        let rate = 1000.0;
        assert!(gain(&spec, 10.0, rate) > 0.98, "passband gain {}", gain(&spec, 10.0, rate));
        assert!(gain(&spec, 400.0, rate) < 0.05, "stopband gain {}", gain(&spec, 400.0, rate));
    }

    #[test]
    fn fir_highpass_rejects_low_and_keeps_high() {
        let spec = FilterSpec { kind: FilterKind::HighPass, cutoff: 100.0, taps: 129, iir: false };
        let rate = 1000.0;
        assert!(gain(&spec, 10.0, rate) < 0.05, "stopband gain {}", gain(&spec, 10.0, rate));
        assert!(gain(&spec, 400.0, rate) > 0.9, "passband gain {}", gain(&spec, 400.0, rate));
    }

    #[test]
    fn iir_lowpass_rolls_off_high_frequencies() {
        let spec = FilterSpec { kind: FilterKind::LowPass, cutoff: 100.0, taps: 0, iir: true };
        let rate = 1000.0;
        let low = gain(&spec, 20.0, rate);
        let high = gain(&spec, 400.0, rate);
        assert!(low > 0.95, "passband {low}");
        assert!(high < 0.25, "stopband {high}");
    }

    #[test]
    fn fir_lowpass_has_unity_dc_gain() {
        let h = fir_lowpass(0.1, 129);
        let sum: f64 = h.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sum {sum}");
        // High-pass must remove a constant.
        let mut flat = vec![1.0f32; 500];
        apply_filter(
            &mut flat,
            &FilterSpec { kind: FilterKind::HighPass, cutoff: 100.0, taps: 129, iir: false },
            1000.0,
        );
        assert!(flat[250].abs() < 1e-6, "residual {}", flat[250]);
    }

    #[test]
    fn tags_distinguish_settings() {
        let a = FilterSpec { kind: FilterKind::LowPass, cutoff: 100.0, taps: 129, iir: false };
        let b = FilterSpec { kind: FilterKind::HighPass, cutoff: 100.0, taps: 129, iir: false };
        let c = FilterSpec { kind: FilterKind::LowPass, cutoff: 200.0, taps: 129, iir: false };
        assert_ne!(a.tag(), b.tag());
        assert_ne!(a.tag(), c.tag());
        assert_eq!(a.tag(), a.tag());
    }
}


