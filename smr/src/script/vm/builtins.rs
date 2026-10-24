//! The commands: what `Abs`, `Mid$`, `Cos` and the rest do.
//!
//! The command table says how a command is called; this says what it does. Each family is
//! checked against Spike2 by programs run inside it (`tools/gen_run_cases.py` writes them,
//! `tools/run_oracle.py` runs them), and where a behaviour has not been checked the code
//! says so. A command not listed here stops the run with [`Stop::Command`](super::Stop).

use super::printf::{self, Fv};
use super::{Arg, Cell, Ref, RunError, Vm, ERR_INDEX, ERR_TYPE, V};
use crate::script::exec::{as_flt, to_int};

type Out = Result<Option<Option<V>>, RunError>;

fn some(v: V) -> Out {
    Ok(Some(Some(v)))
}

fn none() -> Out {
    Ok(Some(None))
}

impl Vm {
    fn a_val(&self, a: &Arg) -> Result<V, RunError> {
        match a {
            Arg::Val(v) => Ok(v.clone()),
            Arg::Ref(r) => self.try_get(*r),
        }
    }

    fn a_str(&self, a: &Arg) -> Result<String, RunError> {
        match self.a_val(a)? {
            V::Str(s) => Ok(s),
            _ => Err(RunError(ERR_TYPE)),
        }
    }

    fn a_flt(&self, a: &Arg) -> Result<f64, RunError> {
        Ok(as_flt(self.a_val(a)?.num()?))
    }

    fn a_int(&self, a: &Arg) -> Result<i64, RunError> {
        to_int(self.a_val(a)?.num()?)
    }

    /// The array a reference names, with its element type, dimensions and cell.
    fn a_array(&self, a: &Arg) -> Option<usize> {
        match a {
            Arg::Ref(Ref::Cell(c)) if matches!(self.cells[*c], Cell::Array { .. }) => Some(*c),
            _ => None,
        }
    }

    /// Apply a real function to a scalar or, in place, to every element of an array.
    fn map_real(&mut self, a: &Arg, f: impl Fn(f64) -> f64) -> Out {
        if let Some(c) = self.a_array(a) {
            if let Cell::Array { ty, data, .. } = &mut self.cells[c] {
                for e in data.iter_mut() {
                    let x = as_flt(e.num()?);
                    *e = match ty {
                        crate::script::compile::Ty::Int => V::Int(to_int(crate::script::exec::Value::Flt(f(x)))?),
                        _ => V::Flt(f(x)),
                    };
                }
            }
            return some(V::Int(0));
        }
        some(V::Flt(f(self.a_flt(a)?)))
    }

    /// The text `Print$` returns and `Print` writes: the first argument is a format.
    /// Arguments the format does not use follow it, and then a newline.
    fn print_text(&self, args: &[Arg]) -> Result<String, RunError> {
        let mut vals: Vec<Fv> = Vec::new();
        for a in args {
            vals.push(match self.a_val(a)? {
                V::Int(i) => Fv::Int(i),
                V::Flt(f) => Fv::Flt(f),
                V::Str(s) => Fv::Str(s),
            });
        }
        let Some(Fv::Str(fmt)) = vals.first().cloned() else {
            return Err(RunError(ERR_TYPE));
        };
        let (mut text, left) = printf::format(&fmt, &vals[1..]);
        if left > 0 {
            for v in &vals[vals.len() - left..] {
                match v {
                    Fv::Int(i) => text.push_str(&i.to_string()),
                    Fv::Flt(f) => text.push_str(&printf::str_dollar(*f, None, None)),
                    Fv::Str(s) => text.push_str(s),
                }
            }
            text.push('\n');
        }
        Ok(text)
    }

    /// Run a command; `Ok(None)` if it is not ported yet.
    pub(super) fn builtin(&mut self, name: &str, variant: usize, args: &[Arg]) -> Out {
        let _ = variant;
        match name {
            // ---- strings -------------------------------------------------------------
            "Len" => {
                if let Some(c) = self.a_array(&args[0]) {
                    if let Cell::Array { dims, .. } = &self.cells[c] {
                        // Spike2 refuses `Len` of an array with more than one dimension.
                        if dims.len() != 1 || args.len() != 1 {
                            return Err(RunError(ERR_TYPE));
                        }
                        return some(V::Int(dims[0] as i64));
                    }
                }
                some(V::Int(self.a_str(&args[0])?.chars().count() as i64))
            }
            "Asc" => {
                let s = self.a_str(&args[0])?;
                some(V::Int(s.chars().next().map(|c| c as i64).unwrap_or(0)))
            }
            "Chr$" => {
                let n = self.a_int(&args[0])?;
                some(V::Str(char::from_u32(n as u32).map(String::from).unwrap_or_default()))
            }
            "Left$" => {
                let s = self.a_str(&args[0])?;
                let n = self.a_int(&args[1])?.max(0) as usize;
                some(V::Str(s.chars().take(n).collect()))
            }
            "Right$" => {
                let s = self.a_str(&args[0])?;
                let n = self.a_int(&args[1])?.max(0) as usize;
                let len = s.chars().count();
                some(V::Str(s.chars().skip(len.saturating_sub(n)).collect()))
            }
            "Mid$" => {
                let s = self.a_str(&args[0])?;
                let pos = self.a_int(&args[1])?;
                let n = if args.len() > 2 { self.a_int(&args[2])?.max(0) as usize } else { usize::MAX };
                let start = (pos.max(1) - 1) as usize;
                some(V::Str(s.chars().skip(start).take(n).collect()))
            }
            "DelStr$" => {
                let s = self.a_str(&args[0])?;
                let pos = self.a_int(&args[1])?;
                let n = self.a_int(&args[2])?.max(0) as usize;
                let start = (pos.max(1) - 1) as usize;
                some(V::Str(
                    s.chars()
                        .enumerate()
                        .filter(|(i, _)| *i < start || *i >= start + n)
                        .map(|(_, c)| c)
                        .collect(),
                ))
            }
            "InStr" => {
                let s: Vec<char> = self.a_str(&args[0])?.chars().collect();
                let f: Vec<char> = self.a_str(&args[1])?.chars().collect();
                let from = if args.len() > 2 { (self.a_int(&args[2])?.max(1) - 1) as usize } else { 0 };
                let mut found = 0;
                if f.is_empty() {
                    found = 1;
                } else if f.len() <= s.len() {
                    for i in from..=(s.len() - f.len()) {
                        if s[i..i + f.len()] == f[..] {
                            found = i + 1;
                            break;
                        }
                    }
                }
                some(V::Int(found as i64))
            }
            "LCase$" => some(V::Str(self.a_str(&args[0])?.to_lowercase())),
            "UCase$" => some(V::Str(self.a_str(&args[0])?.to_uppercase())),
            "Val" => {
                let s = self.a_str(&args[0])?;
                some(V::Flt(parse_leading_number(&s)))
            }
            "Str$" => {
                let x = self.a_flt(&args[0])?;
                let w = if args.len() > 1 { Some(self.a_int(&args[1])?) } else { None };
                let p = if args.len() > 2 { Some(self.a_int(&args[2])?) } else { None };
                some(V::Str(printf::str_dollar(x, w, p)))
            }
            "Print$" => some(V::Str(self.print_text(args)?)),
            "Print" => {
                let t = self.print_text(args)?;
                self.log.push_str(&t);
                some(V::Int(0))
            }
            // ---- real functions ------------------------------------------------------
            "Abs" => {
                if let Some(_) = self.a_array(&args[0]) {
                    return self.map_real(&args[0], f64::abs);
                }
                match self.a_val(&args[0])? {
                    V::Int(i) => some(V::Int(i.wrapping_abs())),
                    v => some(V::Flt(as_flt(v.num()?).abs())),
                }
            }
            "Sqrt" => self.map_real(&args[0], f64::sqrt),
            "Sin" => self.map_real(&args[0], f64::sin),
            "Cos" => self.map_real(&args[0], f64::cos),
            "Tan" => self.map_real(&args[0], f64::tan),
            "Sinh" => self.map_real(&args[0], f64::sinh),
            "Cosh" => self.map_real(&args[0], f64::cosh),
            "Tanh" => self.map_real(&args[0], f64::tanh),
            "Exp" => self.map_real(&args[0], f64::exp),
            "Ln" => self.map_real(&args[0], f64::ln),
            "Log" => self.map_real(&args[0], f64::log10),
            "Trunc" => self.map_real(&args[0], |x| f64::trunc(x) + 0.0),
            "Floor" => self.map_real(&args[0], f64::floor),
            "Ceil" => self.map_real(&args[0], |x| f64::ceil(x) + 0.0),
            "Frac" => self.map_real(&args[0], f64::fract),
            "Round" => self.map_real(&args[0], |x| x.round()),
            "ATan" if args.len() == 1 => self.map_real(&args[0], f64::atan),
            "ATan" => {
                let (y, x) = (self.a_flt(&args[0])?, self.a_flt(&args[1])?);
                some(V::Flt(y.atan2(x)))
            }
            "Pow" => {
                let (x, y) = (self.a_flt(&args[0])?, self.a_flt(&args[1])?);
                some(V::Flt(x.powf(y)))
            }
            "Max" | "Min" => {
                let max = name == "Max";
                // Given one array, Spike2 returns the index of its first largest (or
                // smallest) element, not the value.
                if args.len() == 1 {
                    if let Some(c) = self.a_array(&args[0]) {
                        if let Cell::Array { data, .. } = &self.cells[c] {
                            let mut best = 0usize;
                            for (i, e) in data.iter().enumerate().skip(1) {
                                let (x, m) = (as_flt(e.num()?), as_flt(data[best].num()?));
                                if (max && x > m) || (!max && x < m) {
                                    best = i;
                                }
                            }
                            return some(V::Int(best as i64));
                        }
                    }
                }
                let mut vals: Vec<f64> = Vec::new();
                for a in args {
                    vals.push(self.a_flt(a)?);
                }
                let mut it = vals.into_iter();
                let first = it.next().ok_or(RunError(ERR_INDEX))?;
                some(V::Flt(it.fold(first, |m, x| if max { m.max(x) } else { m.min(x) })))
            }
            _ => Ok(None),
        }
    }
}

/// The number at the start of a string, as `Val` reads it; 0 if there is none.
fn parse_leading_number(s: &str) -> f64 {
    let t = s.trim_start();
    let mut end = 0;
    let b = t.as_bytes();
    let mut seen_digit = false;
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        seen_digit = true;
        end = i;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            seen_digit = true;
            end = i;
        }
    }
    if seen_digit && i < b.len() && (b[i] == b'e' || b[i] == b'E') {
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
