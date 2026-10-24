//! The C formatter Spike2 uses for `Str$`, `Print` and `Print$`: the Microsoft C runtime's
//! `printf` of the Visual C++ 2012 era (`msvcr110.dll`), not glibc's.
//!
//! The differences that show: an exponent has three digits (`1e+006`); a number is first
//! cut to 17 significant decimal digits and then rounded half up on those digits, so `2.25`
//! with `%.1f` is `2.3` and `0.5` with `%.0f` is `1` (glibc rounds the exact binary value
//! half to even); digits past the seventeenth are zeros.

/// A value for a conversion.
#[derive(Debug, Clone)]
pub enum Fv {
    Int(i64),
    Flt(f64),
    Str(String),
}

#[derive(Default, Clone, Copy)]
struct Spec {
    minus: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    prec: Option<usize>,
}

/// The 17 significant digits of |x| and the decimal exponent of the first one.
fn digits17(x: f64) -> (Vec<u8>, i32) {
    if x == 0.0 {
        return (vec![0; 17], 0);
    }
    let s = format!("{:.16e}", x.abs());
    let (m, e) = s.split_once('e').unwrap();
    let d: Vec<u8> = m.bytes().filter(|b| b.is_ascii_digit()).map(|b| b - b'0').collect();
    (d, e.parse().unwrap())
}

/// Round a digit string to `n` digits, half up. Returns the digits and whether it carried
/// into a new leading digit (the exponent then grows by one).
fn round_to(d: &[u8], n: usize) -> (Vec<u8>, bool) {
    let mut out: Vec<u8> = d.iter().copied().take(n).collect();
    while out.len() < n {
        out.push(0);
    }
    if d.len() > n && d[n] >= 5 {
        let mut i = n;
        loop {
            if i == 0 {
                out.insert(0, 1);
                out.truncate(n.max(1));
                return (out, true);
            }
            i -= 1;
            if out[i] == 9 {
                out[i] = 0;
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    (out, false)
}

fn digit_str(d: &[u8]) -> String {
    d.iter().map(|&x| (b'0' + x) as char).collect()
}

fn fmt_f(x: f64, prec: usize, alt: bool) -> String {
    let (d, e) = digits17(x);
    // Digits kept: those before the point plus `prec` after it.
    let keep = e + 1 + prec as i32;
    let (digits, carried, e) = if keep < 0 {
        (Vec::new(), false, e)
    } else if keep == 0 {
        // Only the rounding digit decides whether the first place becomes 1.
        if d[0] >= 5 {
            (vec![1], true, e)
        } else {
            (Vec::new(), false, e)
        }
    } else {
        let (r, c) = round_to(&d, keep as usize);
        (r, c, e)
    };
    let e = if carried { e + 1 } else { e };
    let mut int_part = String::new();
    let mut frac = String::new();
    // Position i of `digits` has decimal exponent e - i (after any carry the first digit's
    // exponent is e).
    let total = digits.len() as i32;
    for pos in (0..=e.max(0)).rev() {
        let idx = e - pos;
        let ch = if idx >= 0 && idx < total { (b'0' + digits[idx as usize]) as char } else { '0' };
        int_part.push(ch);
    }
    if e < 0 {
        int_part = "0".into();
    }
    for k in 1..=prec as i32 {
        let idx = e + k;
        let ch = if idx >= 0 && idx < total { (b'0' + digits[idx as usize]) as char } else { '0' };
        frac.push(ch);
    }
    // Digits beyond the seventeenth are zero in the old runtime.
    if int_part.is_empty() {
        int_part = "0".into();
    }
    let mut s = int_part;
    if prec > 0 || alt {
        s.push('.');
        s.push_str(&frac);
    }
    s
}

fn fmt_e(x: f64, prec: usize, upper: bool, alt: bool) -> String {
    let (d, mut e) = digits17(x);
    let (r, carried) = round_to(&d, prec + 1);
    if carried {
        e += 1;
    }
    if x == 0.0 {
        e = 0;
    }
    let mut s = String::new();
    s.push((b'0' + r[0]) as char);
    if prec > 0 || alt {
        s.push('.');
    }
    s.push_str(&digit_str(&r[1..]));
    s.push(if upper { 'E' } else { 'e' });
    s.push(if e < 0 { '-' } else { '+' });
    s.push_str(&format!("{:03}", e.abs()));
    s
}

fn fmt_g(x: f64, prec: Option<usize>, upper: bool, alt: bool) -> String {
    let p = match prec {
        None => 6,
        Some(0) => 1,
        Some(p) => p,
    };
    let (d, e) = digits17(x);
    let (_, carried) = round_to(&d, p);
    let x_exp = if x == 0.0 { 0 } else if carried { e + 1 } else { e };
    let mut s = if x_exp < -4 || x_exp >= p as i32 {
        fmt_e(x, p - 1, upper, alt)
    } else {
        fmt_f(x, (p as i32 - 1 - x_exp).max(0) as usize, alt)
    };
    if !alt {
        // Strip trailing zeros of the fraction (and a bare point) before any exponent.
        let (mant, exp) = match s.find(|c| c == 'e' || c == 'E') {
            Some(i) => (s[..i].to_string(), s[i..].to_string()),
            None => (s.clone(), String::new()),
        };
        let mant = if mant.contains('.') { mant.trim_end_matches('0').trim_end_matches('.').to_string() } else { mant };
        s = mant + &exp;
    }
    s
}

fn pad(body: String, sign: &str, spec: Spec, numeric: bool) -> String {
    let len = body.chars().count() + sign.chars().count();
    if len >= spec.width {
        return format!("{sign}{body}");
    }
    let fill = spec.width - len;
    if spec.minus {
        format!("{sign}{body}{}", " ".repeat(fill))
    } else if spec.zero && numeric {
        format!("{sign}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{sign}{body}", " ".repeat(fill))
    }
}

fn as_f(v: &Fv) -> f64 {
    match v {
        Fv::Int(i) => *i as f64,
        Fv::Flt(f) => *f,
        Fv::Str(_) => 0.0,
    }
}

fn as_i(v: &Fv) -> i64 {
    match v {
        Fv::Int(i) => *i,
        Fv::Flt(f) => *f as i64,
        Fv::Str(_) => 0,
    }
}

/// Format `fmt` with `args`, consuming them in order. Returns the text and how many
/// arguments were not used.
pub fn format(fmt: &str, args: &[Fv]) -> (String, usize) {
    let mut out = String::new();
    let mut it = args.iter();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            // The script's own escapes survive into the format: \n, \t.
            match chars[i + 1] {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                o => {
                    out.push('\\');
                    out.push(o);
                }
            }
            i += 2;
            continue;
        }
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            out.push('%');
            break;
        }
        if chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        let start = i - 1;
        let mut spec = Spec::default();
        while i < chars.len() {
            match chars[i] {
                '-' => spec.minus = true,
                '+' => spec.plus = true,
                ' ' => spec.space = true,
                '#' => spec.alt = true,
                '0' => spec.zero = true,
                _ => break,
            }
            i += 1;
        }
        let mut w = 0usize;
        while i < chars.len() && chars[i].is_ascii_digit() {
            w = w * 10 + chars[i].to_digit(10).unwrap() as usize;
            i += 1;
        }
        spec.width = w;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                p = p * 10 + chars[i].to_digit(10).unwrap() as usize;
                i += 1;
            }
            spec.prec = Some(p);
        }
        if i >= chars.len() {
            out.extend(&chars[start..]);
            break;
        }
        let conv = chars[i];
        i += 1;
        let sign_of = |neg: bool| -> &'static str {
            if neg {
                "-"
            } else if spec.plus {
                "+"
            } else if spec.space {
                " "
            } else {
                ""
            }
        };
        match conv {
            'd' => {
                let Some(a) = it.next() else { out.push('*'); continue };
                if matches!(a, Fv::Str(_)) {
                    out.push('*');
                    continue;
                }
                let n = as_i(a);
                let mut body = n.unsigned_abs().to_string();
                if let Some(p) = spec.prec {
                    while body.len() < p {
                        body.insert(0, '0');
                    }
                }
                out.push_str(&pad(body, sign_of(n < 0), spec, spec.prec.is_none()));
            }
            'x' | 'X' => {
                let Some(a) = it.next() else { out.push('*'); continue };
                if matches!(a, Fv::Str(_)) {
                    out.push('*');
                    continue;
                }
                let n = as_i(a) as u64;
                let mut body = if conv == 'x' { format!("{n:x}") } else { format!("{n:X}") };
                if spec.alt && n != 0 {
                    body.insert_str(0, if conv == 'x' { "0x" } else { "0X" });
                }
                out.push_str(&pad(body, "", spec, true));
            }
            'c' => {
                let Some(a) = it.next() else { out.push('*'); continue };
                if matches!(a, Fv::Str(_)) {
                    out.push('*');
                    continue;
                }
                let ch = char::from_u32(as_i(a) as u32).unwrap_or('?');
                out.push_str(&pad(ch.to_string(), "", Spec { zero: false, ..spec }, false));
            }
            's' => {
                let Some(a) = it.next() else { out.push('*'); continue };
                // A number where a string is wanted prints as `*`.
                let mut s = match a {
                    Fv::Str(s) => s.clone(),
                    _ => "*".to_string(),
                };
                if let Some(p) = spec.prec {
                    s = s.chars().take(p).collect();
                }
                out.push_str(&pad(s, "", Spec { zero: false, ..spec }, false));
            }
            'f' | 'e' | 'E' | 'g' | 'G' => {
                let Some(a) = it.next() else { out.push('*'); continue };
                if matches!(a, Fv::Str(_)) {
                    out.push('*');
                    continue;
                }
                let x = as_f(a);
                let neg = x.is_sign_negative() && !x.is_nan();
                let body = if x.is_infinite() {
                    "1.#INF".to_string()
                } else if x.is_nan() {
                    "1.#QNAN".to_string()
                } else {
                    match conv {
                        'f' => fmt_f(x, spec.prec.unwrap_or(6), spec.alt),
                        'e' | 'E' => fmt_e(x, spec.prec.unwrap_or(6), conv == 'E', spec.alt),
                        _ => fmt_g(x, spec.prec, conv == 'G', spec.alt),
                    }
                };
                out.push_str(&pad(body, sign_of(neg), spec, x.is_finite()));
            }
            _ => {
                // Not a conversion this formatter knows: the text stays as written.
                out.extend(&chars[start..i]);
            }
        }
    }
    (out, it.count())
}

/// `Str$(x)` is `%g`; with a width `%*g`; with a width and a precision `%*.*g`.
pub fn str_dollar(x: f64, width: Option<i64>, prec: Option<i64>) -> String {
    let mut f = String::from("%");
    if let Some(w) = width {
        f.push_str(&w.to_string());
    }
    if let Some(p) = prec {
        f.push('.');
        f.push_str(&p.to_string());
    }
    f.push('g');
    format(&f, &[Fv::Flt(x)]).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(fmt: &str, a: Fv) -> String {
        format(fmt, &[a]).0
    }

    #[test]
    fn exponents_have_three_digits_and_ties_round_up() {
        assert_eq!(f("%e", Fv::Flt(12345.678)), "1.234568e+004");
        assert_eq!(f("%g", Fv::Flt(1e10)), "1e+010");
        assert_eq!(f("%5.1f|", Fv::Flt(2.25)), "  2.3|");
        assert_eq!(f("%.0f", Fv::Flt(0.5)), "1");
        assert_eq!(f("%.0f", Fv::Flt(2.5)), "3");
    }

    #[test]
    fn g_picks_the_shorter_form_and_strips_zeros() {
        assert_eq!(f("%g", Fv::Flt(1234.5678)), "1234.57");
        assert_eq!(f("%g", Fv::Flt(0.0001)), "0.0001");
        assert_eq!(f("%g", Fv::Flt(123456789.0)), "1.23457e+008");
        assert_eq!(str_dollar(0.000123, Some(12), Some(5)), "    0.000123");
        assert_eq!(str_dollar(42.0, Some(8), Some(0)), "  4e+001");
    }

    #[test]
    fn integers_flags_and_width() {
        assert_eq!(f("%-5d|", Fv::Int(42)), "42   |");
        assert_eq!(f("%05d", Fv::Int(42)), "00042");
        assert_eq!(f("%X", Fv::Int(255)), "FF");
    }
}

/// One real formatted by a C conversion (`f`, `e`, `E`, `g`, `G`) with the given flags and precision.
pub fn format_real(x: f64, conv: char, prec: Option<usize>, flags: &str) -> String {
    let spec = match prec {
        Some(p) => format!("%{flags}.{p}{conv}"),
        None => format!("%{flags}{conv}"),
    };
    format(&spec, &[Fv::Flt(x)]).0
}
