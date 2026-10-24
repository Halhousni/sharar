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

    /// The values of an array argument, in storage order.
    fn array_vals(&self, a: &Arg) -> Result<Vec<V>, RunError> {
        let Some(c) = self.a_array(a) else { return Err(RunError(ERR_TYPE)) };
        match &self.cells[c] {
            Cell::Array { data, .. } => Ok(data.clone()),
            _ => unreachable!(),
        }
    }

    /// `ArrConst`, `ArrAdd` and the rest: the second argument, a number or an array of the
    /// same size, combined element by element into the first, in the first's own type.
    fn array_arith(&mut self, name: &str, args: &[Arg]) -> Out {
        let Some(c) = self.a_array(&args[0]) else { return Err(RunError(ERR_TYPE)) };
        let array_rhs = self.a_array(&args[1]).is_some();
        let rhs: Vec<V> = if array_rhs {
            self.array_vals(&args[1])?
        } else {
            vec![self.a_val(&args[1])?]
        };
        let Cell::Array { ty, data, .. } = &mut self.cells[c] else { unreachable!() };
        let ty = *ty;
        let scalar = rhs.len() == 1 && !array_rhs;
        for (i, e) in data.iter_mut().enumerate() {
            // An array of another size is used as far as it goes.
            if rhs.len() > 1 && i >= rhs.len() {
                break;
            }
            let r = if rhs.len() == 1 { &rhs[0] } else { &rhs[i] };
            // Into an integer array, a real scalar is cut to an integer first by add and
            // subtract but not by multiply and divide, which work in real and cut the result.
            let cut;
            let r = match (ty, r) {
                (crate::script::compile::Ty::Int, V::Flt(f)) if scalar && matches!(name, "ArrAdd" | "ArrSub" | "ArrSubR") => {
                    cut = V::Int(to_int(crate::script::exec::Value::Flt(*f))?);
                    &cut
                }
                _ => r,
            };
            *e = match name {
                "ArrConst" => conv(r, ty)?,
                "ArrAdd" => add_vals(e, r, ty)?,
                "ArrSub" => sub_vals(e, r, ty)?,
                "ArrSubR" => sub_vals(r, e, ty)?,
                "ArrMul" => mul_vals(e, r, ty)?,
                "ArrDiv" => div_vals(e, r, ty)?,
                _ => div_vals(r, e, ty)?,
            };
        }
        some(V::Int(0))
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
            // ---- arrays --------------------------------------------------------------
            "ArrConst" | "ArrAdd" | "ArrSub" | "ArrSubR" | "ArrMul" | "ArrDiv" | "ArrDivR" => {
                self.array_arith(name, args)
            }
            "ArrDot" => {
                let (a, b) = (self.array_vals(&args[0])?, self.array_vals(&args[1])?);
                if a.len() != b.len() {
                    return Err(RunError(ERR_INDEX));
                }
                let mut t = 0.0;
                for (x, y) in a.iter().zip(&b) {
                    t += as_flt(x.num()?) * as_flt(y.num()?);
                }
                some(V::Flt(t))
            }
            "ArrSum" => {
                let a = self.array_vals(&args[0])?;
                let xs: Vec<f64> = a.iter().map(|v| v.num().map(as_flt)).collect::<Result<_, _>>()?;
                let n = xs.len() as f64;
                let sum: f64 = xs.iter().sum();
                if let Some(Arg::Ref(r)) = args.get(1) {
                    self.set(*r, V::Flt(sum / n))?;
                }
                if let Some(Arg::Ref(r)) = args.get(2) {
                    let mean = sum / n;
                    let ss: f64 = xs.iter().map(|x| (x - mean) * (x - mean)).sum();
                    // The sample standard deviation: divided by n - 1.
                    self.set(*r, V::Flt(if xs.len() > 1 { (ss / (n - 1.0)).sqrt() } else { 0.0 }))?;
                }
                some(V::Flt(sum))
            }
            "ArrDiff" | "ArrIntgl" => {
                let Some(c) = self.a_array(&args[0]) else { return Err(RunError(ERR_TYPE)) };
                if let Cell::Array { ty, data, .. } = &mut self.cells[c] {
                    if name == "ArrDiff" {
                        for i in (1..data.len()).rev() {
                            data[i] = sub_vals(&data[i], &data[i - 1], *ty)?;
                        }
                    } else {
                        for i in 1..data.len() {
                            data[i] = add_vals(&data[i], &data[i - 1], *ty)?;
                        }
                    }
                }
                none()
            }
            "ArrSort" => {
                let Some(c) = self.a_array(&args[0]) else { return Err(RunError(ERR_TYPE)) };
                let mode = if args.len() > 1 { self.a_int(&args[1])? } else { 0 };
                let desc = mode & 1 != 0;
                let keys = self.array_vals(&args[0])?;
                let mut order: Vec<usize> = (0..keys.len()).collect();
                let cmp = |a: &V, b: &V| -> std::cmp::Ordering {
                    match (a, b) {
                        (V::Str(x), V::Str(y)) => x.cmp(y),
                        _ => as_flt(a.num().unwrap_or(crate::script::exec::Value::Int(0)))
                            .partial_cmp(&as_flt(b.num().unwrap_or(crate::script::exec::Value::Int(0))))
                            .unwrap_or(std::cmp::Ordering::Equal),
                    }
                };
                // A stable sort: equal keys keep their order.
                order.sort_by(|&i, &j| if desc { cmp(&keys[j], &keys[i]) } else { cmp(&keys[i], &keys[j]) });
                let n = keys.len();
                for a in std::iter::once(&args[0]).chain(args.iter().skip(2)) {
                    let Some(cc) = self.a_array(a) else { continue };
                    if let Cell::Array { data, .. } = &mut self.cells[cc] {
                        if data.len() != n {
                            continue;
                        }
                        let old = data.clone();
                        for (k, &i) in order.iter().enumerate() {
                            data[k] = old[i].clone();
                        }
                    }
                }
                let _ = c;
                some(V::Int(n as i64))
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

/// A value converted to an array's element type, as the typed setters do.
fn conv(v: &V, ty: crate::script::compile::Ty) -> Result<V, RunError> {
    use crate::script::compile::Ty;
    Ok(match (ty, v) {
        (Ty::Int, v) => V::Int(to_int(v.num()?)?),
        (Ty::Float, v) => V::Flt(as_flt(v.num()?)),
        (Ty::Str, V::Str(s)) => V::Str(s.clone()),
        _ => return Err(RunError(ERR_TYPE)),
    })
}

fn arith(
    l: &V,
    r: &V,
    ty: crate::script::compile::Ty,
    int_op: impl Fn(i64, i64) -> Result<i64, RunError>,
    flt_op: impl Fn(f64, f64) -> Result<f64, RunError>,
) -> Result<V, RunError> {
    use crate::script::compile::Ty;
    let real_operand = matches!(l, V::Flt(_)) || matches!(r, V::Flt(_));
    match ty {
        // An integer array met by a real number is worked out as real and then cut back.
        Ty::Int if real_operand => {
            let v = flt_op(as_flt(l.num()?), as_flt(r.num()?))?;
            Ok(V::Int(to_int(crate::script::exec::Value::Flt(v))?))
        }
        Ty::Int => Ok(V::Int(int_op(to_int(l.num()?)?, to_int(r.num()?)?)?)),
        Ty::Float => Ok(V::Flt(flt_op(as_flt(l.num()?), as_flt(r.num()?))?)),
        Ty::Str => Err(RunError(ERR_TYPE)),
    }
}

fn add_vals(l: &V, r: &V, ty: crate::script::compile::Ty) -> Result<V, RunError> {
    arith(l, r, ty, |a, b| Ok(a.wrapping_add(b)), |a, b| Ok(a + b))
}
fn sub_vals(l: &V, r: &V, ty: crate::script::compile::Ty) -> Result<V, RunError> {
    arith(l, r, ty, |a, b| Ok(a.wrapping_sub(b)), |a, b| Ok(a - b))
}
fn mul_vals(l: &V, r: &V, ty: crate::script::compile::Ty) -> Result<V, RunError> {
    arith(l, r, ty, crate::script::exec::int_mul, |a, b| Ok(a * b))
}
fn div_vals(l: &V, r: &V, ty: crate::script::compile::Ty) -> Result<V, RunError> {
    arith(
        l,
        r,
        ty,
        crate::script::exec::int_div,
        |a, b| crate::script::exec::flt_div_check(b).map(|_| a / b),
    )
}
