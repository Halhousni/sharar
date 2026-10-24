//! The maths functions of msvcr110.dll, which is what Spike2's `Sin`, `Cos`, `Ln`, `Pow` and the
//! rest call. A host libm rounds differently in the last place for many arguments, so these are
//! the DLL's own routines: `tools/lift_libm.py` translates each instruction of them into a call on
//! the small machine model in `mach.rs`, and `msvcrt_gen.rs` is the result. They are checked
//! against the DLL, run under Wine, on hundreds of thousands of arguments each
//! (`tools/libm_fuzz.py`) and against Spike2 itself (`tools/run_cases/m_libm_*`).

pub use super::msvcrt_gen::{atan, atan2, cos, cosh, exp, log, log10, pow, sin, sinh, tan, tanh};

/// What the DLL answers for an infinite or NaN argument of a function that is not defined there:
/// the indefinite NaN for an infinity, the argument made quiet for a NaN.
pub(super) fn invalid(x: u64) -> f64 {
    if x & 0xf_ffff_ffff_ffff == 0 {
        f64::from_bits(0xfff8_0000_0000_0000)
    } else {
        f64::from_bits(x | 0x8_0000_0000_0000)
    }
}

/// A function by its C name, for the tests that compare against the DLL.
pub fn call(name: &str, a: f64, b: f64) -> Option<f64> {
    Some(match name {
        "sin" => sin(a),
        "cos" => cos(a),
        "tan" => tan(a),
        "exp" => exp(a),
        "log" => log(a),
        "log10" => log10(a),
        "pow" => pow(a, b),
        "atan" => atan(a),
        "atan2" => atan2(a, b),
        "sinh" => sinh(a),
        "cosh" => cosh(a),
        "tanh" => tanh(a),
        _ => return None,
    })
}
