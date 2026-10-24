//! Commands that run Spike2's own code: `s2lift::commands` holds each command's `Execute`
//! translated from `runtime.s2l`, with the routines of compiler.dll and SpikeUtl.dll it calls.
//! What is left for the host is `CRunInfo`, the script's side of a call: its arguments, its
//! result and the arrays it was given (this file), and the windows and data files (host.rs).
//!
//! A script array is copied into the machine's memory in the layout `CArrObj` expects, the
//! translated code works on it there, and the result is copied back when the command ends.

use super::{Arg, Cell, Ref, RunError, Vm, V};
use crate::script::compile::Ty;
use s2lift::{commands, mach::M, Svc};
use std::panic::{catch_unwind, panic_any, AssertUnwindSafe};

/// What translated code throws with `SetArgError` and the like: Spike2's error code.
struct Thrown(i32);

/// Raised when translated code reaches something not translated (a service the host lacks).
pub const ERR_LIFTED: i32 = -7002;

/// A script array copied into the machine.
struct Bound {
    cell: usize,
    addr: u64,
    ty: Ty,
    dims: Vec<usize>,
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
        let rdx = m.r[2];
        let r8 = m.r[8];
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
            "?GetInt@CRunInfo@@QEBA_JH@Z" => {
                let v = match self.vm.a_int(self.arg(int_arg(rdx))) {
                    Ok(v) => v,
                    Err(e) => panic_any(Thrown(e.0)),
                };
                m.r[0] = v as u64;
            }
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
            other => {
                self.missing = Some(other.to_string());
                panic!("the service {other} is not provided");
            }
        }
    }
}

impl<'v> Run<'v> {
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
        m.r[1] = 0; // the command object: not used
        m.r[2] = info;
        let mut run = Run { vm: self, args, result: None, bound: Vec::new(), missing: None };
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            commands::run(name, &mut m, &mut run);
        }));
        std::panic::set_hook(quiet);
        match outcome {
            Ok(()) => {
                run.copy_back(&m);
                Ok(Some(run.result.take()))
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
