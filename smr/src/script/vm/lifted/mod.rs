//! Commands that run Spike2's own code: `s2lift::commands` holds each command's `Execute`
//! translated from `runtime.s2l`, with the routines of compiler.dll and SpikeUtl.dll it calls.
//! What is left for the host is `CRunInfo`, the script's side of a call: its arguments, its
//! result and the arrays it was given (this file), and the windows and data files (host.rs).
//!
//! A script array is copied into the machine's memory in the layout `CArrObj` expects, the
//! translated code works on it there, and the result is copied back when the command ends.

use super::{Arg, Cell, Ref, RunError, Vm, V};
mod cstring;
mod win;
use crate::script::compile::Ty;
use s2lift::{commands, mach::M, Svc};
use std::panic::{catch_unwind, panic_any, AssertUnwindSafe};

/// What translated code throws with `SetArgError` and the like: Spike2's error code.
struct Thrown(i32);

/// Raised when translated code reaches something not translated (a service the host lacks).
pub const ERR_LIFTED: i32 = -7002;

/// A script array copied into the machine.
#[derive(Clone)]
struct Bound {
    cell: usize,
    addr: u64,
    ty: Ty,
    dims: Vec<usize>,
}

impl Bound {
    fn clone_shape(&self) -> (u64, Vec<usize>) {
        (self.addr, self.dims.clone())
    }
}

fn elem_size(ty: Ty) -> Option<usize> {
    match ty {
        Ty::Int | Ty::Float => Some(8),
        Ty::Str => None,
    }
}

struct Run<'v> {
    vm: &'v mut Vm,
    args: &'v [Arg],
    result: Option<V>,
    bound: Vec<Bound>,
    /// The last service the translated code asked for that the host does not have.
    missing: Option<String>,
    strs: cstring::State,
    win: win::Win,
    /// String arguments the code was given to change: argument number and the holder it edits.
    holders: Vec<(i32, u64)>,
}

impl<'v> Run<'v> {
    fn arg(&self, idx: i32) -> &Arg {
        if idx < 1 || idx as usize > self.args.len() {
            panic_any(Thrown(-0x1b75));
        }
        &self.args[idx as usize - 1]
    }

    /// Put script array `idx` into the machine as a `CArrObj` at `obj`.
    fn fill_in_array(&mut self, m: &mut M, idx: i32, obj: u64) {
        let Some(c) = self.vm.a_array(self.arg(idx)) else { panic_any(Thrown(-0x1b7e)) };
        let Cell::Array { ty, dims, data } = &self.vm.cells[c] else { unreachable!() };
        let (ty, dims) = (*ty, dims.clone());
        let Some(es) = elem_size(ty) else { panic_any(Thrown(-0x1b7e)) };
        // One copy per array: a command may fill the same argument in twice.
        let addr = match self.bound.iter().find(|b| b.cell == c) {
            Some(b) => b.addr,
            None => {
                let addr = m.alloc((data.len() * es).max(8));
                for (i, v) in data.iter().enumerate() {
                    let bits = match v {
                        V::Int(n) => *n as u64,
                        V::Flt(f) => f.to_bits(),
                        V::Str(_) => 0,
                    };
                    m.st(addr + (i * es) as u64, 8, bits);
                }
                self.bound.push(Bound { cell: c, addr, ty, dims: dims.clone() });
                addr
            }
        };
        self.write_obj(m, obj, ty, &dims, addr);
    }

    /// The fields of a `CArrObj` that the translated code reads: the type description (type,
    /// number of dimensions, size of each), the stride of each dimension in bytes, the data.
    fn write_obj(&self, m: &mut M, obj: u64, ty: Ty, dims: &[usize], addr: u64) {
        let vt = match ty {
            Ty::Int => 1,
            Ty::Float => 2,
            Ty::Str => 3,
        };
        m.st(obj + 0x08, 8, 0);
        m.st(obj + 0x10, 4, vt);
        m.st(obj + 0x14, 2, dims.len() as u64);
        for k in 0..5 {
            m.st(obj + 0x18 + 4 * k as u64, 4, dims.get(k).copied().unwrap_or(0) as u64);
        }
        let mut stride = 8usize;
        for k in (0..dims.len()).rev() {
            m.st(obj + 0x2c + 4 * k as u64, 4, stride as u64);
            stride *= dims[k];
        }
        m.st(obj + 0x40, 8, addr);
        m.st(obj + 0x48, 8, 0);
        m.st(obj + 0x50, 4, 0xffff_ffff);
        m.st(obj + 0x88, 2, 0);
    }

    fn copy_back(&mut self, m: &M) {
        for (idx, holder) in std::mem::take(&mut self.holders) {
            let text = cstring::from_units(&self.strs.read(m, holder, true));
            if let Arg::Ref(r) = self.args[idx as usize - 1] {
                let _ = self.vm.set(r, V::Str(text));
            }
        }
        for b in &self.bound {
            if let Cell::Array { data, dims, .. } = &mut self.vm.cells[b.cell] {
                let n: usize = b.dims.iter().product();
                *dims = b.dims.clone();
                *data = (0..n)
                    .map(|i| {
                        let bits = m.ld(b.addr + 8 * i as u64, 8);
                        match b.ty {
                            Ty::Int => V::Int(bits as i64),
                            _ => V::Flt(f64::from_bits(bits)),
                        }
                    })
                    .collect();
            }
        }
    }
}

impl<'v> Svc for Run<'v> {
    fn import(&mut self, m: &mut M, id: u32) {
        let name = commands::IMPORT_NAMES[id as usize];
        if std::env::var_os("S2LIFT_SERVICES").is_some() {
            eprintln!("service {name} rcx={:#x} rdx={:#x} r8={:#x} r9={:#x}", m.r[1], m.r[2], m.r[8], m.r[9]);
        }
        let rdx = m.r[2];
        let r8 = m.r[8];
        let r9 = m.r[9];
        let int_arg = |v: u64| v as u32 as i32;
        let num = |r: Result<f64, RunError>| match r {
            Ok(v) => v,
            Err(e) => panic_any(Thrown(e.0)),
        };
        match name {
            "?GetFloat@CRunInfo@@QEBANH@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                m.set_xl(0, v.to_bits());
            }
            "?GetInt@CRunInfo@@QEBA_JH@Z" => m.r[0] = self.int_arg(int_arg(rdx)) as u64,
            "?GetIntErr@CRunInfo@@QEBA_JH_J0@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if !(r8 as i64 <= v && v <= m.r[9] as i64) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as u64;
            }
            "?GetIntLim@CRunInfo@@QEBA_JH_J0@Z" => {
                let v = self.int_arg(int_arg(rdx));
                m.r[0] = v.clamp(r8 as i64, (m.r[9] as i64).max(r8 as i64)) as u64;
            }
            "?GetI32@CRunInfo@@QEBAHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if v < i32::MIN as i64 || v > i32::MAX as i64 {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as i32 as u32 as u64;
            }
            "?GetI32Err@CRunInfo@@QEBAHHHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                if !(int_arg(r8) as i64 <= v && v <= int_arg(m.r[9]) as i64) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_INT));
                }
                m.r[0] = v as i32 as u32 as u64;
            }
            "?GetI32Lim@CRunInfo@@QEBAHHHH@Z" => {
                let v = self.int_arg(int_arg(rdx));
                let (lo, hi) = (int_arg(r8) as i64, int_arg(m.r[9]) as i64);
                // as the DLL does it: below lo gives lo; otherwise above hi gives hi
                let r = if lo <= v { if hi < v { hi } else { v } } else { lo };
                m.r[0] = r as i32 as u32 as u64;
            }
            "?GetFloatErr@CRunInfo@@QEBANHNN@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                if !(f64::from_bits(m.xl(2)) <= v && v <= f64::from_bits(m.xl(3))) {
                    panic_any(Thrown(crate::script::vm::ERR_ARG_REAL));
                }
                m.set_xl(0, v.to_bits());
            }
            "?GetFloatLim@CRunInfo@@QEBANHNN@Z" => {
                let v = num(self.vm.a_flt(self.arg(int_arg(rdx))));
                let (lo, hi) = (f64::from_bits(m.xl(2)), f64::from_bits(m.xl(3)));
                let r = if lo <= v && v <= hi { v } else if lo <= v { hi } else { lo };
                m.set_xl(0, r.to_bits());
            }
            "?GetArgType@CRunInfo@@QEBAXHAEAVTypeDesc@@@Z" => self.arg_type(m, int_arg(rdx), r8),
            "?Resize@CRunInfo@@QEBAXHAEAVCArrObj@@H@Z" => self.resize(m, int_arg(rdx), r8, int_arg(m.r[9])),
            "?SetFloat@CRunInfo@@QEBAXHN@Z" => {
                let v = f64::from_bits(m.xl(2));
                self.set_arg(int_arg(rdx), V::Flt(v));
            }
            "?SetInt@CRunInfo@@QEBAXH_J@Z" => self.set_arg(int_arg(rdx), V::Int(r8 as i64)),
            "?StoreIntResult@CRunInfo@@QEBAX_J@Z" => self.result = Some(V::Int(rdx as i64)),
            "?StoreFloatResult@CRunInfo@@QEBAXN@Z" => self.result = Some(V::Flt(f64::from_bits(m.xl(1)))),
            "?SetArgError@CRunInfo@@QEBAXFH@Z" => panic_any(Thrown(rdx as u16 as i16 as i32)),
            "?FillInArray@CRunInfo@@QEBAXHAEAVCArrObj@@@Z" | "?FillInArray0@CRunInfo@@QEBAXHAEAVCArrObj@@@Z" => {
                self.fill_in_array(m, int_arg(rdx), r8)
            }
            "?GetCString@CRunInfo@@QEBAXHAEAV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                self.strs.write(m, r8, &units, true);
            }
            "?GetConstStringHolder@CRunInfo@@QEBAAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@H@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                let holder = m.alloc(8);
                self.strs.write(m, holder, &units, true);
                m.r[0] = holder;
            }
            "?GetStringHolder@CRunInfo@@QEBAPEAV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@H_N@Z" => {
                let idx = int_arg(rdx);
                let text = self.str_arg(idx);
                let units: Vec<u32> = text.encode_utf16().map(|u| u as u32).collect();
                let holder = m.alloc(8);
                self.strs.write(m, holder, &units, true);
                self.holders.push((idx, holder));
                m.r[0] = holder;
            }
            "?GetConstString@CRunInfo@@QEBAPEB_WH@Z" => {
                let text = self.str_arg(int_arg(rdx));
                let units: Vec<u16> = text.encode_utf16().collect();
                let p = m.alloc(units.len() * 2 + 2);
                for (i, u) in units.iter().enumerate() {
                    m.st(p + 2 * i as u64, 2, *u as u64);
                }
                m.r[0] = p;
            }
            "?SetCString@CRunInfo@@QEBAXHAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                let text = cstring::from_units(&self.strs.read(m, r8, true));
                self.set_arg(int_arg(rdx), V::Str(text));
            }
            "?StoreStringResult@CRunInfo@@QEBAXAEBV?$CStringT@_WV?$StrTraitMFC_DLL@_WV?$ChTraitsCRT@_W@ATL@@@@@ATL@@@Z" => {
                self.result = Some(V::Str(cstring::from_units(&self.strs.read(m, rdx, true))));
            }
            "?StoreStringResult@CRunInfo@@QEBAXPEB_W@Z" => {
                let mut units = Vec::new();
                while m.ld(rdx + 2 * units.len() as u64, 2) != 0 {
                    units.push(m.ld(rdx + 2 * units.len() as u64, 2) as u16);
                }
                self.result = Some(V::Str(String::from_utf16_lossy(&units)));
            }
            other if cstring::call(&mut self.strs, m, other) => {}
            other if self.win.call(m, other) => {}
            // The C++ runtime and the C library: memory is the machine's heap, which is never
            // freed during a command.
            "??2@YAPEAX_K@Z" | "??_U@YAPEAX_K@Z" => m.r[0] = m.alloc(m.r[1] as usize),
            "??3@YAXPEAX@Z" | "??_V@YAXPEAX@Z" | "free" | "?_Orphan_all@_Container_base0@std@@QEAAXXZ" => {}
            "?_Xbad_alloc@std@@YAXXZ" | "?_Xlength_error@std@@YAXPEBD@Z" | "?_Xout_of_range@std@@YAXPEBD@Z" => {
                panic_any(Thrown(-7003));
            }
            "ldexp" => {
                let x = f64::from_bits(m.xl(0));
                m.set_xl(0, scalbn(x, m.r[2] as u32 as i32).to_bits());
            }
            "frexp" => {
                let (mant, e) = frexp(f64::from_bits(m.xl(0)));
                m.st(m.r[2], 4, e as u32 as u64);
                m.set_xl(0, mant.to_bits());
            }
            "_time64" => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                if m.r[1] != 0 {
                    m.st(m.r[1], 8, now);
                }
                m.r[0] = now;
            }
            "_localtime64_s" => {
                // (struct tm *out, const __time64_t *t): local time, here the machine's zone
                let t = m.ld(rdx, 8) as i64;
                let (tm, ok) = localtime(t);
                for (i, v) in tm.iter().enumerate() {
                    m.st(m.r[1] + 4 * i as u64, 4, *v as u32 as u64);
                }
                m.r[0] = if ok { 0 } else { 22 };
            }
            "GetLocaleInfoW" => {
                // (locale, type, buffer, size): the date and time formats of a US English system
                let kind = (rdx as u32) & 0x0fff_ffff; // LOCALE_NOUSEROVERRIDE and the like are flags
                const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
                const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
                let text: String = match kind {
                    0x1f => "M/d/yyyy".into(),                   // LOCALE_SSHORTDATE
                    0x20 => "dddd, MMMM dd, yyyy".into(),        // LOCALE_SLONGDATE
                    0x1003 => "h:mm:ss tt".into(),               // LOCALE_STIMEFORMAT
                    0x1d => "/".into(),                           // LOCALE_SDATE
                    0x1e => ":".into(),                           // LOCALE_STIME
                    0x21 => "0".into(),                           // LOCALE_IDATE: month day year
                    0x22 => "0".into(),                           // LOCALE_ILDATE
                    0x23 => "0".into(),                           // LOCALE_ITIME: 12 hour clock
                    0x25 => "0".into(),                           // LOCALE_ITLZERO
                    0x28 => "AM".into(),                          // LOCALE_S1159
                    0x29 => "PM".into(),                          // LOCALE_S2359
                    0x2a..=0x30 => DAYS[(kind - 0x2a) as usize].into(),
                    0x31..=0x37 => DAYS[(kind - 0x31) as usize][..3].into(),
                    0x38..=0x43 => MONTHS[(kind - 0x38) as usize].into(),
                    0x44..=0x4f => MONTHS[(kind - 0x44) as usize][..3].into(),
                    _ => String::new(),
                };
                let units: Vec<u16> = text.encode_utf16().collect();
                if r9 > 0 {
                    for (i, u) in units.iter().enumerate() {
                        m.st(r8 + 2 * i as u64, 2, *u as u64);
                    }
                    m.st(r8 + 2 * units.len() as u64, 2, 0);
                }
                m.r[0] = units.len() as u64 + 1;
            }
            "_wtoi" => {
                let mut v = Vec::new();
                while m.ld(rcx_of(m) + 2 * v.len() as u64, 2) != 0 {
                    v.push(m.ld(rcx_of(m) + 2 * v.len() as u64, 2) as u16);
                }
                let s = String::from_utf16_lossy(&v);
                m.r[0] = crate::script::text::parse_int(s.trim_start()) as i32 as u32 as u64;
            }
            other => {
                self.missing = Some(other.to_string());
                panic!("the service {other} is not provided");
            }
        }
    }
}

impl<'v> Run<'v> {
    fn str_arg(&self, idx: i32) -> String {
        match self.vm.a_str(self.arg(idx)) {
            Ok(s) => s,
            Err(e) => panic_any(Thrown(e.0)),
        }
    }

    fn int_arg(&self, idx: i32) -> i64 {
        match self.vm.a_int(self.arg(idx)) {
            Ok(v) => v,
            Err(e) => panic_any(Thrown(e.0)),
        }
    }

    /// `GetArgType`: the type of argument `idx`, in the 28 bytes of a `TypeDesc` at `td`.
    fn arg_type(&mut self, m: &mut M, idx: i32, td: u64) {
        let (vt, dims): (u64, Vec<usize>) = match self.arg(idx).clone() {
            Arg::Val(V::Int(_)) => (1, vec![]),
            Arg::Val(V::Flt(_)) => (2, vec![]),
            Arg::Val(V::Str(_)) => (3, vec![]),
            Arg::Ref(r) => match self.vm.cells.get(match r { Ref::Cell(c) | Ref::Elem(c, _) => c }) {
                Some(Cell::Array { ty, dims, .. }) if matches!(r, Ref::Cell(_)) => {
                    (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, dims.clone())
                }
                Some(Cell::Array { ty, .. }) => (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, vec![]),
                Some(Cell::Scalar { ty, .. }) => (match ty { Ty::Int => 1, Ty::Float => 2, Ty::Str => 3 }, vec![]),
                None => (0, vec![]),
            },
        };
        // InitTypeDesc: type 2, no dimensions, every size -1; then what the argument has.
        m.st(td, 4, vt);
        m.st(td + 4, 2, dims.len() as u64);
        for k in 0..5 {
            m.st(td + 8 + 4 * k as u64, 4, dims.get(k).map(|d| *d as u64).unwrap_or(0xffff_ffff));
        }
    }

    /// `Resize`: give the last dimension of array argument `idx` the size `n`, keeping the
    /// elements that have a place in both, and refresh the `CArrObj`.
    fn resize(&mut self, m: &mut M, idx: i32, obj: u64, n: i32) {
        let Some(c) = self.vm.a_array(self.arg(idx)) else { panic_any(Thrown(-0x1b7e)) };
        let Some(k) = self.bound.iter().position(|b| b.cell == c) else { panic_any(Thrown(-0x1b7e)) };
        let old = self.bound[k].clone_shape();
        let mut dims = old.1.clone();
        let last = dims.len() - 1;
        if dims[last] as i32 == n {
            return;
        }
        dims[last] = n.max(0) as usize;
        let total: usize = dims.iter().product();
        let addr = m.alloc((total * 8).max(8));
        // copy the elements whose indices exist in both shapes
        let old_total: usize = old.1.iter().product();
        for flat in 0..old_total {
            let mut rest = flat;
            let mut idxs = vec![0usize; old.1.len()];
            for d in (0..old.1.len()).rev() {
                idxs[d] = rest % old.1[d].max(1);
                rest /= old.1[d].max(1);
            }
            if idxs.iter().zip(&dims).all(|(i, d)| i < d) {
                let to = idxs.iter().zip(&dims).fold(0, |acc, (i, d)| acc * d + i);
                let v = m.ld(old.0 + 8 * flat as u64, 8);
                m.st(addr + 8 * to as u64, 8, v);
            }
        }
        let ty = self.bound[k].ty;
        self.bound[k].addr = addr;
        self.bound[k].dims = dims.clone();
        self.write_obj(m, obj, ty, &dims, addr);
    }

    fn set_arg(&mut self, idx: i32, v: V) {
        match *self.arg(idx) {
            Arg::Ref(r) => {
                if let Err(e) = self.vm.set(r, v) {
                    panic_any(Thrown(e.0));
                }
            }
            Arg::Val(_) => panic_any(Thrown(-0x1b74)),
        }
    }
}

/// Is `name` one of the commands translated?
pub fn has(name: &str) -> bool {
    commands::COMMANDS.contains(&name)
}

impl Vm {
    /// Run a translated command. The run information the code reads directly is its argument
    /// count and the number of the variant the arguments matched.
    pub(super) fn run_lifted(&mut self, name: &str, variant: usize, args: &[Arg]) -> Result<Option<Option<V>>, RunError> {
        let mut m = commands::machine();
        let info = m.alloc(64);
        m.st(info, 4, args.len() as u64);
        m.st(info + 4, 4, variant.max(1) as u64);
        // The command object: its members start as zeros (strings in it are written before read).
        m.r[1] = m.alloc(0x400);
        m.r[2] = info;
        let mut run = Run { vm: self, args, result: None, bound: Vec::new(), missing: None, strs: Default::default(), win: Default::default(), holders: Vec::new() };
        let quiet = std::panic::take_hook();
        if std::env::var_os("S2LIFT_TRACE").is_none() {
            std::panic::set_hook(Box::new(|_| {}));
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            commands::run(name, &mut m, &mut run);
        }));
        std::panic::set_hook(quiet);
        match outcome {
            Ok(()) => {
                run.copy_back(&m);
                // A command that returns a value and did not store one gives zero.
                let mut result = run.result.take();
                if result.is_none() {
                    result = match crate::script::commands::find(name).map(|(_, c)| c.ret) {
                        Some(crate::script::commands::Ret::Int) => Some(V::Int(0)),
                        Some(crate::script::commands::Ret::Real) => Some(V::Flt(0.0)),
                        Some(crate::script::commands::Ret::Str) => Some(V::Str(String::new())),
                        _ => None,
                    };
                }
                Ok(Some(result))
            }
            Err(payload) => {
                if let Some(t) = payload.downcast_ref::<Thrown>() {
                    return Err(RunError(t.0));
                }
                let why = match payload.downcast_ref::<String>() {
                    Some(s) => s.clone(),
                    None => payload.downcast_ref::<&str>().map(|s| s.to_string()).unwrap_or_default(),
                };
                run.vm.lifted_error = Some(why);
                Err(RunError(ERR_LIFTED))
            }
        }
    }
}

/// `ldexp`: x * 2^n, rounded once.
fn scalbn(mut x: f64, mut n: i32) -> f64 {
    if x == 0.0 || !x.is_finite() || n == 0 {
        return x;
    }
    // scale in steps that cannot overflow the power of two; a subnormal result is rounded once
    // by the last multiplication
    while n > 1023 {
        x *= f64::from_bits(0x7fe0_0000_0000_0000);
        n -= 1023;
        if x.is_infinite() {
            return x;
        }
    }
    while n < -1022 {
        // stay normal until the last step so that the rounding happens once
        x *= f64::from_bits(0x0010_0000_0000_0000) * 2.0f64.powi(0);
        n += 1022;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((n + 1023) as u64) << 52)
}

/// `frexp`: the mantissa in [0.5, 1) and the exponent.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // subnormal: scale up first
        let (m, e) = frexp(x * f64::from_bits(0x4350_0000_0000_0000)); // 2^54
        return (m, e - 54);
    }
    (f64::from_bits(bits & 0x800f_ffff_ffff_ffff | 0x3fe0_0000_0000_0000), exp - 1022)
}

fn rcx_of(m: &M) -> u64 {
    m.r[1]
}

/// Broken-down local time of a Unix time, in a C `struct tm`'s order: seconds, minutes, hours, day
/// of month, month (from 0), years since 1900, weekday (Sunday 0), day of year, daylight saving.
/// The machine's own time zone, from the C library.
fn localtime(t: i64) -> ([i32; 9], bool) {
    #[repr(C)]
    struct Tm {
        f: [i32; 9],
        // glibc and others add a zone offset and name after the standard fields
        extra: [i64; 4],
    }
    let mut tm = Tm { f: [0; 9], extra: [0; 4] };
    #[cfg(unix)]
    let ok = {
        extern "C" {
            fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
        }
        unsafe { !localtime_r(&t, &mut tm).is_null() }
    };
    #[cfg(windows)]
    let ok = {
        extern "C" {
            fn _localtime64_s(out: *mut Tm, t: *const i64) -> i32;
        }
        unsafe { _localtime64_s(&mut tm, &t) == 0 }
    };
    #[cfg(not(any(unix, windows)))]
    let ok = false;
    if ok {
        return (tm.f, true);
    }
    // no C library to ask: UTC
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days);
    let wday = (days + 4).rem_euclid(7);
    let yday = days - days_from_civil(y, 1, 1);
    ([(secs % 60) as i32, (secs / 60 % 60) as i32, (secs / 3600) as i32, d as i32, mo as i32 - 1, (y - 1900) as i32, wday as i32, yday as i32, 0], true)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A host with only the operating system and the C++ runtime behind it: enough to run the data
/// file engine (`CSonFile`) without a script around it.
#[derive(Default)]
pub struct Bare {
    strs: cstring::State,
    win: win::Win,
}

impl Svc for Bare {
    fn import(&mut self, m: &mut M, id: u32) {
        let name = commands::IMPORT_NAMES[id as usize];
        match name {
            "??2@YAPEAX_K@Z" | "??_U@YAPEAX_K@Z" => m.r[0] = m.alloc(m.r[1] as usize),
            "??3@YAXPEAX@Z" | "??_V@YAXPEAX@Z" | "free" | "?_Orphan_all@_Container_base0@std@@QEAAXXZ" => {}
            other if cstring::call(&mut self.strs, m, other) => {}
            other if self.win.call(m, other) => {}
            other => panic!("the service {other} is not provided"),
        }
    }
}

impl Bare {
    /// Call an exported routine of the data file engine by its decorated name.
    pub fn call(&mut self, m: &mut M, name: &str) {
        if !commands::call_export(name, m, self) {
            panic!("{name} was not translated");
        }
    }
}
