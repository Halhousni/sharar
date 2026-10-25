//! The interpreter: a program from [`compile`](super::compile) run to completion.
//!
//! `exec.rs` holds the opcodes whose vendor code was read, over numbers only. This is the
//! engine around them: strings and arrays, call frames, parameters by value and by
//! reference, and the commands. Scalar arithmetic and the loop opcodes use the same
//! helpers, so a `for` loop here steps exactly as `exec::Machine`'s does.
//!
//! Storage is an arena of cells. Globals are the first cells, in symbol order; a call
//! allocates cells for its by-value parameters, its locals and its result above them and
//! frees them on return, so a reference never outlives the cell it names. A reference is
//! a cell, or one element of an array cell.
//!
//! Every behaviour here is meant to be checked against Spike2 itself; where a behaviour
//! has not been, the code says so rather than guess (see [`Stop`]).

use super::commands;
use super::host::{Host, Mem};
use super::compile::{Compiled, Const, RoutineInfo, Ty};
use super::exec::{
    as_flt, flt_div_check, flt_truthy, int_div, int_mod, int_mul, int_result, to_int, Code,
    ForLoop, RunError, Value, ERR_OVERFLOW,
};

/// Raised for an array subscript out of range. The vendor's own code has not been read for
/// this one: the value is a placeholder until a run in Spike2 gives it.
pub const ERR_INDEX: i32 = -7000;
/// An integer argument outside the range its command allows (`GetI32Err`), and a real one
/// (`GetFloatErr`); the same codes Spike2 raises.
pub const ERR_ARG_INT: i32 = -1000;
pub const ERR_ARG_REAL: i32 = -1001;
/// A call with the wrong kind of value, e.g. a string where a number is wanted.
pub const ERR_TYPE: i32 = -7001;
/// A routine passed to a command is the number of the routine added to this.
pub(crate) const ROUTINE_ARG: i64 = 0x4000_0000_0000;

/// An argument a translated command has made ready for a script function: a value, or an array (a cell
/// holding a copy of the command's own array, which is written back when the function returns).
#[derive(Debug, Clone)]
pub(crate) enum XArg {
    Val(V),
    Arr { cell: usize, obj: u64 },
}

/// A value a script holds.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Int(i64),
    Flt(f64),
    Str(String),
}

impl V {
    fn num(&self) -> Result<Value, RunError> {
        match self {
            V::Int(i) => Ok(Value::Int(*i)),
            V::Flt(f) => Ok(Value::Flt(*f)),
            V::Str(_) => Err(RunError(ERR_TYPE)),
        }
    }
    fn from_num(v: Value) -> V {
        match v {
            Value::Int(i) => V::Int(i),
            Value::Flt(f) => V::Flt(f),
        }
    }
    fn zero(ty: Ty) -> V {
        match ty {
            Ty::Int => V::Int(0),
            Ty::Float => V::Flt(0.0),
            Ty::Str => V::Str(String::new()),
        }
    }
}

#[derive(Debug, Clone)]
enum Cell {
    Scalar { ty: Ty, v: V },
    Array { ty: Ty, dims: Vec<usize>, data: Vec<V> },
}

/// A place a value can be read from or written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ref {
    Cell(usize),
    Elem(usize, usize),
    /// The data array of a result view's channel: `View(h%, c%).[`…`]`.
    ViewArr(i64, i64),
    /// One element of it.
    ViewElem(i64, i64, i64),
}

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Val(V),
    Ref(Ref),
    Loop(ForLoop, Ref),
    /// A command's word, or a routine's, waiting for `Call`.
    Cmd(usize),
    Routine(usize),
}

#[derive(Debug, Clone, Copy)]
enum ArgSlot {
    /// A by-value parameter: a cell of its own.
    Own(usize),
    /// A reference parameter: the caller's place.
    Alias(Ref),
}

#[derive(Debug)]
struct Frame {
    routine: usize,
    args: Vec<ArgSlot>,
    locals: Vec<usize>,
    ret_cell: Option<usize>,
    /// Where to resume, and whether the call wants its value left on the stack.
    ret_ip: i32,
    want_value: bool,
    /// The arena size before the call allocated anything.
    arena_base: usize,
}

/// What stopped a run.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    Done,
    Error(i32),
    /// An opcode this engine does not execute yet.
    Unported { op: u16, at: usize },
    /// A command whose behaviour is not ported yet.
    Command(String),
    /// The caller asked the run to stop.
    Cancelled,
}

/// One argument handed to a command: a value, or a place the command may write to.
#[derive(Debug, Clone)]
pub enum Arg {
    Val(V),
    Ref(Ref),
}

mod builtins;
pub mod lifted;
pub use s2lift::{msvcrt, special};
mod printf;

pub struct Vm {
    /// Why the last translated command stopped, when it reached something not provided.
    pub lifted_error: Option<String>,
    lifted_world: Option<Box<lifted::World>>,
    code: Code,
    cells: Vec<Cell>,
    stack: Vec<Item>,
    frames: Vec<Frame>,
    prog: Compiled,
    first_word: u16,
    pool_base: u16,
    with_commands: bool,
    /// What `Print` and friends wrote.
    pub log: String,
    /// Set by a command that cannot run yet.
    pending_command: Option<String>,
    host: Option<Box<dyn Host>>,
    /// The current view when there is no host to say.
    view: i64,
    pub(crate) read_cfg: super::text::ReadCfg,
    /// One generator each for `Rand`, `RandExp` and `RandNorm`, as Spike2 keeps.
    pub(crate) rng: [builtins::CRand; 3],
    /// A script `ScriptRun` asked for: it starts when this one ends.
    pub(crate) next_script: Option<(String, i64)>,
    /// The globals of the scripts that ran before this one (`ScriptRun`).
    retired: Vec<(Vec<String>, Vec<Cell>)>,
    /// Arguments a translated command has made ready for a script function, and what the function gave back.
    pub(crate) xargs: Vec<XArg>,
    pub(crate) xret: Option<V>,
    /// The array objects the last function call left for the command to take off the stack again.
    pub(crate) xpop: Vec<u64>,
}

fn flt_to_num(v: &V) -> Result<f64, RunError> {
    Ok(as_flt(v.num()?))
}

impl Vm {
    /// Create the program's globals and set it ready to run.
    pub fn new(prog: Compiled, with_commands: bool) -> Result<Vm, String> {
        let mut cells = Vec::new();
        for g in &prog.globals {
            if g.is_routine {
                cells.push(Cell::Scalar { ty: Ty::Int, v: V::Int(0) });
                continue;
            }
            if g.sizes.is_empty() {
                let v = match &g.init {
                    Some(Const::Int(i)) => match g.ty {
                        Ty::Float => V::Flt(*i as f64),
                        _ => V::Int(*i),
                    },
                    Some(Const::Float(f)) => match g.ty {
                        Ty::Int => V::Int(*f as i64),
                        _ => V::Flt(*f),
                    },
                    Some(Const::Str(s)) => V::Str(s.clone()),
                    None => V::zero(g.ty),
                };
                cells.push(Cell::Scalar { ty: g.ty, v });
            } else {
                // A size that does not fold is set by the declaration's own code when it runs.
                let mut dims = Vec::new();
                for s in &g.sizes {
                    match s {
                        Some(n) if *n >= 0 => dims.push(*n as usize),
                        Some(_) => return Err(format!("{}: negative array size", g.name)),
                        None => dims.push(0),
                    }
                }
                let n: usize = dims.iter().product();
                cells.push(Cell::Array { ty: g.ty, dims, data: vec![V::zero(g.ty); n] });
            }
        }
        let first_word = prog.first_word;
        let pool_base = first_word + prog.globals.len() as u16;
        let code = Code::new(prog.code.clone());
        Ok(Vm {
            lifted_error: None,
            lifted_world: None,
            code,
            cells,
            stack: Vec::new(),
            frames: Vec::new(),
            prog,
            first_word,
            pool_base,
            with_commands,
            log: String::new(),
            pending_command: None,
            host: None,
            view: 0,
            read_cfg: super::text::ReadCfg::default(),
            rng: [builtins::CRand::new(0), builtins::CRand::new(0), builtins::CRand::new(0)],
            next_script: None,
            retired: Vec::new(),
            xargs: Vec::new(),
            xret: None,
            xpop: Vec::new(),
        })
    }

    /// Run commands that act on Spike2's world through `host`.
    pub fn with_host(mut self, host: Box<dyn Host>) -> Vm {
        self.host = Some(host);
        self
    }

    /// Hand the host back, with whatever state the run left in it.
    pub fn into_host(self) -> Option<Box<dyn Host>> {
        self.host
    }

    fn current_view(&self) -> i64 {
        self.host.as_ref().map(|h| h.current_view()).unwrap_or(self.view)
    }

    fn set_view(&mut self, h: i64) -> Result<(), RunError> {
        match self.host.as_mut() {
            Some(host) => host.set_view(h),
            None => {
                self.view = h;
                Ok(())
            }
        }
    }

    // ---- reading and writing places --------------------------------------------------

    fn get(&self, r: Ref) -> V {
        match r {
            Ref::Cell(c) => match &self.cells[c] {
                Cell::Scalar { v, .. } => v.clone(),
                Cell::Array { .. } => panic!("an array is not a value"),
            },
            Ref::Elem(c, i) => match &self.cells[c] {
                Cell::Array { data, .. } => data[i].clone(),
                _ => panic!("not an array"),
            },
            Ref::ViewArr(..) | Ref::ViewElem(..) => panic!("a result view's data is read with try_get"),
        }
    }

    /// Read a place, refusing a whole array.
    fn try_get(&self, r: Ref) -> Result<V, RunError> {
        match r {
            Ref::ViewElem(vh, ch, i) => {
                return match self.host.as_ref().map(|h| h.view_elem(vh, ch, i)) {
                    Some(Ok(v)) => Ok(V::Flt(v)),
                    Some(Err(e)) => Err(RunError(e)),
                    None => Err(RunError(ERR_TYPE)),
                };
            }
            Ref::ViewArr(..) => return Err(RunError(ERR_TYPE)),
            _ => {}
        }
        if let Ref::Cell(c) = r {
            if matches!(self.cells[c], Cell::Array { .. }) {
                return Err(RunError(ERR_TYPE));
            }
        }
        Ok(self.get(r))
    }

    fn ty_of(&self, r: Ref) -> Ty {
        match r {
            Ref::Cell(c) | Ref::Elem(c, _) => match &self.cells[c] {
                Cell::Scalar { ty, .. } | Cell::Array { ty, .. } => *ty,
            },
            Ref::ViewArr(..) | Ref::ViewElem(..) => Ty::Float,
        }
    }

    /// Write, converting to the place's own type as the typed setters do: an integer place
    /// takes `to_int`, a real one `cvtsi2sd`.
    pub(super) fn set(&mut self, r: Ref, v: V) -> Result<(), RunError> {
        if let Ref::ViewElem(vh, ch, i) = r {
            let x = as_flt(v.num()?);
            return match self.host.as_mut().map(|h| h.set_view_elem(vh, ch, i, x)) {
                Some(Ok(())) => Ok(()),
                Some(Err(e)) => Err(RunError(e)),
                None => Err(RunError(ERR_TYPE)),
            };
        }
        let ty = self.ty_of(r);
        let v = match (ty, v) {
            (Ty::Int, v) => V::Int(to_int(v.num()?)?),
            (Ty::Float, v) => V::Flt(as_flt(v.num()?)),
            (Ty::Str, V::Str(s)) => V::Str(s),
            (Ty::Str, _) => return Err(RunError(ERR_TYPE)),
        };
        match r {
            Ref::Cell(c) => {
                if let Cell::Scalar { v: slot, .. } = &mut self.cells[c] {
                    *slot = v;
                }
            }
            Ref::Elem(c, i) => {
                if let Cell::Array { data, .. } = &mut self.cells[c] {
                    data[i] = v;
                }
            }
            Ref::ViewArr(..) | Ref::ViewElem(..) => {}
        }
        Ok(())
    }

    // ---- the stack --------------------------------------------------------------------

    fn pop(&mut self) -> Item {
        self.stack.pop().expect("stack underflow: the compiler would not emit this")
    }

    fn value(&mut self) -> Result<V, RunError> {
        match self.pop() {
            Item::Val(v) => Ok(v),
            Item::Ref(r) => self.try_get(r),
            _ => Err(RunError(ERR_TYPE)),
        }
    }

    fn num(&mut self) -> Result<Value, RunError> {
        self.value()?.num()
    }

    fn int(&mut self) -> Result<i64, RunError> {
        to_int(self.num()?)
    }

    fn flt(&mut self) -> Result<f64, RunError> {
        Ok(as_flt(self.num()?))
    }

    fn reference(&mut self) -> Ref {
        match self.pop() {
            Item::Ref(r) => r,
            other => panic!("expected a place, got {other:?}"),
        }
    }

    fn push(&mut self, v: V) {
        self.stack.push(Item::Val(v));
    }

    // ---- frames and calls -------------------------------------------------------------

    fn arg_ref(&self, k: usize) -> Ref {
        let f = self.frames.last().expect("an argument outside a routine");
        match f.args[k] {
            ArgSlot::Own(c) => Ref::Cell(c),
            ArgSlot::Alias(r) => r,
        }
    }

    fn local_ref(&self, k: usize) -> Ref {
        Ref::Cell(self.frames.last().expect("a local outside a routine").locals[k])
    }

    fn call_routine(&mut self, ri: usize, args: Vec<Item>, want_value: bool) -> Result<(), RunError> {
        let r: RoutineInfo = self.prog.routines[ri].clone();
        let arena_base = self.cells.len();
        let mut slots = Vec::new();
        for (p, a) in r.params.iter().zip(args) {
            if p.by_ref || p.array {
                match a {
                    Item::Ref(rf) => slots.push(ArgSlot::Alias(rf)),
                    _ => return Err(RunError(ERR_TYPE)),
                }
            } else {
                let v = match a {
                    Item::Val(v) => v,
                    Item::Ref(rf) => self.try_get(rf)?,
                    _ => return Err(RunError(ERR_TYPE)),
                };
                let v = match (p.ty, v) {
                    (Ty::Int, v) => V::Int(to_int(v.num()?)?),
                    (Ty::Float, v) => V::Flt(as_flt(v.num()?)),
                    (Ty::Str, V::Str(s)) => V::Str(s),
                    _ => return Err(RunError(ERR_TYPE)),
                };
                self.cells.push(Cell::Scalar { ty: p.ty, v });
                slots.push(ArgSlot::Own(self.cells.len() - 1));
            }
        }
        let mut locals = Vec::new();
        for (k, (_, ty)) in r.locals.iter().enumerate() {
            let shape = &r.shapes[k];
            if shape.is_empty() {
                self.cells.push(Cell::Scalar { ty: *ty, v: V::zero(*ty) });
            } else {
                let dims: Vec<usize> = shape.iter().map(|s| s.unwrap_or(0).max(0) as usize).collect();
                let n: usize = dims.iter().product();
                self.cells.push(Cell::Array { ty: *ty, dims, data: vec![V::zero(*ty); n] });
            }
            locals.push(self.cells.len() - 1);
        }
        let ret_cell = r.ret.map(|t| {
            self.cells.push(Cell::Scalar { ty: t, v: V::zero(t) });
            self.cells.len() - 1
        });
        self.frames.push(Frame {
            routine: ri,
            args: slots,
            locals,
            ret_cell,
            ret_ip: self.code.ip,
            want_value,
            arena_base,
        });
        // The routine's first word is its own marker opcode; execution starts after it.
        self.code.ip = r.entry as i32 + 1;
        Ok(())
    }

    /// Run script routine `ri` to its end from inside a command, with `args`; the value it returns.
    pub(crate) fn call_sync(&mut self, ri: usize, items: Vec<Item>) -> Result<Option<V>, RunError> {
        let depth = self.frames.len();
        let saved_ip = self.code.ip;
        let wants = self.prog.routines[ri].ret.is_some();
        self.call_routine(ri, items, wants)?;
        while self.frames.len() > depth {
            match self.step()? {
                None => {}
                Some(_) => return Err(RunError(ERR_TYPE)),
            }
        }
        self.code.ip = saved_ip;
        if !wants {
            return Ok(None);
        }
        match self.stack.pop() {
            Some(Item::Val(v)) => Ok(Some(v)),
            _ => Err(RunError(ERR_TYPE)),
        }
    }

    fn do_return(&mut self) {
        let f = self.frames.pop().expect("Return outside a routine");
        if f.want_value {
            if let Some(c) = f.ret_cell {
                let v = match &self.cells[c] {
                    Cell::Scalar { v, .. } => v.clone(),
                    _ => unreachable!(),
                };
                self.cells.truncate(f.arena_base);
                self.code.ip = f.ret_ip;
                self.push(v);
                return;
            }
        }
        self.cells.truncate(f.arena_base);
        self.code.ip = f.ret_ip;
    }

    fn call(&mut self, as_proc: bool) -> Result<(), RunError> {
        let callee = self.pop();
        match callee {
            Item::Routine(ri) => {
                let argc = self.int()? as usize;
                let at = self.stack.len() - argc;
                let args: Vec<Item> = self.stack.drain(at..).collect();
                let wants = !as_proc && self.prog.routines[ri].ret.is_some();
                self.call_routine(ri, args, wants)
            }
            Item::Cmd(ci) => {
                let (_, cmd) = commands::all().enumerate().nth(ci).map(|(i, c)| (i, c)).unwrap();
                let variant = if cmd.variants.len() > 1 { self.int()? as usize } else { 1 };
                let argc = self.int()? as usize;
                let at = self.stack.len() - argc;
                let items: Vec<Item> = self.stack.drain(at..).collect();
                let args = items
                    .into_iter()
                    .map(|i| match i {
                        Item::Val(v) => Arg::Val(v),
                        Item::Ref(r) => Arg::Ref(r),
                        // a script routine named as an argument: its number, past anything a value can be
                        Item::Routine(ri) => Arg::Val(V::Int(ROUTINE_ARG + ri as i64)),
                        _ => Arg::Val(V::Int(0)),
                    })
                    .collect();
                let out = self.command(&cmd, variant, args)?;
                if let Some(v) = out {
                    if !as_proc {
                        self.push(v);
                    }
                }
                Ok(())
            }
            _ => Err(RunError(ERR_TYPE)),
        }
    }

    // ---- array subscripts -------------------------------------------------------------

    fn index(&mut self, n: usize) -> Result<Ref, RunError> {
        let mut idx = Vec::with_capacity(n);
        for _ in 0..n {
            idx.push(self.int()?);
        }
        idx.reverse();
        let arr = self.reference();
        if let Ref::ViewArr(vh, ch) = arr {
            if n != 1 {
                return Err(RunError(ERR_TYPE));
            }
            return Ok(Ref::ViewElem(vh, ch, idx[0]));
        }
        let Ref::Cell(c) = arr else { return Err(RunError(ERR_TYPE)) };
        let Cell::Array { dims, .. } = &self.cells[c] else { return Err(RunError(ERR_TYPE)) };
        if dims.len() != n {
            return Err(RunError(ERR_TYPE));
        }
        let mut flat = 0usize;
        for (k, &i) in idx.iter().enumerate() {
            if i < 0 || i as usize >= dims[k] {
                return Err(RunError(ERR_INDEX));
            }
            flat = flat * dims[k] + i as usize;
        }
        Ok(Ref::Elem(c, flat))
    }

    // ---- the run ----------------------------------------------------------------------

    fn word_item(&self, w: u16) -> Option<Item> {
        if self.with_commands && (0x7a..0x7a + commands::count() as u16).contains(&w) {
            return Some(Item::Cmd((w - 0x7a) as usize));
        }
        if w >= self.first_word && w < self.pool_base {
            let i = (w - self.first_word) as usize;
            if self.prog.globals[i].is_routine {
                let ri = self.prog.routines.iter().position(|r| r.word == w)?;
                return Some(Item::Routine(ri));
            }
            return Some(Item::Ref(Ref::Cell(i)));
        }
        if w >= self.pool_base {
            let mut k = (w - self.pool_base) as usize;
            if k < self.prog.strings.len() {
                return Some(Item::Val(V::Str(self.prog.strings[k].clone())));
            }
            k -= self.prog.strings.len();
            if k < self.prog.ints.len() {
                return Some(Item::Val(V::Int(self.prog.ints[k])));
            }
            k -= self.prog.ints.len();
            if k < self.prog.floats.len() {
                return Some(Item::Val(V::Flt(self.prog.floats[k])));
            }
        }
        None
    }

    pub fn step(&mut self) -> Result<Option<Stop>, RunError> {
        let at = self.code.ip as usize;
        let Some(&op) = self.code.words.get(at) else {
            return Ok(Some(Stop::Done));
        };
        self.code.ip += 1;
        match op {
            0..=0x14 => self.push(V::Int(op as i64)),
            0x15 | 0x16 | 0x39 => {
                let v = self.value()?;
                let r = self.reference();
                self.set(r, v)?;
            }
            0x17..=0x1b => {
                let r = self.int()?;
                let l = self.int()?;
                let v = match op {
                    0x17 => l.wrapping_add(r),
                    0x18 => l.wrapping_sub(r),
                    0x19 => int_mul(l, r)?,
                    0x1a => int_div(l, r)?,
                    _ => int_mod(l, r)?,
                };
                self.push(V::Int(v));
            }
            0x1c..=0x20 => {
                let r = self.flt()?;
                let l = self.flt()?;
                let v = match op {
                    0x1c => l + r,
                    0x1d => l - r,
                    0x1e => l * r,
                    0x1f => {
                        flt_div_check(r)?;
                        l / r
                    }
                    _ => {
                        flt_div_check(r)?;
                        l % r
                    }
                };
                self.push(V::Flt(v));
            }
            // InitInt, InitFloat, InitString: a copy of a variable, for a by-value argument.
            0x21 | 0x22 | 0x3b => {
                let v = self.value()?;
                self.push(v);
            }
            0x23 => {
                // PopFor: drop the loop record of the loop being left.
                if matches!(self.stack.last(), Some(Item::Loop(..))) {
                    self.stack.pop();
                }
            }
            0x24 => self.call(true)?,
            0x25 => self.call(false)?,
            0x26..=0x28 | 0x5d => {
                // A routine's marker: only reached by falling into it, which the
                // compiler's `Halt` prevents.
            }
            0x29 | 0x2a => self.do_return(),
            0x2b => {
                let f = self.frames.last().expect("ReturnVar outside a routine");
                let c = f.ret_cell.expect("ReturnVar in a proc");
                self.stack.push(Item::Ref(Ref::Cell(c)));
            }
            0x2c => {
                let k = self.code.words[self.code.ip as usize] as usize;
                self.code.ip += 1;
                let r = self.arg_ref(k - 1);
                self.stack.push(Item::Ref(r));
            }
            0x2d => {
                let k = self.code.words[self.code.ip as usize] as usize;
                self.code.ip += 1;
                let r = self.local_ref(k - 1);
                self.stack.push(Item::Ref(r));
            }
            0x2f..=0x33 => {
                let r = self.arg_ref((op - 0x2f) as usize);
                self.stack.push(Item::Ref(r));
            }
            0x34..=0x38 => {
                let r = self.local_ref((op - 0x34) as usize);
                self.stack.push(Item::Ref(r));
            }
            0x3a => {
                let r = self.value()?;
                let l = self.value()?;
                match (l, r) {
                    (V::Str(a), V::Str(b)) => self.push(V::Str(a + &b)),
                    _ => return Err(RunError(ERR_TYPE)),
                }
            }
            0x3c => self.code.jump()?,
            0x3d => {
                let c = self.num()?;
                self.code.jump_if_false(c)?;
            }
            0x3e..=0x46 => {
                let rhs = self.value()?;
                let place = self.reference();
                let cur = self.try_get(place)?;
                let v = match op {
                    0x3e..=0x41 => {
                        let (c, r) = (to_int(cur.num()?)?, to_int(rhs.num()?)?);
                        V::Int(match op {
                            0x3e => c.wrapping_add(r),
                            0x3f => c.wrapping_sub(r),
                            0x40 => int_mul(c, r)?,
                            _ => int_div(c, r)?,
                        })
                    }
                    0x42..=0x45 => {
                        let (c, r) = (as_flt(cur.num()?), as_flt(rhs.num()?));
                        V::Flt(match op {
                            0x42 => c + r,
                            0x43 => c - r,
                            0x44 => c * r,
                            _ => {
                                flt_div_check(r)?;
                                c / r
                            }
                        })
                    }
                    _ => match (cur, rhs) {
                        (V::Str(a), V::Str(b)) => V::Str(a + &b),
                        _ => return Err(RunError(ERR_TYPE)),
                    },
                };
                self.set(place, v)?;
            }
            0x47..=0x4c => {
                let r = self.int()?;
                let l = self.int()?;
                let b = match op {
                    0x47 => l == r,
                    0x48 => l != r,
                    0x49 => l > r,
                    0x4a => l >= r,
                    0x4b => l < r,
                    _ => l <= r,
                };
                self.push(V::from_num(int_result(b)));
            }
            0x4d..=0x52 => {
                let r = self.flt()?;
                let l = self.flt()?;
                let b = match op {
                    0x4d => l == r || l.is_nan() || r.is_nan(),
                    0x4e => !(l == r || l.is_nan() || r.is_nan()),
                    0x4f => l > r,
                    0x50 => l >= r,
                    0x51 => l < r,
                    _ => l <= r,
                };
                self.push(V::from_num(int_result(b)));
            }
            0x53..=0x58 => {
                let r = self.value()?;
                let l = self.value()?;
                let (V::Str(a), V::Str(b)) = (l, r) else { return Err(RunError(ERR_TYPE)) };
                // Not yet checked against Spike2: whether the comparison ignores case.
                let ord = a.cmp(&b);
                let b = match op {
                    0x53 => ord.is_eq(),
                    0x54 => ord.is_ne(),
                    0x55 => ord.is_gt(),
                    0x56 => ord.is_ge(),
                    0x57 => ord.is_lt(),
                    _ => ord.is_le(),
                };
                self.push(V::from_num(int_result(b)));
            }
            0x59 => {
                let v = self.int()?;
                self.push(V::Int(v.wrapping_neg()));
            }
            0x5a => {
                let v = self.int()?;
                self.push(V::from_num(int_result(v == 0)));
            }
            0x5b => {
                let v = self.flt()?;
                self.push(V::Flt(-v));
            }
            0x5c => {
                let v = self.flt()?;
                self.push(V::from_num(int_result(!flt_truthy(v))));
            }
            0x5e | 0x5f => {
                let step = self.num()?;
                let limit = self.num()?;
                let start = self.num()?;
                let place = self.reference();
                let mut var = self.try_get(place)?.num()?;
                let r = self.code.make_for(&mut var, start, limit, step, op == 0x5e);
                self.set(place, V::from_num(var))?;
                if let Some(lp) = r? {
                    self.stack.push(Item::Loop(lp, place));
                }
            }
            0x60 => {
                let Some(&Item::Loop(lp, place)) = self.stack.last() else {
                    panic!("Next without a loop record");
                };
                let mut var = self.try_get(place)?.num()?;
                let again = self.code.next(&lp, &mut var)?;
                self.set(place, V::from_num(var))?;
                if !again {
                    self.stack.pop();
                }
            }
            0x61..=0x66 => {
                let r = self.int()?;
                let l = self.int()?;
                let v = match op {
                    0x61 => (l != 0 && r != 0) as i64,
                    0x62 => (l != 0 || r != 0) as i64,
                    0x63 => ((l != 0) != (r != 0)) as i64,
                    0x64 => l & r,
                    0x65 => l | r,
                    _ => l ^ r,
                };
                self.push(V::Int(v));
            }
            0x67..=0x69 => {
                let r = flt_truthy(self.flt()?);
                let l = flt_truthy(self.flt()?);
                let b = match op {
                    0x67 => l && r,
                    0x68 => l || r,
                    _ => l != r,
                };
                self.push(V::from_num(int_result(b)));
            }
            0x6b..=0x6f => {
                let r = self.index((op - 0x6b + 1) as usize)?;
                self.stack.push(Item::Ref(r));
            }
            0x6a | 0x71 => {
                // A local array's sizes sit under it on the stack, a resized array's under
                // the mask's count; either way they are taken in reverse.
                let mut target = None;
                let n = if op == 0x71 {
                    let mask = self.code.words[self.code.ip as usize];
                    self.code.ip += 1;
                    mask.count_ones() as usize
                } else {
                    let Ref::Cell(c) = self.reference() else { return Err(RunError(ERR_TYPE)) };
                    target = Some(Ref::Cell(c));
                    match &self.cells[c] {
                        Cell::Array { dims, .. } => dims.len(),
                        _ => return Err(RunError(ERR_TYPE)),
                    }
                };
                let mut dims = Vec::new();
                for _ in 0..n {
                    let d = self.int()?;
                    if d < 0 {
                        return Err(RunError(ERR_INDEX));
                    }
                    dims.push(d as usize);
                }
                dims.reverse();
                let Ref::Cell(c) = target.unwrap_or_else(|| self.reference()) else { return Err(RunError(ERR_TYPE)) };
                if let Cell::Array { ty, dims: old, data } = &mut self.cells[c] {
                    if dims.len() != old.len() {
                        return Err(RunError(ERR_TYPE));
                    }
                    // Elements keep their indices where both shapes have them.
                    let n: usize = dims.iter().product();
                    let mut fresh = vec![V::zero(*ty); n];
                    let rank = dims.len();
                    for (flat, v) in data.iter().enumerate() {
                        let mut rest = flat;
                        let mut idx = vec![0usize; rank];
                        for k in (0..rank).rev() {
                            idx[k] = rest % old[k].max(1);
                            rest /= old[k].max(1);
                        }
                        if idx.iter().zip(&dims).all(|(i, d)| i < d) {
                            let to = idx.iter().zip(&dims).fold(0, |acc, (i, d)| acc * d + i);
                            fresh[to] = v.clone();
                        }
                    }
                    *data = fresh;
                    *old = dims;
                }
            }
            // View(h%) as a value, View(h%) as a statement, View(), and the swap that ends
            // View(h%).Command().
            // View(h%, c%) followed by .[i]: the data array of a result view's channel
            0x72 => {
                let ch = self.int()?;
                let vh = self.int()?;
                self.stack.push(Item::Ref(Ref::ViewArr(vh, ch)));
            }
            0x73 => {
                let new = self.int()?;
                let old = self.current_view();
                self.set_view(new)?;
                self.push(V::Int(old));
            }
            0x74 => {
                let h = self.int()?;
                self.set_view(h)?;
            }
            0x75 => {
                let h = self.current_view();
                self.push(V::Int(h));
            }
            0x76 => {
                let result = self.value()?;
                let prev = self.int()?;
                self.set_view(prev)?;
                self.push(result);
            }
            0x79 => return Ok(Some(Stop::Done)),
            w if w >= 0x7a => match self.word_item(w) {
                Some(it) => self.stack.push(it),
                None => return Ok(Some(Stop::Unported { op, at })),
            },
            _ => return Ok(Some(Stop::Unported { op, at })),
        }
        if let Some(name) = self.pending_command.take() {
            return Ok(Some(Stop::Command(name)));
        }
        Ok(None)
    }

    /// Run until `Halt`, the end of the code, an error, or something not ported.
    pub fn run(&mut self) -> Stop {
        loop {
            match self.step() {
                Ok(None) => {}
                Ok(Some(Stop::Done)) => match self.chain() {
                    Ok(true) => {}
                    Ok(false) => return Stop::Done,
                    Err(e) => return Stop::Error(e),
                },
                Ok(Some(s)) => return s,
                Err(RunError(e)) => return Stop::Error(e),
            }
        }
    }

    /// The script `ScriptRun` named, if there is one, takes this one's place: the views and the log stay.
    fn chain(&mut self) -> Result<bool, i32> {
        let Some((name, _flags)) = self.next_script.take() else { return Ok(false) };
        let Some(src) = self.host.as_ref().and_then(|h| h.read_script(&name)) else { return Err(-1) };
        let Ok(compiled) = super::compile_with_commands(&src) else { return Err(-1) };
        let Ok(mut next) = Vm::new(compiled, self.with_commands) else { return Err(-1) };
        next.host = self.host.take();
        next.lifted_world = self.lifted_world.take();
        next.log = std::mem::take(&mut self.log);
        next.read_cfg = self.read_cfg.clone();
        next.rng = std::mem::replace(&mut self.rng, [builtins::CRand::new(0), builtins::CRand::new(0), builtins::CRand::new(0)]);
        next.lifted_error = self.lifted_error.take();
        next.retired = std::mem::take(&mut self.retired);
        next.retired.push((self.prog.globals.iter().map(|g| g.name.clone()).collect(), std::mem::take(&mut self.cells)));
        *self = next;
        Ok(true)
    }

    /// Like [`run`](Vm::run), but stops with [`Stop::Cancelled`] soon after `cancel` is set.
    pub fn run_cancellable(&mut self, cancel: &std::sync::atomic::AtomicBool) -> Stop {
        let mut n = 0u32;
        loop {
            n = n.wrapping_add(1);
            if n & 0x3ff == 0 && cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Stop::Cancelled;
            }
            match self.step() {
                Ok(None) => {}
                Ok(Some(Stop::Done)) => match self.chain() {
                    Ok(true) => {}
                    Ok(false) => return Stop::Done,
                    Err(e) => return Stop::Error(e),
                },
                Ok(Some(s)) => return s,
                Err(RunError(e)) => return Stop::Error(e),
            }
        }
    }

    // ---- results ----------------------------------------------------------------------

    /// The value of a global scalar by name.
    pub fn global(&self, name: &str) -> Option<V> {
        let i = self.prog.globals.iter().position(|g| g.name == name)?;
        match &self.cells[i] {
            Cell::Scalar { v, .. } => Some(v.clone()),
            _ => None,
        }
    }

    /// The elements of a global array by name, in storage order.
    pub fn array(&self, name: &str) -> Option<Vec<V>> {
        let find = |names: Vec<&str>, cells: &[Cell]| {
            let i = names.iter().position(|n| *n == name)?;
            match &cells[i] {
                Cell::Array { data, .. } => Some(data.clone()),
                _ => None,
            }
        };
        find(self.prog.globals.iter().map(|g| g.name.as_str()).collect(), &self.cells).or_else(|| {
            // a script that ScriptRun started took over: the arrays of the one that started it
            self.retired.iter().rev().find_map(|(names, cells)| find(names.iter().map(|n| n.as_str()).collect(), cells))
        })
    }

    // ---- commands ---------------------------------------------------------------------

    fn command(
        &mut self,
        cmd: &commands::Command,
        variant: usize,
        args: Vec<Arg>,
    ) -> Result<Option<V>, RunError> {
        let val = |vm: &Vm, a: &Arg| -> V {
            match a {
                Arg::Val(v) => v.clone(),
                Arg::Ref(r) => vm.try_get(*r).unwrap_or(V::Int(0)),
            }
        };
        let _ = &val;
        // For trying a hand-written command against Spike2's own code: S2LIFT_FORCE=Name,Name or all
        if lifted::has(cmd.name) && lifted::forced(cmd.name) {
            if let Some(out) = self.run_lifted(cmd.name, variant, &args)? {
                return Ok(out);
            }
        }
        if let Some(mut host) = self.host.take() {
            let r = host.command(cmd.name, variant, &args, self);
            self.host = Some(host);
            if let Some(out) = r? {
                return Ok(out);
            }
        }
        match self.builtin(cmd.name, variant, &args)? {
            Some(out) => Ok(out),
            // Spike2's own code for the command, with this crate answering what it asks
            None if lifted::has(cmd.name) => match self.run_lifted(cmd.name, variant, &args)? {
                Some(out) => Ok(out),
                None => Ok(None),
            },
            None => {
                self.pending_command = Some(cmd.name.to_string());
                Ok(None)
            }
        }
    }
}

#[allow(dead_code)]
const _: i32 = ERR_OVERFLOW;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{compile, compile_with_commands};

    fn run(src: &str) -> Vm {
        let c = compile(src).unwrap();
        let mut vm = Vm::new(c, false).unwrap();
        assert_eq!(vm.run(), Stop::Done);
        vm
    }

    #[test]
    fn scalars_and_loops() {
        let vm = run("var s%, i%;\nfor i% := 1 to 10 do s% += i% next;");
        assert_eq!(vm.global("s%"), Some(V::Int(55)));
    }

    #[test]
    fn strings_and_arrays() {
        let vm = run("var a$, v[3];\na$ := \"ab\" + \"cd\";\nv[1] := 2.5;\nv[2] := v[1] * 2;");
        assert_eq!(vm.global("a$"), Some(V::Str("abcd".into())));
        assert_eq!(vm.array("v"), Some(vec![V::Flt(0.0), V::Flt(2.5), V::Flt(5.0)]));
    }

    #[test]
    fn procs_and_funcs_with_value_and_reference_parameters() {
        let vm = run(
            "var r%, n%;\n\
             n% := 3;\n\
             func Sq%(x%)\nx% := x% * x%;\nreturn x%;\nend;\n\
             proc Inc(&k%)\nk% += 1;\nend;\n\
             r% := Sq%(n%);\nInc(n%);",
        );
        assert_eq!(vm.global("r%"), Some(V::Int(9)));
        assert_eq!(vm.global("n%"), Some(V::Int(4)), "by-value left n% alone until Inc");
    }

    #[test]
    fn a_command_runs_when_the_table_is_registered() {
        let c = compile_with_commands("var n%;\nn% := Len(\"abcd\");").unwrap();
        let mut vm = Vm::new(c, true).unwrap();
        assert_eq!(vm.run(), Stop::Done);
        assert_eq!(vm.global("n%"), Some(V::Int(4)));
    }
}

impl Mem for Vm {
    fn value(&self, a: &Arg) -> Result<V, RunError> {
        self.a_val(a)
    }
    fn store(&mut self, r: Ref, v: V) -> Result<(), RunError> {
        self.set(r, v)
    }
    fn array(&self, a: &Arg) -> Option<(Vec<V>, Vec<usize>)> {
        let c = self.a_array(a)?;
        match &self.cells[c] {
            Cell::Array { data, dims, .. } => Some((data.clone(), dims.clone())),
            _ => None,
        }
    }
    fn set_array(&mut self, a: &Arg, data: Vec<V>) -> Result<(), RunError> {
        let c = self.a_array(a).ok_or(RunError(ERR_TYPE))?;
        let ty = self.ty_of(Ref::Cell(c));
        if let Cell::Array { data: d, .. } = &mut self.cells[c] {
            for (slot, v) in d.iter_mut().zip(data) {
                *slot = match (ty, v) {
                    (Ty::Int, v) => V::Int(to_int(v.num()?)?),
                    (Ty::Float, v) => V::Flt(as_flt(v.num()?)),
                    (Ty::Str, V::Str(s)) => V::Str(s),
                    _ => return Err(RunError(ERR_TYPE)),
                };
            }
        }
        Ok(())
    }
    fn target(&self, a: &Arg) -> Option<super::text::Target> {
        let Arg::Ref(r) = a else { return None };
        let ty = self.ty_of(*r);
        if let Ref::Cell(c) = r {
            if let Cell::Array { data, .. } = &self.cells[*c] {
                return Some(super::text::Target { ty, array_len: Some(data.len()) });
            }
        }
        Some(super::text::Target { ty, array_len: None })
    }
    fn store_element(&mut self, a: &Arg, index: usize, v: V) -> Result<(), RunError> {
        let Arg::Ref(Ref::Cell(c)) = a else { return Err(RunError(ERR_TYPE)) };
        let r = Ref::Elem(*c, index);
        self.set(r, v)
    }
    fn read_cfg(&mut self) -> &mut super::text::ReadCfg {
        &mut self.read_cfg
    }
    fn log(&mut self, text: &str) {
        self.log_append(text);
    }
}
