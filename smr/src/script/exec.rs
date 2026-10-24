//! Run-time meaning of the control-flow opcodes: the two jumps, `MakeIntFor`,
//! `MakeFltFor` and `Next`.
//!
//! The interpreter in compiler.dll is not a switch. A builder at RVA 0x19c0 makes
//! one object per opcode, in opcode order, and each class's third virtual method
//! (vtable slot 2) executes it. These are transcribed from those methods:
//!
//! | opcode | class (RTTI) | execute |
//! |---|---|---|
//! | `JumpRelImm` | `TJumpImmediateOperator` | 0xaf10 |
//! | `JumpRelImmF` | `TJumpImmediateIfFalse` | 0xd900 |
//! | `MakeIntFor`, `MakeFltFor` | `TMakeFor` (one class, an int flag) | 0xb0f0 → 0xb100 |
//! | `Next` | `TNext` | 0xc4f0 |
//!
//! Comparisons follow the compiled `comisd` branches, so a NaN goes where the
//! vendor's flags send it rather than where `<` would.

/// Error codes the vendor raises through 0x2d840.
pub const ERR_BAD_JUMP: i32 = -7016;
pub const ERR_ZERO_STEP: i32 = -7030;
/// Integer overflow in `*`, or a float out of range when converted to an int.
pub const ERR_OVERFLOW: i32 = -7011;
/// Division or modulus by zero, int or float.
pub const ERR_DIV_ZERO: i32 = -7012;
/// Not a script error: `i64::MIN / -1` and `i64::MIN mod -1` reach `idiv`
/// unchecked, so the vendor takes a hardware divide fault. The port reports it.
pub const FAULT_IDIV: i32 = i32::MIN;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i64),
    Flt(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunError(pub i32);

/// The instruction pointer and its bound. The vendor keeps both in the code frame:
/// `ip` at +0x34 and the bound at +0x30. The bound is probably the program length
/// in words; that has not been confirmed from the code that sets it.
#[derive(Debug, Clone)]
pub struct Code {
    pub words: Vec<u16>,
    pub ip: i32,
    pub bound: i32,
}

impl Code {
    pub fn new(words: Vec<u16>) -> Self {
        let bound = words.len() as i32;
        Code { words, ip: 0, bound }
    }

    fn operand(&mut self) -> u16 {
        let w = self.words[self.ip as usize];
        self.ip += 1;
        w
    }

    /// 0xe270 and the inline copies of it. A target below -2 is an error; a target
    /// past the bound is ignored and `ip` stays where it was. That second branch
    /// looks like a bug and is reproduced as compiled.
    fn set_ip(&mut self, target: i32) -> Result<(), RunError> {
        if target < -2 {
            return Err(RunError(ERR_BAD_JUMP));
        }
        if target <= self.bound {
            self.ip = target;
        }
        Ok(())
    }

    /// `JumpRelImm`, with `ip` on the operand word. The offset is signed and counts
    /// from the word after the operand.
    pub fn jump(&mut self) -> Result<(), RunError> {
        let off = self.operand() as i16 as i32;
        self.set_ip(self.ip + off)
    }

    /// `JumpRelImmF`, with `cond` popped off the stack. The jump is taken only when
    /// the value is zero. A float NaN counts as true, so it does not jump.
    pub fn jump_if_false(&mut self, cond: Value) -> Result<(), RunError> {
        let off = self.operand() as i16 as i32;
        let sign = match cond {
            Value::Int(v) => v.signum(),
            // comisd v, 0: jb → -1 (taken for NaN, CF = 1), ja → 1, else 0.
            Value::Flt(v) => {
                if v < 0.0 || v.is_nan() {
                    -1
                } else if v > 0.0 {
                    1
                } else {
                    0
                }
            }
        };
        if sign == 0 {
            self.set_ip(self.ip + off)?;
        }
        Ok(())
    }

    /// `MakeIntFor` / `MakeFltFor`. The stack holds, from the bottom, the loop
    /// variable, the start, the limit and the step. The variable is set to the
    /// start; a zero step is an error; then, if the start is already past the
    /// limit, the loop is skipped by the unsigned operand and nothing is pushed.
    /// Otherwise the returned loop record is pushed for `Next`.
    pub fn make_for(
        &mut self,
        var: &mut Value,
        start: Value,
        limit: Value,
        step: Value,
        int: bool,
    ) -> Result<Option<ForLoop>, RunError> {
        let skip = self.operand() as i32;
        // The record keeps the operand's own index; Next resumes one word later.
        let reentry = self.ip - 1;
        let lp = if int {
            let (s, l, st) = (to_int(start)?, to_int(limit)?, to_int(step)?);
            *var = Value::Int(s);
            if st == 0 {
                return Err(RunError(ERR_ZERO_STEP));
            }
            ForLoop { reentry, up: st >= 0, step: Value::Int(st), limit: Value::Int(l) }
        } else {
            let (s, l, st) = (as_flt(start), as_flt(limit), as_flt(step));
            *var = Value::Flt(s);
            // ucomisd step, 0; jne: an unordered (NaN) step is an error too.
            if st == 0.0 || st.is_nan() {
                return Err(RunError(ERR_ZERO_STEP));
            }
            // setae after comisd step, 0.0.
            ForLoop { reentry, up: st >= 0.0, step: Value::Flt(st), limit: Value::Flt(l) }
        };
        if lp.in_range(*var) {
            Ok(Some(lp))
        } else {
            self.set_ip(self.ip + skip)?;
            Ok(None)
        }
    }

    /// `Next`, with the loop record on top of the stack. Adds the step; if the
    /// variable is still in range, jumps back to the word after `MakeFor`'s operand
    /// and returns true. Otherwise the record is popped and execution falls through.
    pub fn next(&mut self, lp: &ForLoop, var: &mut Value) -> Result<bool, RunError> {
        *var = match (*var, lp.step) {
            (v, Value::Int(st)) => Value::Int(as_int(v).wrapping_add(st)),
            (v, Value::Flt(st)) => Value::Flt(as_flt(v) + st),
        };
        if lp.in_range(*var) {
            self.set_ip(lp.reentry + 1)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// The record `TMakeFor` allocates (0x48 bytes): re-entry at +0x10, direction at
/// +0x18, the int flag at +0x19, step at +0x28/+0x38 and limit at +0x30/+0x40.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForLoop {
    pub reentry: i32,
    pub up: bool,
    pub step: Value,
    pub limit: Value,
}

impl ForLoop {
    /// Int: `jg` exits upward, `jge` continues downward. Float: `jbe` continues
    /// upward and `jae` downward, so a NaN keeps an upward loop going and ends a
    /// downward one.
    fn in_range(&self, v: Value) -> bool {
        match self.limit {
            Value::Int(l) => {
                let v = as_int(v);
                if self.up { v <= l } else { v >= l }
            }
            Value::Flt(l) => {
                let v = as_flt(v);
                // comisd v, l: unordered sets CF, so jbe is taken and jae is not.
                if self.up { !(v > l) } else { v >= l }
            }
        }
    }
}

pub(crate) fn as_int(v: Value) -> i64 {
    match v {
        Value::Int(i) => i,
        Value::Flt(f) => f as i64,
    }
}

/// A value read as an int (0xadf0 for a float). Out of range is -7011, NaN
/// included (`comisd` then `jb` is taken when unordered). The upper test is `ja`
/// against 2^63, so 2^63 itself passes and `cvttsd2si` returns the indefinite
/// integer, `i64::MIN`; Rust's `as` would saturate to `i64::MAX` instead.
pub fn to_int(v: Value) -> Result<i64, RunError> {
    match v {
        Value::Int(i) => Ok(i),
        Value::Flt(f) => {
            const LIM: f64 = 9.223372036854775807e18; // 2^63 as stored
            if f > LIM || f < -LIM || f.is_nan() {
                Err(RunError(ERR_OVERFLOW))
            } else if f == LIM {
                Ok(i64::MIN)
            } else {
                Ok(f as i64)
            }
        }
    }
}

pub fn as_flt(v: Value) -> f64 {
    match v {
        Value::Int(i) => i as f64,
        Value::Flt(f) => f,
    }
}

/// One stack entry. The vendor's stack holds objects; a port needs three kinds:
/// a value, a reference to a variable (what assignment and `MakeFor` pop), and
/// the loop record `MakeFor` pushes for `Next`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Item {
    Val(Value),
    Var(usize),
    Loop(ForLoop, usize),
}

/// What stopped a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// `Halt`, or the end of the code.
    Done,
    /// A vendor run-time error code.
    Error(i32),
    /// An opcode this port does not execute yet, at a word index.
    Unported { op: u16, at: usize },
}

/// A stack machine for the opcodes whose execute methods have been read: the
/// constant words, the int and float arithmetic, the comparisons, the logical and
/// bitwise operators, the unary operators, assignment and compound assignment,
/// the two jumps, `MakeIntFor`/`MakeFltFor`/`Next` and `Halt`. Everything else
/// stops with [`Stop::Unported`] rather than guessing.
///
/// Symbol words (above 0x79) are resolved through `symbols`, indexed by
/// `word - 0x7a`. The vendor indexes two tables there and the port cannot build
/// them until the compiler is ported, so tests supply the list.
#[derive(Debug, Clone)]
pub struct Machine {
    pub code: Code,
    pub stack: Vec<Item>,
    pub vars: Vec<Value>,
    pub symbols: Vec<Item>,
}

pub(crate) fn int_result(b: bool) -> Value {
    Value::Int(b as i64)
}

/// `ucomisd x, 0` then `jne`: NaN compares equal to zero, so it reads as false.
pub(crate) fn flt_truthy(x: f64) -> bool {
    !(x == 0.0 || x.is_nan())
}

/// 0xb080: wrapping multiply, then divide back to detect overflow.
pub(crate) fn int_mul(l: i64, r: i64) -> Result<i64, RunError> {
    let p = l.wrapping_mul(r);
    if r != 0 {
        let q = p.checked_div(r).ok_or(RunError(FAULT_IDIV))?;
        if q != l {
            return Err(RunError(ERR_OVERFLOW));
        }
    }
    Ok(p)
}

pub(crate) fn int_div(l: i64, r: i64) -> Result<i64, RunError> {
    if r == 0 {
        return Err(RunError(ERR_DIV_ZERO));
    }
    l.checked_div(r).ok_or(RunError(FAULT_IDIV))
}

pub(crate) fn int_mod(l: i64, r: i64) -> Result<i64, RunError> {
    if r == 0 {
        return Err(RunError(ERR_DIV_ZERO));
    }
    // A divisor of 0x4000 takes a masked shortcut that gives the same truncated
    // remainder as idiv.
    l.checked_rem(r).ok_or(RunError(FAULT_IDIV))
}

/// Float divide and modulus test the divisor with `ucomisd`/`je`, so a NaN
/// divisor raises the same error as zero.
pub(crate) fn flt_div_check(r: f64) -> Result<(), RunError> {
    if r == 0.0 || r.is_nan() {
        Err(RunError(ERR_DIV_ZERO))
    } else {
        Ok(())
    }
}

impl Machine {
    pub fn new(words: Vec<u16>, vars: usize) -> Self {
        Machine {
            code: Code::new(words),
            stack: Vec::new(),
            vars: vec![Value::Int(0); vars],
            symbols: Vec::new(),
        }
    }

    fn pop(&mut self) -> Item {
        self.stack.pop().expect("stack underflow: the compiler would not emit this")
    }

    /// A popped operand, read through a variable reference if it is one.
    fn value(&mut self) -> Value {
        match self.pop() {
            Item::Val(v) => v,
            Item::Var(i) => self.vars[i],
            Item::Loop(..) => panic!("a loop record is not a value"),
        }
    }

    fn var(&mut self) -> usize {
        match self.pop() {
            Item::Var(i) => i,
            other => panic!("expected a variable, got {other:?}"),
        }
    }

    fn int(&mut self) -> Result<i64, RunError> {
        let v = self.value();
        to_int(v)
    }

    fn flt(&mut self) -> f64 {
        let v = self.value();
        as_flt(v)
    }

    fn push(&mut self, v: Value) {
        self.stack.push(Item::Val(v));
    }

    /// Store into a variable, converting to the variable's own type the way the
    /// typed setter does: an int variable takes `to_int`, a float one `cvtsi2sd`.
    fn store(&mut self, i: usize, v: Value) -> Result<(), RunError> {
        self.vars[i] = match self.vars[i] {
            Value::Int(_) => Value::Int(to_int(v)?),
            Value::Flt(_) => Value::Flt(as_flt(v)),
        };
        Ok(())
    }

    /// Execute one instruction. The right-hand operand is on top of the stack.
    pub fn step(&mut self) -> Result<Option<Stop>, RunError> {
        let at = self.code.ip as usize;
        let Some(&op) = self.code.words.get(at) else {
            return Ok(Some(Stop::Done));
        };
        self.code.ip += 1;
        match op {
            // The builder's first loop: 21 integer constants, 0 to 20.
            0..=0x14 => self.push(Value::Int(op as i64)),
            0x15 | 0x16 => {
                let v = self.value();
                let i = self.var();
                self.store(i, v)?;
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
                self.push(Value::Int(v));
            }
            0x1c..=0x20 => {
                let r = self.flt();
                let l = self.flt();
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
                        l % r // C fmod: the sign follows the dividend, as Rust's %
                    }
                };
                self.push(Value::Flt(v));
            }
            0x3c => self.code.jump()?,
            0x3d => {
                let c = self.value();
                self.code.jump_if_false(c)?;
            }
            0x3e..=0x45 => {
                let r = self.value();
                let i = self.var();
                let cur = self.vars[i];
                let v = match op {
                    0x3e => Value::Int(to_int(cur)?.wrapping_add(to_int(r)?)),
                    0x3f => Value::Int(to_int(cur)?.wrapping_sub(to_int(r)?)),
                    0x40 => Value::Int(int_mul(to_int(cur)?, to_int(r)?)?),
                    0x41 => Value::Int(int_div(to_int(cur)?, to_int(r)?)?),
                    0x42 => Value::Flt(as_flt(cur) + as_flt(r)),
                    0x43 => Value::Flt(as_flt(cur) - as_flt(r)),
                    0x44 => Value::Flt(as_flt(cur) * as_flt(r)),
                    _ => {
                        let r = as_flt(r);
                        flt_div_check(r)?;
                        Value::Flt(as_flt(cur) / r)
                    }
                };
                self.store(i, v)?;
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
                self.push(int_result(b));
            }
            0x4d..=0x52 => {
                let r = self.flt();
                let l = self.flt();
                let b = match op {
                    // ucomisd then je/jne: unordered sets ZF, so NaN is "equal".
                    0x4d => l == r || l.is_nan() || r.is_nan(),
                    0x4e => !(l == r || l.is_nan() || r.is_nan()),
                    // comisd with ja/setae/seta: unordered gives 0, as Rust does.
                    0x4f => l > r,
                    0x50 => l >= r,
                    0x51 => l < r,
                    _ => l <= r,
                };
                self.push(int_result(b));
            }
            0x59 => {
                let v = self.int()?;
                self.push(Value::Int(v.wrapping_neg()));
            }
            0x5a => {
                let v = self.int()?;
                self.push(int_result(v == 0));
            }
            0x5b => {
                let v = self.flt();
                self.push(Value::Flt(-v)); // xorpd with the sign bit
            }
            0x5c => {
                let v = self.flt();
                self.push(int_result(!flt_truthy(v)));
            }
            0x5e | 0x5f => {
                let step = self.value();
                let limit = self.value();
                let start = self.value();
                let i = self.var();
                let mut var = self.vars[i];
                let r = self.code.make_for(&mut var, start, limit, step, op == 0x5e);
                // The variable is written before the step is checked.
                self.vars[i] = var;
                if let Some(lp) = r? {
                    self.stack.push(Item::Loop(lp, i));
                }
            }
            0x60 => {
                let Some(&Item::Loop(lp, i)) = self.stack.last() else {
                    panic!("Next without a loop record");
                };
                let mut var = self.vars[i];
                let again = self.code.next(&lp, &mut var)?;
                self.vars[i] = var;
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
                self.push(Value::Int(v));
            }
            0x67..=0x69 => {
                let r = flt_truthy(self.flt());
                let l = flt_truthy(self.flt());
                let b = match op {
                    0x67 => l && r,
                    0x68 => l || r,
                    _ => l != r,
                };
                self.push(int_result(b));
            }
            0x79 => return Ok(Some(Stop::Done)),
            0x7a.. => {
                let k = (op - 0x7a) as usize;
                match self.symbols.get(k) {
                    Some(&it) => self.stack.push(it),
                    None => return Ok(Some(Stop::Unported { op, at })),
                }
            }
            _ => return Ok(Some(Stop::Unported { op, at })),
        }
        Ok(None)
    }

    /// Run until `Halt`, the end of the code, an error, or an unported opcode.
    pub fn run(&mut self) -> Stop {
        loop {
            match self.step() {
                Ok(None) => {}
                Ok(Some(s)) => return s,
                Err(RunError(e)) => return Stop::Error(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ip: i32, n: usize) -> Code {
        let mut c = Code::new(vec![0; n]);
        c.ip = ip;
        c
    }

    #[test]
    fn a_jump_counts_from_the_word_after_its_operand() {
        let mut c = at(4, 20);
        c.words[4] = (-3i16) as u16;
        c.jump().unwrap();
        assert_eq!(c.ip, 2);
        let mut c = at(4, 20);
        c.words[4] = 7;
        c.jump().unwrap();
        assert_eq!(c.ip, 12);
    }

    #[test]
    fn a_jump_past_the_bound_is_ignored_and_one_below_minus_two_fails() {
        let mut c = at(4, 10);
        c.words[4] = 100;
        c.jump().unwrap();
        assert_eq!(c.ip, 5, "ip stays after the operand");
        let mut c = at(4, 10);
        c.words[4] = (-8i16) as u16; // 5 - 8 = -3
        assert_eq!(c.jump(), Err(RunError(ERR_BAD_JUMP)));
        let mut c = at(4, 10);
        c.words[4] = (-7i16) as u16; // -2 is accepted
        c.jump().unwrap();
        assert_eq!(c.ip, -2);
    }

    #[test]
    fn jump_if_false_takes_zero_only_and_nan_is_true() {
        for (v, taken) in [
            (Value::Int(0), true),
            (Value::Int(-1), false),
            (Value::Int(5), false),
            (Value::Flt(0.0), true),
            (Value::Flt(-0.0), true),
            (Value::Flt(1e-300), false),
            (Value::Flt(f64::NAN), false),
        ] {
            let mut c = at(0, 20);
            c.words[0] = 5;
            c.jump_if_false(v).unwrap();
            assert_eq!(c.ip == 6, taken, "{v:?}");
        }
    }

    /// `for i := 1 to 3` runs three times and leaves i = 4.
    #[test]
    fn an_int_loop_runs_and_leaves_the_variable_one_step_past() {
        let mut c = at(10, 40);
        c.words[10] = 9;
        let mut i = Value::Int(0);
        let lp = c
            .make_for(&mut i, Value::Int(1), Value::Int(3), Value::Int(1), true)
            .unwrap()
            .expect("entered");
        assert_eq!(c.ip, 11);
        let mut bodies = 1;
        while c.next(&lp, &mut i).unwrap() {
            assert_eq!(c.ip, 11);
            bodies += 1;
        }
        assert_eq!((bodies, i), (3, Value::Int(4)));
    }

    #[test]
    fn a_loop_already_past_its_limit_skips_by_the_operand() {
        let mut c = at(10, 40);
        c.words[10] = 9;
        let mut i = Value::Int(0);
        let r = c.make_for(&mut i, Value::Int(5), Value::Int(3), Value::Int(1), true).unwrap();
        assert_eq!((r, c.ip, i), (None, 20, Value::Int(5)));
        // Downward: 3 to 5 step -1 is also past.
        let mut c = at(10, 40);
        c.words[10] = 9;
        let r = c.make_for(&mut i, Value::Int(3), Value::Int(5), Value::Int(-1), true).unwrap();
        assert_eq!((r, c.ip), (None, 20));
    }

    #[test]
    fn a_zero_step_is_an_error_and_so_is_a_nan_one() {
        let mut i = Value::Int(0);
        let mut c = at(0, 4);
        let r = c.make_for(&mut i, Value::Int(0), Value::Int(3), Value::Int(0), true);
        assert_eq!(r, Err(RunError(ERR_ZERO_STEP)));
        let mut c = at(0, 4);
        let r = c.make_for(&mut i, Value::Flt(0.0), Value::Flt(3.0), Value::Flt(f64::NAN), false);
        assert_eq!(r, Err(RunError(ERR_ZERO_STEP)));
    }

    #[test]
    fn a_nan_keeps_an_upward_float_loop_and_ends_a_downward_one() {
        let mut v = Value::Flt(0.0);
        let mut c = at(0, 20);
        let r = c.make_for(&mut v, Value::Flt(f64::NAN), Value::Flt(1.0), Value::Flt(0.5), false);
        assert!(r.unwrap().is_some(), "upward: NaN start enters");
        let mut c = at(0, 20);
        let r = c.make_for(&mut v, Value::Flt(f64::NAN), Value::Flt(1.0), Value::Flt(-0.5), false);
        assert!(r.unwrap().is_none(), "downward: NaN start skips");
    }

    #[test]
    fn a_float_loop_steps_down() {
        let mut c = at(0, 20);
        c.words[0] = 3;
        let mut x = Value::Flt(0.0);
        let lp = c
            .make_for(&mut x, Value::Flt(1.0), Value::Flt(0.0), Value::Flt(-0.25), false)
            .unwrap()
            .unwrap();
        let mut seen = vec![x];
        while c.next(&lp, &mut x).unwrap() {
            seen.push(x);
        }
        let want: Vec<Value> = [1.0, 0.75, 0.5, 0.25, 0.0].map(Value::Flt).into();
        assert_eq!(seen, want);
    }

    // ── the stack machine ──────────────────────────────────────────────────

    const SYM: u16 = 0x7a;

    fn m(words: &[u16], vars: &[Value]) -> Machine {
        let mut m = Machine::new(words.to_vec(), vars.len());
        m.vars = vars.to_vec();
        m.symbols = (0..vars.len()).map(Item::Var).collect();
        m
    }

    #[test]
    fn assignment_and_arithmetic() {
        // a% := 7 - 3*2 ; the right operand is on top
        let mut x = m(&[SYM, 7, 3, 2, 0x19, 0x18, 0x15, 0x79], &[Value::Int(0)]);
        assert_eq!(x.run(), Stop::Done);
        assert_eq!(x.vars[0], Value::Int(1));
    }

    #[test]
    fn a_for_loop_sums_one_to_ten() {
        // s% := 0; for i% := 1 to 10 do s% += i% next; halt
        let words = [
            SYM, 0, 0x15, // s% := 0
            SYM + 1, 1, 10, 1, 0x5e, 4, // MakeIntFor, skip to Halt if empty
            SYM, SYM + 1, 0x3e, // s% += i%
            0x60, // Next
            0x79,
        ];
        let mut x = m(&words, &[Value::Int(0), Value::Int(0)]);
        assert_eq!(x.run(), Stop::Done);
        assert_eq!(x.vars, [Value::Int(55), Value::Int(11)]);
        assert!(x.stack.is_empty(), "Next pops the record when the loop ends");
    }

    #[test]
    fn a_while_loop_from_the_two_jumps() {
        // while n% > 0 do n% -= 1; c% += 2 wend
        let words = [
            SYM, 0, 0x49, 0x3d, 8, // n% > 0, JumpRelImmF +8 → word 13
            SYM, 1, 0x3f, // n% -= 1
            SYM + 1, 2, 0x3e, // c% += 2
            0x3c, (-13i16) as u16, // back to word 0
            0x79,
        ];
        let mut x = m(&words, &[Value::Int(4), Value::Int(0)]);
        assert_eq!(x.run(), Stop::Done);
        assert_eq!(x.vars, [Value::Int(0), Value::Int(8)]);
    }

    #[test]
    fn division_by_zero_and_overflow_raise_the_vendor_codes() {
        assert_eq!(m(&[5, 0, 0x1a], &[]).run(), Stop::Error(ERR_DIV_ZERO));
        assert_eq!(m(&[5, 0, 0x1b], &[]).run(), Stop::Error(ERR_DIV_ZERO));
        let mut x = m(&[SYM, SYM, 0x19], &[Value::Int(1 << 40)]);
        assert_eq!(x.run(), Stop::Error(ERR_OVERFLOW));
        // Float: zero and NaN divisors both raise -7012.
        let mut x = m(&[SYM, SYM + 1, 0x1f], &[Value::Flt(1.0), Value::Flt(0.0)]);
        assert_eq!(x.run(), Stop::Error(ERR_DIV_ZERO));
        let mut x = m(&[SYM, SYM + 1, 0x1f], &[Value::Flt(1.0), Value::Flt(f64::NAN)]);
        assert_eq!(x.run(), Stop::Error(ERR_DIV_ZERO));
    }

    #[test]
    fn nan_comparisons_follow_the_compiled_flags() {
        let n = Value::Flt(f64::NAN);
        let one = Value::Flt(1.0);
        let cmp = |op: u16, l: Value, r: Value| {
            let mut x = m(&[SYM, SYM + 1, op], &[l, r]);
            x.run();
            x.stack.last().copied()
        };
        let t = Some(Item::Val(Value::Int(1)));
        let f = Some(Item::Val(Value::Int(0)));
        assert_eq!(cmp(0x4d, n, one), t, "FltEquals: NaN = 1 is true");
        assert_eq!(cmp(0x4e, n, one), f, "FltNotEq: NaN <> 1 is false");
        for op in 0x4f..=0x52 {
            assert_eq!(cmp(op, n, one), f, "ordered comparison {op:#x}");
        }
        assert_eq!(cmp(0x67, n, one), f, "FltLogAND treats NaN as false");
        assert_eq!(cmp(0x68, n, Value::Flt(0.0)), f, "FltLogOR treats NaN as false");
        let mut x = m(&[SYM, 0x5c], &[n]);
        x.run();
        assert_eq!(x.stack.last().copied(), t, "FltUNot: not NaN is true");
    }

    #[test]
    fn an_int_variable_rejects_an_out_of_range_float() {
        let mut x = m(&[SYM, SYM + 1, 0x15], &[Value::Int(0), Value::Flt(1e19)]);
        assert_eq!(x.run(), Stop::Error(ERR_OVERFLOW));
        let mut x = m(&[SYM, SYM + 1, 0x15], &[Value::Int(0), Value::Flt(-2.7)]);
        assert_eq!(x.run(), Stop::Done);
        assert_eq!(x.vars[0], Value::Int(-2), "truncation, cvttsd2si");
    }

    #[test]
    fn an_unported_opcode_stops_with_its_address() {
        assert_eq!(m(&[1, 0x24], &[]).run(), Stop::Unported { op: 0x24, at: 1 });
    }

    // ── checked against Spike2 ─────────────────────────────────────────────
    //
    // tools/oracle/t1.s2s and t2.s2s were run in Spike2 8.10 and wrote their
    // variables to a file; the numbers below are what came back. The programs
    // here are hand-assembled equivalents, because the compiler is not ported:
    // they check the run-time rules, not Spike2's own instruction layout.

    /// A tiny assembler: symbols are pushed by word, forward jumps patched later.
    struct Asm {
        w: Vec<u16>,
        sym: Vec<Item>,
    }

    impl Asm {
        fn new(vars: usize) -> Self {
            Asm { w: Vec::new(), sym: (0..vars).map(Item::Var).collect() }
        }
        fn v(&mut self, i: usize) -> &mut Self {
            self.w.push(SYM + i as u16);
            self
        }
        fn k(&mut self, x: f64) -> &mut Self {
            self.sym.push(Item::Val(Value::Flt(x)));
            self.w.push(SYM + (self.sym.len() - 1) as u16);
            self
        }
        fn op(&mut self, o: &[u16]) -> &mut Self {
            self.w.extend_from_slice(o);
            self
        }
        /// Emit an opcode with a placeholder operand; returns the operand index.
        fn fwd(&mut self, o: u16) -> usize {
            self.w.extend_from_slice(&[o, 0]);
            self.w.len() - 1
        }
        /// Point a forward operand at the current end.
        fn land(&mut self, at: usize) {
            self.w[at] = (self.w.len() - (at + 1)) as u16;
        }
        fn back(&mut self, o: u16, to: usize) {
            self.w.push(o);
            let after = self.w.len() + 1;
            self.w.push((to as i32 - after as i32) as i16 as u16);
        }
        fn machine(&self, vars: &[Value]) -> Machine {
            let mut x = Machine::new(self.w.clone(), vars.len());
            x.vars = vars.to_vec();
            x.symbols = self.sym.clone();
            x
        }
    }

    /// The oracle file, if this machine has pulled it from the guest.
    fn oracle(name: &str) -> Option<Vec<u8>> {
        for d in ["vmbin/oracle", "../vmbin/oracle"] {
            if let Ok(b) = std::fs::read(format!("{d}/{name}")) {
                eprintln!("found {d}/{name}");
                return Some(b);
            }
        }
        eprintln!("skipping the file comparison: {name} not present");
        None
    }

    #[test]
    fn t1_matches_spike2() {
        let (a, b, s, i, c, x, y, z) = (0, 1, 2, 3, 4, 5, 6, 7);
        let mut p = Asm::new(8);
        p.v(a).op(&[7, 3, 2, 0x19, 0x18, 0x15]); // a% := 7 - 3*2
        p.v(s).op(&[0, 0x15]); // s% := 0
        p.v(i).op(&[1, 10, 1]);
        let skip = p.fwd(0x5e); // for i% := 1 to 10
        p.v(s).v(i).op(&[0x3e, 0x60]); // s% += i% next
        p.land(skip);
        p.v(b).op(&[4, 0x15]).v(c).op(&[0, 0x15]);
        let top = p.w.len();
        p.v(b).op(&[0, 0x49]); // while b% > 0
        let out = p.fwd(0x3d);
        p.v(b).op(&[1, 0x3f]).v(c).op(&[2, 0x3e]); // b% -= 1; c% += 2
        p.back(0x3c, top); // wend
        p.land(out);
        p.v(x).k(1.5).op(&[0x16]); // x := 1.5
        p.v(y).v(x).op(&[2, 0x1e]).k(0.25).op(&[0x1d, 0x16]); // y := x*2 - 0.25
        p.v(z).op(&[0, 0x16]);
        p.v(x).op(&[1, 0]).k(-0.25);
        let skip = p.fwd(0x5f); // for x := 1 to 0 step -0.25
        p.v(z).v(x).op(&[0x42, 0x60]); // z += x next
        p.land(skip);
        p.op(&[0x79]);

        let int = Value::Int(0);
        let flt = Value::Flt(0.0);
        let mut m = p.machine(&[int, int, int, int, int, flt, flt, flt]);
        assert_eq!(m.run(), Stop::Done);
        let want = [
            Value::Int(1),
            Value::Int(0),
            Value::Int(55),
            Value::Int(11),
            Value::Int(8),
            Value::Flt(-0.25),
            Value::Flt(2.75),
            Value::Flt(2.5),
        ];
        assert_eq!(m.vars, want);

        // Spike2 wrote five i32 and three f64 with BWrite.
        if let Some(bin) = oracle("t1.bin") {
            assert_eq!(bin.len(), 44);
            for (k, w) in want.iter().enumerate() {
                let got = if k < 5 {
                    Value::Int(i32::from_le_bytes(bin[4 * k..4 * k + 4].try_into().unwrap()) as i64)
                } else {
                    let o = 20 + 8 * (k - 5);
                    Value::Flt(f64::from_le_bytes(bin[o..o + 8].try_into().unwrap()))
                };
                assert_eq!(got, *w, "variable {k}");
            }
        }
    }

    /// The single-expression cases of t2, each as Spike2 reported it.
    #[test]
    fn t2_matches_spike2() {
        let nan = Value::Flt(f64::NAN);
        let one = Value::Flt(1.0);
        let top = |words: &[u16], vars: &[Value]| -> Result<Value, Stop> {
            let mut x = m(words, vars);
            match x.run() {
                Stop::Done => {}
                s => return Err(s),
            }
            match x.stack.last() {
                Some(Item::Val(v)) => Ok(*v),
                other => panic!("{other:?}"),
            }
        };
        let i = |v: i64| Ok(Value::Int(v));
        let mut got = Vec::new();
        for op in [0x4d, 0x4e, 0x51, 0x4f, 0x52, 0x50] {
            got.push(top(&[SYM, SYM + 1, op], &[nan, one])); // r[0]..r[5]
        }
        got.push(top(&[SYM, 0x5c], &[nan])); // r[6] not n
        // `n and 1` stopped Spike2 with "Integer Overflow": the compiler chose the
        // int LogAND, and NaN does not convert.
        got.push(top(&[SYM, 1, 0x61], &[nan]));
        let mut k = m(&[SYM, SYM + 1, 0x15], &[Value::Int(0), Value::Flt(-2.7)]);
        k.run();
        got.push(Ok(k.vars[0])); // r[9]
        got.push(top(&[SYM, 3, 0x1b], &[Value::Int(-7)])); // -7 mod 3
        got.push(top(&[7, SYM, 0x1b], &[Value::Int(-3)])); // 7 mod -3
        got.push(top(&[SYM, 2, 0x1a], &[Value::Int(-7)])); // -7 / 2
        got.push(top(&[SYM, SYM + 1, 0x20], &[Value::Flt(-7.5), Value::Flt(2.0)])); // r[13]
        got.push(top(&[6, 3, 0x64], &[])); // 6 band 3
        got.push(top(&[6, 3, 0x66], &[])); // 6 bxor 3
        got.push(top(&[2, 0, 0x63], &[])); // 2 xor 0
        let want = [
            i(1),
            i(0),
            i(0),
            i(0),
            i(0),
            i(0),
            i(1),
            Err(Stop::Error(ERR_OVERFLOW)),
            i(-2),
            i(-1),
            i(1),
            i(-3),
            Ok(Value::Flt(-1.5)),
            i(2),
            i(5),
            i(1),
        ];
        assert_eq!(got, want);

        // Skipped int loop leaves the variable at the start; float loop ends at 2.5.
        let mut p = Asm::new(1);
        p.v(0).op(&[5, 3, 1]);
        let s = p.fwd(0x5e);
        p.op(&[0x60]);
        p.land(s);
        let mut x = p.machine(&[Value::Int(0)]);
        x.run();
        assert_eq!(x.vars[0], Value::Int(5));
        let mut p = Asm::new(1);
        p.v(0).op(&[1, 2]).k(0.5);
        let s = p.fwd(0x5f);
        p.op(&[0x60]);
        p.land(s);
        let mut x = p.machine(&[Value::Flt(0.0)]);
        x.run();
        assert_eq!(x.vars[0], Value::Flt(2.5));

        // t2.bin is r[0..20] as f64; r[7] and r[8] were set to -1 after the
        // overflow case was split out, and r[19] stays 0 because its loop is skipped.
        if let Some(bin) = oracle("t2.bin") {
            let r: Vec<f64> = bin.chunks(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect();
            let file = [
                1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, -1.0, -1.0, -2.0, -1.0, 1.0, -3.0, -1.5, 5.0, 2.5,
                2.0, 5.0, 1.0, 0.0,
            ];
            assert_eq!(r, file);
        }
    }
}
