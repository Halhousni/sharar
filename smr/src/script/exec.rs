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
            let (s, l, st) = (as_int(start), as_int(limit), as_int(step));
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

fn as_int(v: Value) -> i64 {
    match v {
        Value::Int(i) => i,
        Value::Flt(f) => f as i64,
    }
}

fn as_flt(v: Value) -> f64 {
    match v {
        Value::Int(i) => i as f64,
        Value::Flt(f) => f,
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
}
