//! Parks-McClellan FIR design, ported from CED's `Filters.dll`.
//!
//! Spike2 does not design filters with a window. `FIRWin.s2l` builds a band table
//! and calls `CFIR::fircalc` in `Filters.dll`, which runs a Remez exchange over a
//! uniform frequency grid. This module is a line-by-line port of that code:
//! `fircalc` → `docalc` → `Equivalent` → `Remez` → `invfft` → `Impulse`, with
//! `GetCoeff` mirroring the symmetric half-array into the full length.
//!
//! Every constant here was read out of the binary rather than derived:
//! the tap-count formula and band layouts from `FIRWin.s2l` (`FUN_180004060`,
//! `FUN_180003d30`), the exchange from `Filters.dll` (`CFIR::Remez` at 0x2930,
//! `CFIR::docalc` at 0x3520). `docs/filters.md` records where each one came from.
//!
//! The port is checked against CED's own `Filters.dll`, run under Wine by
//! `tools/fir_oracle.py`; coefficients agree to within `1e-12` relative.

/// Longest filter Spike2 will design: `CFIR::SetFilt` clamps to 0x7ff.
pub const MAX_COEFS: usize = 0x7ff;
/// Grid capacity: `CFIR::docalc` refuses a grid beyond 0x4020 points.
const MAX_GRID: usize = 0x4020;
/// `CFIR::Remez` gives up after 25 iterations.
const MAX_ITER: usize = 25;

const TWO_PI: f64 = 6.283_185_307_179_586;
const PI: f64 = 3.141_592_653_589_793;
/// Convergence margin `CFIR::Remez` allows before it declares a decrease.
const CONV_MARGIN: f64 = 1.000_01;
const GRID_EPS: f64 = 1e-6;
const DIFF_WEIGHT_FLOOR: f64 = 0.000_1;

/// One `CBand`: a design band and the desired response and weight on it.
///
/// Field order matches the struct as `CFIR` reads it — `low` at +0x00,
/// `high` at +0x08, `fx` at +0x10 (the desired amplitude) and `wt` at +0x18.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub low: f64,
    pub high: f64,
    pub fx: f64,
    pub wt: f64,
}

impl Band {
    pub fn new(low: f64, high: f64, fx: f64, wt: f64) -> Self {
        Self { low, high, fx, wt }
    }
}

/// Filter types as `CFIR::SetFilt` numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignType {
    /// Type 0 in the script language: pass band below, stop band above.
    LowPass,
    /// Type 1.
    HighPass,
    /// Type 2.
    BandPass,
    /// Type 3.
    BandStop,
    /// Type 4: differentiator, gain 2π·f.
    Differentiator,
}

impl DesignType {
    /// The `jtype` `CFIR::SetFilt` stores in +0xfc.
    ///
    /// `SetFilt` collapses anything below 4 to 0 — so a low pass, high pass,
    /// band pass and band stop all reach the exchange as type 0, and only the
    /// differentiator keeps a type of its own. The band table is what makes
    /// them different filters, not the type.
    fn jtype(self) -> i32 {
        match self {
            DesignType::Differentiator => 4,
            _ => 0,
        }
    }

    /// The `type%` the script command passes for this filter.
    pub fn script_code(self) -> i32 {
        match self {
            DesignType::LowPass => 0,
            DesignType::HighPass => 1,
            DesignType::BandPass => 2,
            DesignType::BandStop => 3,
            DesignType::Differentiator => 4,
        }
    }
}

/// Why a design could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignError {
    /// `CFIR::fircalc` rejects overlapping, reversed or out-of-range bands
    /// (error code 4).
    BadBand { index: usize },
    /// The exchange ran out of iterations or a grid evaluation failed.
    NoConvergence,
    /// The tap count or frequency is outside what Spike2 accepts.
    OutOfRange,
}

impl std::fmt::Display for DesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesignError::BadBand { index } => {
                write!(f, "band {index} is reversed, overlapping or past Nyquist")
            }
            DesignError::NoConvergence => write!(f, "the exchange did not converge"),
            DesignError::OutOfRange => write!(f, "the tap count or frequency is out of range"),
        }
    }
}

impl std::error::Error for DesignError {}

/// A finished design.
#[derive(Debug, Clone, PartialEq)]
pub struct Design {
    /// Coefficients, lowest index first, length equal to the tap count.
    pub coeffs: Vec<f64>,
    /// Deviation the exchange settled on, before the sampling-rate conversion.
    pub dev: f64,
    /// Frequency of each extremal, as `CFIR::GetExtremal` reports it.
    pub extrema: Vec<f64>,
}

/// The `CFIR` object: same fields, same layout, same arithmetic.
struct Cfm {
    nfilt: usize,
    jtype: i32,
    /// +0xf4: true unless the type is 0 or 6.
    diff: bool,
    /// +0xf8: nfilt is odd.
    odd: bool,
    /// +0xe8: half the coefficient count, rounded up for an odd low pass.
    r: usize,
    /// +0xec: grid points in use.
    ngrid: usize,
    /// +0xf0: the alternating sign `gee` applies.
    nod: i32,
    /// +0x08: error word, bit 0 = retry the grid, bit 1 = grid was shortened.
    err: u32,
    /// +0x10
    coeff: Vec<f64>,
    /// +0x28
    alpha: Vec<f64>,
    /// +0x40: cosine of the grid frequency at each extremal.
    x: Vec<f64>,
    /// +0x58
    y: Vec<f64>,
    /// +0x70
    des: Vec<f64>,
    /// +0x88
    weight: Vec<f64>,
    /// +0xa0
    grid: Vec<f64>,
    /// +0xb8
    h: Vec<f64>,
    /// +0xd0
    ext: Vec<usize>,
}

impl Cfm {
    /// `CFIR::SetFilt`, including the tap-count clamp to 2047.
    fn new(nfilt: usize, jtype: i32) -> Self {
        let nfilt = nfilt.min(MAX_COEFS);
        let odd = nfilt % 2 == 1;
        let diff = !(jtype == 0 || jtype == 6);
        let mut r = nfilt / 2;
        if odd && !diff {
            r += 1;
        }
        let n = r + 1;
        Self {
            nfilt,
            jtype,
            diff,
            odd,
            r,
            ngrid: 0,
            nod: 0,
            err: 0,
            coeff: vec![0.0; n],
            alpha: vec![0.0; n],
            x: vec![0.0; n + 1],
            y: vec![0.0; n],
            des: vec![0.0; MAX_GRID],
            weight: vec![0.0; MAX_GRID],
            grid: vec![0.0; MAX_GRID],
            h: vec![0.0; n + 1],
            ext: vec![0; n + 1],
        }
    }

    /// `CFIR::eff`: the desired amplitude at `freq` on band `i`.
    fn eff(&self, freq: f64, bands: &[Band], i: usize) -> f64 {
        let b = bands[i];
        if self.jtype == 4 {
            return freq * b.fx;
        }
        if self.jtype == 6 && b.low > 0.0 && freq > 0.0 {
            return (b.low / freq).sqrt() * b.fx;
        }
        b.fx
    }

    /// `CFIR::wate`: the weight at `freq` on band `i`. A differentiator
    /// divides by frequency so the error is measured against a flat target.
    fn wate(&self, freq: f64, bands: &[Band], i: usize) -> f64 {
        let b = bands[i];
        if self.jtype == 4 && (b.fx > DIFF_WEIGHT_FLOOR || b.fx == DIFF_WEIGHT_FLOOR) {
            return b.wt / freq;
        }
        b.wt
    }

    /// `CFIR::docalc`: build the grid, run the exchange, turn the result into
    /// an impulse response and then into coefficients. Returns the deviation,
    /// or a negative value when the exchange failed.
    fn docalc(&mut self, bands: &[Band], lgrid: i32) -> f64 {
        let lgrid0 = lgrid;
        let mut lgrid = lgrid;
        // Grid sizing: each band contributes one point per step of `delf`, and
        // the grid is shrunk until it fits the 0x4020-point capacity.
        loop {
            let delf = 0.5 / (self.r as f64 * lgrid as f64);
            let mut n: i64 = 0;
            for b in bands {
                n -= ((b.high - b.low) * (-1.0 / delf)) as i64;
            }
            if (n as usize) < MAX_GRID || lgrid <= 1 {
                break;
            }
            lgrid -= 1;
        }
        if lgrid != lgrid0 {
            self.err |= 2;
        }
        let delf = 0.5 / (self.r as f64 * lgrid as f64);

        let mut n = 0usize;
        for (i, b) in bands.iter().enumerate() {
            let mut lo = b.low;
            // A differentiator cannot start at DC, so band 0 begins one step in.
            if i == 0 && self.diff && b.low <= delf && delf != b.low {
                lo = delf;
            }
            let hi = b.high;
            loop {
                self.grid[n] = lo;
                self.des[n] = self.eff(lo, bands, i);
                self.weight[n] = self.wate(lo, bands, i);
                n += 1;
                lo += delf;
                if lo > hi {
                    break;
                }
            }
            // The last point of the band is snapped to the band edge, which is
            // what 0x18000376e does — it overwrites rather than appends.
            n -= 1;
            self.grid[n] = hi;
            self.des[n] = self.eff(hi, bands, i);
            self.weight[n] = self.wate(hi, bands, i);
            n += 1;
        }
        self.ngrid = n;

        // Half a sample from Nyquist is unreachable for the mirrored half.
        if self.diff == self.odd && (0.5 - delf) < self.grid[n - 1] {
            self.ngrid = n - 1;
        }

        self.equivalent();

        // Equally spaced initial extrema.
        let last = (self.ngrid - 1) as f64;
        for k in 0..self.r {
            self.ext[k] = (k as f64 * (last / self.r as f64)) as usize;
        }
        self.ext[self.r] = self.ngrid - 1;

        // Full coverage of 0..Nyquist is the case invfft handles without the
        // frequency warp. It holds for every FIRQuick design.
        let full = bands[0].low == 0.0 && bands[bands.len() - 1].high == 0.5;

        let dev = self.remez();
        if dev < 0.0 {
            return dev;
        }
        self.invfft(full);
        self.impulse();
        dev
    }

    /// `CFIR::Equivalent`: the odd-length low pass and the differentiator are
    /// exchanged in a warped frequency variable, which is a division of the
    /// desired response and a multiplication of the weight.
    fn equivalent(&mut self) {
        if !self.diff {
            if self.odd {
                return;
            }
            for i in 0..self.ngrid {
                let d = (self.grid[i] * PI).cos();
                self.des[i] /= d;
                self.weight[i] *= d;
            }
        } else if !self.odd {
            for i in 0..self.ngrid {
                let d = (self.grid[i] * PI).sin();
                self.des[i] /= d;
                self.weight[i] *= d;
            }
        } else {
            for i in 0..self.ngrid {
                let d = (self.grid[i] * TWO_PI).sin();
                self.des[i] /= d;
                self.weight[i] *= d;
            }
        }
    }

    /// `CFIR::geeNoErr`: barycentric interpolation of the desired response at
    /// grid point `i`, using the current extremal set.
    fn gee_no_err(&self, i: usize, ncoef: usize) -> f64 {
        let cf = (self.grid[i] * TWO_PI).cos();
        let mut num = 0.0;
        let mut den = 0.0;
        for j in 0..ncoef {
            let d = cf - self.x[j];
            if d == 0.0 {
                return 1.0;
            }
            let a = self.alpha[j] / d;
            den += a;
            num += a * self.y[j];
        }
        num / den
    }

    /// `CFIR::gee`: the error at grid point `i`, weighted and sign-flipped.
    fn gee(&self, i: usize, nod: i32) -> f64 {
        (self.gee_no_err(i, self.r + 1) - self.des[i]) * self.weight[i] * nod as f64
    }

    /// `CFIR::Remez`: the exchange itself.
    fn remez(&mut self) -> f64 {
        let r = self.r;
        let step = (r.saturating_sub(1) / 15) + 1;
        let mut prev_dev = -1.0f64;
        let mut iter = 0usize;

        loop {
            self.ext[r + 1] = self.ngrid;
            for k in 0..=r {
                self.x[k] = (self.grid[self.ext[k]] * TWO_PI).cos();
            }

            // Barycentric denominators, multiplied in the strided order the
            // original uses so the rounding matches.
            for k in 0..=r {
                let mut acc = 1.0f64;
                for s in 0..step {
                    let mut j = s;
                    while j <= r {
                        if j != k {
                            acc = (self.x[k] - self.x[j]) * acc * 2.0;
                        }
                        j += step;
                    }
                }
                self.alpha[k] = 1.0 / acc;
            }

            // Deviation from the weighted error at the extrema.
            let mut num = 0.0;
            let mut den = 0.0;
            let mut sign = 1i32;
            for k in 0..=r {
                let g = self.ext[k];
                num += self.alpha[k] * self.des[g];
                den += (sign as f64 * self.alpha[k]) / self.weight[g];
                sign = -sign;
            }
            let dev = (num / den).abs();
            let mut s0 = 1i32;
            if num / den > 0.0 {
                s0 = -1;
            }

            let mut sg = s0;
            for k in 0..=r {
                let g = self.ext[k];
                self.y[k] = (sg as f64 * dev) / self.weight[g] + self.des[g];
                sg = -sg;
            }

            if dev < prev_dev {
                self.err |= 1;
                return 1.0;
            }

            let changed = self.search_extrema(dev, s0);
            if changed == 0 {
                return dev;
            }

            iter += 1;
            if iter > MAX_ITER {
                return dev;
            }
            prev_dev = dev;
        }
    }

    /// `CFIR::Remez`'s extremal search, transcribed from the code at 0x180002c60
    /// rather than inferred: each extremum moves to the peak of the error
    /// between its neighbours, upward first, and once anything has moved the
    /// downward scan is abandoned. After the sweep the two ends are re-checked
    /// against `first_error * 1.00001` and the set is shifted if a peak was
    /// missed outside its range.
    fn search_extrema(&mut self, dev: f64, s0: i32) -> usize {
        let r = self.r;
        let mut res8: usize = 0;
        let nod_orig = -s0;
        let mut nod = nod_orig;
        let mut last_new: isize = -1;
        let mut first_err = dev;
        let mut last_best = dev;
        let mut l18 = self.ext[0];
        let mut l10 = self.ext[r];

        for k in 0..=r {
            nod = -nod;
            let j = self.ext[k];
            let next = self.ext[k + 1];
            let mut best = dev;
            let mut body_done = false;

            // Upward from j+1, when the interval is wide enough.
            if next > j + 1 {
                let mut i = j + 1;
                let mut e = self.gee(i, nod);
                if e > dev {
                    best = e;
                    i += 1;
                    while i < next {
                        let e2 = self.gee(i, nod);
                        if e2 <= best {
                            break;
                        }
                        best = e2;
                        i += 1;
                    }
                    res8 += 1;
                    last_new = (i - 1) as isize;
                    self.ext[k] = i - 1;
                    body_done = true;
                }
            }

            if !body_done {
                let mut moved = false;
                if (j as isize - 1) > last_new {
                    let mut i = j as isize - 1;
                    let mut e = self.gee(i as usize, nod);
                    loop {
                        if e > dev {
                            best = e;
                            i -= 1;
                            while i > last_new {
                                let e2 = self.gee(i as usize, nod);
                                if e2 <= best {
                                    break;
                                }
                                best = e2;
                                i -= 1;
                            }
                            // This branch keeps the old index as the bound.
                            last_new = j as isize;
                            self.ext[k] = (i + 1) as usize;
                            res8 += 1;
                            moved = true;
                            break;
                        }
                        if res8 != 0 {
                            break;
                        }
                        i -= 1;
                        if i <= last_new {
                            break;
                        }
                        e = self.gee(i as usize, nod);
                    }
                }
                if !moved {
                    // Only while nothing has moved: take the first point past
                    // the deviation deeper into the interval.
                    if res8 == 0 {
                        let mut i = j + 2;
                        while i < next {
                            let e = self.gee(i, nod);
                            i += 1;
                            if e > dev {
                                best = e;
                                while i < next {
                                    let e2 = self.gee(i, nod);
                                    if e2 <= best {
                                        break;
                                    }
                                    best = e2;
                                    i += 1;
                                }
                                self.ext[k] = i - 1;
                                res8 = 1;
                                break;
                            }
                        }
                    }
                    last_new = self.ext[k] as isize;
                }
            }

            if k == 0 {
                first_err = best;
            }
            last_best = best;
        }

        // End repair.
        if self.ext[0] < l18 {
            l18 = self.ext[0];
        }
        if self.ext[r] > l10 {
            l10 = self.ext[r];
        }
        let mut repair = 0u8;

        // The head scan compares against whatever the last sweep left in the
        // running best, not against the first extremum's error (0x180002ff7).
        let head_thr = last_best * CONV_MARGIN;
        let mut i = 0usize;
        while (i as isize) < l18 as isize {
            let e = self.gee(i, nod_orig);
            if e > head_thr {
                let mut best = e;
                let mut j = i + 1;
                if (j as isize) < l18 as isize {
                    while (j as isize) < l18 as isize {
                        let e2 = self.gee(j, nod_orig);
                        if e2 <= best {
                            break;
                        }
                        best = e2;
                        j += 1;
                    }
                }
                l18 = j - 1;
                self.ext[r + 1] = l18;
                repair = 1;
                if best > first_err {
                    first_err = best;
                }
                res8 += 1;
                break;
            }
            i += 1;
        }

        let tail_nod = -nod;
        let mut thr = first_err * CONV_MARGIN;
        let mut i = self.ngrid as isize - 1;
        while i > l10 as isize {
            let e = self.gee(i as usize, tail_nod);
            if e > thr {
                let mut best = e;
                let mut j = i - 1;
                while j > l10 as isize {
                    let e2 = self.gee(j as usize, tail_nod);
                    if e2 <= best {
                        break;
                    }
                    best = e2;
                    j -= 1;
                }
                // The new last extremum goes into the spare slot and the
                // left shift below brings it into place (0x18000318c).
                l10 = self.ext[r + 1];
                repair = 2;
                self.ext[r + 1] = (j + 1) as usize;
                res8 += 1;
                // The tail threshold becomes the error just found, not a
                // margin around it (0x180003137).
                thr = best;
            }
            i -= 1;
        }

        if repair == 1 {
            for k in (1..=r).rev() {
                self.ext[k] = self.ext[k - 1];
            }
            self.ext[0] = l18;
        } else if repair == 2 {
            for k in 0..=r {
                self.ext[k] = self.ext[k + 1];
            }
        }

        res8
    }

    /// `CFIR::invfft`: sample the equiripple response at `r` points and take
    /// the cosine series back to an impulse response.
    fn invfft(&mut self, full: bool) {
        let r = self.r;
        let inv = 1.0 / (2.0 * r as f64 - 1.0);
        // The sentinel the original writes past the last extremal cosine.
        self.x[r + 1] = -2.0;

        let uniform = r < 4 || full;
        let (mut wa, mut wb) = (0.0f64, 0.0f64);
        if !uniform {
            let c0 = (self.grid[0] * TWO_PI).cos();
            let c1 = (self.grid[self.ngrid - 1] * TWO_PI).cos();
            wa = 2.0 / (c0 - c1);
            wb = (c1 + c0) * (-1.0 / (c0 - c1));
        }

        let saved = self.grid[0];
        let mut amp = vec![0.0f64; r];
        let mut idx = 0usize;
        for (i, a) in amp.iter_mut().enumerate() {
            let mut freq = i as f64 * inv;
            let mut c = (freq * TWO_PI).cos();
            if !uniform {
                c = (c - wb) / wa;
                freq = c.acos() / TWO_PI;
            }
            while idx < r && c - self.x[idx] < -GRID_EPS {
                idx += 1;
            }
            let idx_c = idx.min(r);
            if (c - self.x[idx_c]).abs() < GRID_EPS {
                *a = self.y[idx_c];
            } else {
                self.grid[0] = freq;
                *a = self.gee_no_err(0, r + 1);
            }
            if idx > 0 {
                idx -= 1;
            }
        }
        self.grid[0] = saved;

        for i in 0..r {
            let mut acc = 0.0f64;
            for u in 1..r {
                acc += (u as f64 * i as f64 * inv * TWO_PI).cos() * amp[u];
            }
            self.h[i] = acc * 2.0 + amp[0];
        }
        for (i, hv) in self.h.iter_mut().enumerate().take(r) {
            if i == 0 {
                *hv *= inv;
            } else {
                *hv *= 2.0 * inv;
            }
        }
        if r < 4 {
            self.h[r] = 0.0;
            if r + 1 < self.h.len() {
                self.h[r + 1] = 0.0;
            }
        }
    }

    /// `CFIR::Impulse`: fold the impulse response into the symmetric half of
    /// the coefficient array.
    fn impulse(&mut self) {
        let r = self.r;
        if !self.diff {
            if self.odd {
                for i in 0..r - 1 {
                    self.coeff[i] = self.h[r - 1 - i] * 0.5;
                }
                self.coeff[r - 1] = self.h[0];
            } else {
                self.coeff[0] = self.h[r - 1] * 0.25;
                let mut i = 1;
                while i < r - 1 {
                    self.coeff[i] = (self.h[r - i - 1] + self.h[r - i]) * 0.25;
                    i += 1;
                }
                self.coeff[r - 1] = self.h[1] * 0.25 + self.h[0] * 0.5;
            }
        } else if self.odd {
            self.coeff[0] = self.h[r - 1] * 0.25;
            self.coeff[1] = self.h[r - 2] * 0.25;
            let mut i = 2;
            while i < r - 1 {
                self.coeff[i] = (self.h[r - i - 1] - self.h[r - i + 1]) * 0.25;
                i += 1;
            }
            self.coeff[r - 1] = self.h[0] * 0.5 - self.h[2] * 0.25;
            self.coeff[r] = 0.0;
        } else {
            self.coeff[0] = self.h[r - 1] * 0.25;
            let mut i = 1;
            while i < r - 1 {
                self.coeff[i] = (self.h[r - i - 1] - self.h[r - i]) * 0.25;
                i += 1;
            }
            self.coeff[r - 1] = self.h[0] * 0.5 - self.h[1] * 0.25;
        }
    }

    /// `CFIR::GetCoeff`: the array is stored as its first half and mirrored.
    fn get_coeff(&self, i: usize) -> f64 {
        if i < self.r {
            return self.coeff[i];
        }
        if (i == self.r && self.diff && self.odd) || i >= self.nfilt {
            return 0.0;
        }
        let idx = self.nfilt as isize - i as isize - 1;
        if idx < 0 {
            return 0.0;
        }
        let v = self.coeff[idx as usize];
        if self.diff {
            -v
        } else {
            v
        }
    }
}

/// Design a filter the way `CFIR::fircalc` does.
///
/// `bands` must be in ascending order and inside `0.0..=0.5`. `lgrid` is the
/// points-per-half-sample-step of the exchange grid; `FIRWin.s2l` passes 16 for
/// a low or high pass, 9 for a band filter and 31 for a differentiator.
/// `retry` asks for the 16 → 9 → 31 ladder Spike2 falls back through when the
/// exchange fails to converge on the first grid.
pub fn design(
    nfilt: usize,
    jtype: DesignType,
    bands: &[Band],
    lgrid: i32,
    retry: bool,
) -> Result<Design, DesignError> {
    if nfilt < 3 || nfilt > MAX_COEFS {
        return Err(DesignError::OutOfRange);
    }
    for (i, b) in bands.iter().enumerate() {
        let bad = b.low < 0.0
            || b.high <= b.low
            || b.high > 0.5
            || (i > 0 && b.low < bands[i - 1].high);
        if bad {
            return Err(DesignError::BadBand { index: i });
        }
    }

    let mut cfm = Cfm::new(nfilt, jtype.jtype());
    let grids = [lgrid, lgrid / 2 + 1, lgrid * 2 - 1];
    let tries = if retry { 3 } else { 1 };

    let mut dev = 0.0f64;
    for g in grids.iter().take(tries) {
        dev = cfm.docalc(bands, *g);
        if !(dev >= 0.0 && cfm.err & 1 != 0) {
            break;
        }
    }
    if dev < 0.0 {
        return Err(DesignError::NoConvergence);
    }

    let coeffs = (0..cfm.nfilt).map(|i| cfm.get_coeff(i)).collect();
    let extrema = (0..cfm.r).map(|k| cfm.grid[cfm.ext[k]]).collect();
    Ok(Design { coeffs, dev, extrema })
}

/// The band table `FIRQuick` builds for a type, a corner frequency and a
/// transition width, all as fractions of the sampling rate.
///
/// `FIRWin.s2l` 0x180003d30. The stop bands carry a weight of 10 and the
/// band filters leave a 0.02 gap either side of the transition so the exchange
/// is free there.
pub fn quick_bands(t: DesignType, freq: f64, width: f64) -> Vec<Band> {
    let half = width * 0.5;
    match t {
        DesignType::LowPass => vec![
            Band::new(0.0, freq - half, 1.0, 1.0),
            Band::new(freq + half, 0.5, 0.0, 10.0),
        ],
        DesignType::HighPass => vec![
            Band::new(0.0, freq - half, 0.0, 10.0),
            Band::new(freq + half, 0.5, 1.0, 1.0),
        ],
        DesignType::BandPass => vec![
            Band::new(0.0, freq - half - 0.02, 0.0, 10.0),
            Band::new(freq - half, freq + half, 1.0, 1.0),
            Band::new(freq + half + 0.02, 0.5, 0.0, 10.0),
        ],
        DesignType::BandStop => vec![
            Band::new(0.0, freq - half - 0.02, 1.0, 1.0),
            Band::new(freq - half, freq + half, 0.0, 10.0),
            Band::new(freq + half + 0.02, 0.5, 1.0, 1.0),
        ],
        DesignType::Differentiator => vec![
            Band::new(0.0, freq, TWO_PI, 1.0),
            Band::new(freq + width, 0.5, 0.0, 10.0),
        ],
    }
}

/// The tap count `FIRWin.s2l` derives from the requested attenuation and
/// transition width, at 0x180004060.
///
/// A low or high pass uses `(atten - 23.9·width - 5.585) / (14.41·width + 0.0723)`;
/// a band filter uses a width-independent line, `1 + 2.7739·(atten - 6.063)`.
/// The result is clamped to 2047 and rounded up to odd, except for the
/// differentiator below 0.48, which keeps an even count.
pub fn quick_tap_count(t: DesignType, freq: f64, width: f64, atten: f64) -> usize {
    let odd = |n: usize| if n % 2 == 0 { n + 1 } else { n };
    let n = match t {
        DesignType::LowPass | DesignType::HighPass => {
            let d = ((atten - width * 23.9) - 5.585) / (width * 14.41 + 0.0723);
            (d as usize) + 1
        }
        DesignType::BandPass | DesignType::BandStop => {
            let d = 1.0 - ((atten - 0.478) - 5.585) * -2.773_925_104_022_191;
            d as usize
        }
        DesignType::Differentiator => {
            if freq > 0.48 {
                return 0x14;
            }
            let d = ((atten - width * 23.9) - 5.585) / (width * 14.41 + 0.0723);
            let n = (d as usize) + 1;
            if freq < 0.4 {
                return odd(n).min(MAX_COEFS);
            }
            return n.min(MAX_COEFS);
        }
    };
    odd(n).min(MAX_COEFS)
}

/// The grid size `FIRQuick` passes for a type: 2 for a low, high or
/// differentiator design, 3 for a band filter, and 1 for a differentiator
/// above 0.48, where it also takes a fixed 20 coefficients (0x180004060).
fn quick_lgrid(t: DesignType, freq: f64) -> i32 {
    match t {
        DesignType::LowPass | DesignType::HighPass => 2,
        DesignType::BandPass | DesignType::BandStop => 3,
        DesignType::Differentiator => {
            if freq > 0.48 {
                1
            } else {
                2
            }
        }
    }
}

/// `FIRQuick(coef[], type%, freq, width, atten)`: the whole script command.
///
/// `freq`, `width` and `atten` are the fractions of the sampling rate and the
/// stop-band attenuation in dB that Spike2's function takes. The returned
/// vector is the coefficient array the script would hand to `ArrFilt`.
pub fn fir_quick(
    t: DesignType,
    freq: f64,
    width: f64,
    atten: f64,
) -> Result<Design, DesignError> {
    if !(0.0..=0.5).contains(&freq) || width <= 0.0 || width > 0.1 || atten <= 0.0 {
        return Err(DesignError::OutOfRange);
    }
    let nfilt = quick_tap_count(t, freq, width, atten);
    let bands = quick_bands(t, freq, width);
    design(nfilt, t, &bands, quick_lgrid(t, freq), true)
}

/// Apply a symmetric FIR in place, as `ArrFir` in `compiler.dll` does.
///
/// The window is edge-clamped rather than zero-padded: `ArrFir` holds the
/// pointer on the first sample for the first half-window and on the last for
/// the tail, so the ends see a continuation of the end value.
pub fn arr_fir(data: &mut [f64], coef: &[f64]) {
    let nc = coef.len();
    if nc <= 1 || nc > data.len() {
        return;
    }
    let half = (nc - 1) / 2;
    let n = data.len();
    let src: Vec<f64> = data.to_vec();
    let at = |i: isize| -> f64 {
        let idx = i.clamp(0, n as isize - 1);
        src[idx as usize]
    };
    for k in 0..n {
        let mut acc = 0.0;
        for (j, c) in coef.iter().enumerate() {
            acc += c * at(k as isize + j as isize - half as isize);
        }
        data[k] = acc;
    }
}
