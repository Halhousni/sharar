//! Special functions of SpikeUtl.dll, which `LnGamma`, `GammaP`, `GammaQ`, `BetaI` and
//! `BinomialC` call: the DLL's own routines, translated from its machine code by
//! `tools/lift_spikeutl.py` (they are Boost.Math's, and the last bits depend on every step).

pub use super::spikeutl_gen::{beta, beta_i, binomial_c, gamma_p, gamma_q, ln_gamma};

/// A function by name, for the tests that compare against the DLL.
pub fn call(name: &str, a: f64, b: f64, c: f64) -> Option<f64> {
    Some(match name {
        "LnGamma" => ln_gamma(a),
        "GammaP" => gamma_p(a, b),
        "GammaQ" => gamma_q(a, b),
        "Beta" => beta(a, b),
        "BetaI" => beta_i(a, b, c),
        "BinomialC" => binomial_c(a as i32, b as i32),
        _ => return None,
    })
}
