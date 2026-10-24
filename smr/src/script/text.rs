//! Reading values out of text: what `ReadStr` and `Read` do.
//!
//! A line is cut into tokens at delimiters and each target takes one: a number parses the
//! front of its token, a string takes what is left of the line. The delimiters, the
//! characters skipped before a token and the quote characters are set by `ReadSetup`. All of
//! this is as Spike2 does it, found by feeding it awkward lines (`tools/gen_run_cases.py`,
//! `g_readstr_*`).

use super::compile::Ty;
use super::exec::RunError;
use super::host::Mem;
use super::vm::{Arg, V};

/// `ReadSetup`'s state.
#[derive(Debug, Clone)]
pub struct ReadCfg {
    /// Characters that end a token and are consumed with it.
    pub delims: String,
    /// Characters skipped before a token, which also end one.
    pub skip: String,
    pub quote_open: String,
    pub quote_close: String,
}

impl Default for ReadCfg {
    fn default() -> Self {
        ReadCfg { delims: ",\t".into(), skip: " ".into(), quote_open: "\"".into(), quote_close: "\"".into() }
    }
}

/// How a target takes values.
pub struct Target {
    pub ty: Ty,
    /// `Some(n)` for an array of n elements.
    pub array_len: Option<usize>,
}

/// An integer from the front of a token: optional sign, then decimal digits or `0x` and hex
/// digits; nothing numeric gives 0.
pub fn parse_int(t: &str) -> i64 {
    let b = t.as_bytes();
    let mut i = 0;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let v = if b.len() >= i + 2 && b[i] == b'0' && (b[i + 1] == b'x' || b[i + 1] == b'X') {
        let mut v: i64 = 0;
        let mut j = i + 2;
        while j < b.len() && b[j].is_ascii_hexdigit() {
            v = v.wrapping_mul(16).wrapping_add((b[j] as char).to_digit(16).unwrap() as i64);
            j += 1;
        }
        v
    } else {
        let mut v: i64 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add((b[i] - b'0') as i64);
            i += 1;
        }
        v
    };
    if neg {
        v.wrapping_neg()
    } else {
        v
    }
}

/// A real from the front of a token: sign, digits, a point, digits, an exponent. Hex and
/// words such as `nan` are not numbers here.
pub fn parse_real(t: &str) -> f64 {
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0.0;
    }
    let mut end = i;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            end = j;
        }
    }
    t[..end].parse().unwrap_or(0.0)
}

/// Fill the targets from `line`; returns how many tokens were read. A target with no token
/// left is set to zero or the empty string.
pub fn read_values(line: &str, cfg: &ReadCfg, args: &[Arg], mem: &mut dyn Mem) -> Result<i64, RunError> {
    let chars: Vec<char> = line.chars().collect();
    let mut pos = 0usize;
    let mut count = 0i64;
    let is_skip = |c: char| cfg.skip.contains(c);
    let is_delim = |c: char| cfg.delims.contains(c);
    let mut exhausted = false;
    for arg in args {
        let Some(t) = mem.target(arg) else { return Err(RunError(super::vm::ERR_TYPE)) };
        let n = t.array_len.unwrap_or(1);
        for k in 0..n {
            let mut value = match t.ty {
                Ty::Int => V::Int(0),
                Ty::Float => V::Flt(0.0),
                Ty::Str => V::Str(String::new()),
            };
            if !exhausted {
                while pos < chars.len() && is_skip(chars[pos]) {
                    pos += 1;
                }
                if pos >= chars.len() {
                    exhausted = true;
                } else if t.ty == Ty::Str {
                    // A quoted string runs to its closing quote; otherwise a string takes the
                    // rest of the line.
                    let q = cfg.quote_open.chars().position(|c| c == chars[pos]);
                    let text = match q.and_then(|i| cfg.quote_close.chars().nth(i)) {
                        Some(close) => {
                            let start = pos + 1;
                            let end = chars[start..].iter().position(|&c| c == close).map(|e| start + e);
                            let s: String = chars[start..end.unwrap_or(chars.len())].iter().collect();
                            pos = end.map(|e| e + 1).unwrap_or(chars.len());
                            if pos < chars.len() && is_delim(chars[pos]) {
                                pos += 1;
                            }
                            s
                        }
                        None => {
                            let s: String = chars[pos..].iter().collect();
                            pos = chars.len();
                            s
                        }
                    };
                    value = V::Str(text);
                    count += 1;
                } else {
                    let start = pos;
                    while pos < chars.len() && !is_delim(chars[pos]) && !is_skip(chars[pos]) {
                        pos += 1;
                    }
                    let tok: String = chars[start..pos].iter().collect();
                    // Spaces may stand between a token and the delimiter that ends it.
                    while pos < chars.len() && is_skip(chars[pos]) {
                        pos += 1;
                    }
                    if pos < chars.len() && is_delim(chars[pos]) {
                        pos += 1;
                    }
                    value = match t.ty {
                        Ty::Int => V::Int(parse_int(&tok)),
                        _ => V::Flt(parse_real(&tok)),
                    };
                    count += 1;
                }
            }
            match arg {
                Arg::Ref(r) if t.array_len.is_none() => mem.store(*r, value)?,
                _ => mem.store_element(arg, k, value)?,
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_take_the_front_of_a_token() {
        assert_eq!(parse_int("12abc"), 12);
        assert_eq!(parse_int("3.7"), 3);
        assert_eq!(parse_int("0x10"), 16);
        assert_eq!(parse_int("--5"), 0);
        assert_eq!(parse_int(".5"), 0);
        assert_eq!(parse_int("-5"), -5);
    }

    #[test]
    fn reals_take_the_front_of_a_token() {
        assert_eq!(parse_real("1e3"), 1000.0);
        assert_eq!(parse_real("1e"), 1.0);
        assert_eq!(parse_real("-.5e1"), -5.0);
        assert_eq!(parse_real("0x10"), 0.0);
        assert_eq!(parse_real("nan"), 0.0);
        assert!(parse_real("1e400").is_infinite());
    }
}
