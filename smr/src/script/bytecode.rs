//! Spike2's compiled script format: one 16-bit word per opcode, with at most one
//! operand word after it.
//!
//! Recovered from the instruction formatter that `CScriptMgr::Disassemble` calls
//! once per instruction (compiler.dll, RVA 0x48c70). That function reads the opcode
//! with `movzwl (program, ip, 2)` and, for the opcodes listed below, increments `ip`
//! once more and reads the next word. Nothing else in it advances `ip`, so every
//! instruction is one or two words long. The comparison ladder at 0x48d6d..0x49129
//! is transcribed in [`shape`], branch for branch.
//!
//! What the formatter does not say is what the words *mean* at run time. The
//! operand of `JumpRelImm` is printed as a signed number, which fits a relative
//! jump, but no interpreter has been read to confirm the base it is relative to.

/// Mnemonics for opcodes 20..=121, in table order. The vendor's array is indexed
/// by `opcode - 0x14` (RVA 0x8a9e0, lookup at 0x48320); `tools/opcode_table.py`
/// regenerates this list from the DLL.
///
/// Element 0 (`Const`) is in the array but never reached: the lookup tests
/// `opcode <= 0x14` first and formats those as `const=%d`.
pub const NAMES: [&str; 102] = [
    "Const", "Int:=", "Flt:=", "Int+", "Int-", "Int*", "Int/", "IntMod", "Float+", "Float-",
    "Float*", "Float/", "FloatMod", "InitInt", "InitFloat", "PopFor", "CallAsProc", "Call",
    "IntProc", "FloatProc", "StringProc", "Return", "End", "ReturnVar", "GetArg", "GetLocal",
    "NullOp", "Arg1", "Arg2", "Arg3", "Arg4", "Arg5", "Local1", "Local2", "Local3", "Local4",
    "Local5", "StringAssign", "StringAdd", "InitString", "JumpRelImm", "JumpRelImmF", "Int+=",
    "Int-=", "Int*=", "Int/=", "Flt+=", "Flt-=", "Flt*=", "Flt/=", "String+=", "IntEquals",
    "IntNotEq", "IntGreat", "IntGtOrEq", "IntLess", "IntLessOrEq", "FltEquals", "FltNotEq",
    "FltGreat", "FltGtOrEq", "FltLess", "FltLessOrEq", "Str=", "Str<>", "Str>", "Str>=", "Str<",
    "Str<=", "IntUMinus", "IntUNot", "FltUMinus", "FltUNot", "VoidProc", "MakeIntFor",
    "MakeFltFor", "Next", "LogAND", "LogOR", "LogXor", "BitAND", "BitOR", "BitXor", "FltLogAND",
    "FltLogOR", "FltLogXor", "ResizeLocArr", "ArrIndex", "Arr2Index", "Arr3Index", "Arr4Index",
    "Arr5Index", "MakeNDArr", "ResizeArray", "CurView", "GetSetView", "SetView", "GetView",
    "SetViewSwap", "MatTrans", "MatDiag", "Halt",
];

/// The highest opcode with a mnemonic. Words above it are symbol references.
pub const LAST_OPCODE: u16 = 0x79;

/// How the formatter treats one opcode: whether a second word follows, how it is
/// printed, and whether the listing breaks the line after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// No operand.
    Plain,
    /// `GetArg` / `GetLocal`: one word `n`, printed `[n]:` and then the name of
    /// argument or local `n - 1` of the current procedure.
    Slot { arg: bool },
    /// `Arg1`..`Arg5`, `Local1`..`Local5`: no operand, the slot is in the opcode.
    ImplicitSlot { arg: bool, index: u16 },
    /// `JumpRelImm`, `JumpRelImmF`: one word, printed as a signed 16-bit number.
    Signed,
    /// `MakeIntFor`, `MakeFltFor`: one word, printed unsigned in decimal.
    Unsigned,
    /// `MakeNDArr`, `ResizeArray`: one word, printed unsigned in octal after `oct:`.
    Octal,
    /// `IntProc`, `FloatProc`, `StringProc`, `VoidProc`: no operand; the listing
    /// looks the instruction's own address up in the procedure table and prints
    /// the procedure's name.
    ProcEntry,
    /// Opcodes 0..=20: no operand, printed `const=N`.
    ConstRef,
    /// Words above 0x79: no operand. The word minus 0x7a indexes one symbol table
    /// (at +0xb0 of the manager), and past its end a second one (at +0x80), whose
    /// entries may carry a value: an int or float printed in parentheses, or a
    /// string printed in quotes. Which tables those are has not been confirmed.
    Symbol,
}

/// Whether a second word is consumed.
impl Shape {
    pub fn has_operand(self) -> bool {
        matches!(self, Shape::Slot { .. } | Shape::Signed | Shape::Unsigned | Shape::Octal)
    }
}

/// The formatter's dispatch, in the order its comparison ladder tests.
pub fn shape(op: u16) -> Shape {
    match op {
        0..=0x14 => Shape::ConstRef,
        0x2c => Shape::Slot { arg: true },
        0x2d => Shape::Slot { arg: false },
        0x2f..=0x33 => Shape::ImplicitSlot { arg: true, index: op - 0x2f },
        0x34..=0x38 => Shape::ImplicitSlot { arg: false, index: op - 0x34 },
        0x3c | 0x3d => Shape::Signed,
        0x26..=0x28 | 0x5d => Shape::ProcEntry,
        0x5e | 0x5f => Shape::Unsigned,
        0x70 | 0x71 => Shape::Octal,
        0x7a.. => Shape::Symbol,
        _ => Shape::Plain,
    }
}

/// Whether the vendor listing ends the line after this opcode. Everything else is
/// followed by a space and the next instruction on the same line, so a listing
/// line is roughly one statement.
pub fn ends_line(op: u16) -> bool {
    matches!(
        op,
        0x15 | 0x16              // Int:=, Flt:=
        | 0x24 | 0x25            // CallAsProc, Call
        | 0x26..=0x28            // IntProc, FloatProc, StringProc
        | 0x29 | 0x2a            // Return, End
        | 0x39                   // StringAssign
        | 0x3c | 0x3d            // JumpRelImm, JumpRelImmF
        | 0x3e..=0x46            // the compound assignments
        | 0x5d..=0x60            // VoidProc, MakeIntFor, MakeFltFor, Next
        | 0x6a..=0x71            // ResizeLocArr, ArrIndex..Arr5Index, MakeNDArr, ResizeArray
        | 0x72 | 0x74 | 0x79     // CurView, SetView, Halt
    )
}

/// The mnemonic, or `None` for constant and symbol words.
pub fn name(op: u16) -> Option<&'static str> {
    if (0x15..=LAST_OPCODE).contains(&op) {
        Some(NAMES[(op - 0x14) as usize])
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instr {
    /// Word index of the opcode.
    pub at: usize,
    pub op: u16,
    pub operand: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub at: usize,
    pub op: u16,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "opcode {} at word {} needs an operand past the end", self.op, self.at)
    }
}

/// Split a word stream into instructions. A missing operand at the end is an
/// error; the vendor loop stops at the end of the buffer instead, but a port
/// should not quietly drop half an instruction.
pub fn decode(words: &[u16]) -> Result<Vec<Instr>, DecodeError> {
    let mut out = Vec::new();
    let mut ip = 0;
    while ip < words.len() {
        let op = words[ip];
        let operand = if shape(op).has_operand() {
            let w = *words.get(ip + 1).ok_or(DecodeError { at: ip, op })?;
            Some(w)
        } else {
            None
        };
        out.push(Instr { at: ip, op, operand });
        ip += 1 + operand.is_some() as usize;
    }
    Ok(out)
}

/// One instruction as the vendor listing prints it, without the names that need
/// the compiler's symbol tables (slot names, procedure names, symbol words).
pub fn format(i: &Instr) -> String {
    let n = name(i.op).unwrap_or("");
    match (shape(i.op), i.operand) {
        (Shape::ConstRef, _) => format!("const={}", i.op),
        (Shape::Symbol, _) => format!("sym#{}", i.op - 0x7a),
        (Shape::Slot { .. }, Some(w)) => format!("{n}[{w}]:"),
        // The colon is printed unconditionally for a slot, but for a procedure
        // only together with the name found in the procedure table.
        (Shape::ImplicitSlot { .. }, _) => format!("{n}:"),
        (Shape::Signed, Some(w)) => format!("{n} {}", w as i16),
        (Shape::Unsigned, Some(w)) => format!("{n} {w}"),
        (Shape::Octal, Some(w)) => format!("{n} oct:{w:o}"),
        _ => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_line_up_with_the_opcode_numbers() {
        assert_eq!(name(0x15), Some("Int:="));
        assert_eq!(name(0x3c), Some("JumpRelImm"));
        assert_eq!(name(0x60), Some("Next"));
        assert_eq!(name(0x79), Some("Halt"));
        assert_eq!(name(0x14), None, "0x14 is formatted as const=20");
        assert_eq!(name(0x7a), None);
    }

    #[test]
    fn exactly_eight_opcodes_take_an_operand() {
        let with: Vec<u16> = (0..=0x79).filter(|&o| shape(o).has_operand()).collect();
        assert_eq!(with, [0x2c, 0x2d, 0x3c, 0x3d, 0x5e, 0x5f, 0x70, 0x71]);
    }

    #[test]
    fn line_breaks_follow_the_formatter() {
        // Opcodes whose branch reaches 0x1800492de or 0x1800490a8 (r12b = 1), from
        // a literal transcription of the comparison ladder.
        let want: Vec<u16> = [
            0x15, 0x16, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x39, 0x3c, 0x3d, 0x3e, 0x3f,
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x5d, 0x5e, 0x5f, 0x60, 0x6a, 0x6b, 0x6c,
            0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x74, 0x79,
        ]
        .into();
        let got: Vec<u16> = (0..=0x7a).filter(|&o| ends_line(o)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn slots_are_numbered_from_the_opcode() {
        assert_eq!(shape(0x2f), Shape::ImplicitSlot { arg: true, index: 0 });
        assert_eq!(shape(0x33), Shape::ImplicitSlot { arg: true, index: 4 });
        assert_eq!(shape(0x34), Shape::ImplicitSlot { arg: false, index: 0 });
        assert_eq!(shape(0x38), Shape::ImplicitSlot { arg: false, index: 4 });
    }

    #[test]
    fn decodes_a_stream_and_formats_it() {
        // GetLocal 3, JumpRelImm -4, MakeNDArr 8, a constant word, a symbol word.
        let words = [0x2d, 3, 0x3c, 0xfffc, 0x70, 8, 7, 0x7a + 5, 0x79];
        let ins = decode(&words).unwrap();
        let text: Vec<String> = ins.iter().map(format).collect();
        assert_eq!(
            text,
            ["GetLocal[3]:", "JumpRelImm -4", "MakeNDArr oct:10", "const=7", "sym#5", "Halt"]
        );
        assert_eq!(ins.iter().map(|i| i.at).collect::<Vec<_>>(), [0, 2, 4, 6, 7, 8]);
    }

    #[test]
    fn a_missing_operand_is_an_error() {
        assert_eq!(decode(&[0x15, 0x3c]), Err(DecodeError { at: 1, op: 0x3c }));
    }

    #[test]
    fn the_names_table_matches_the_dll() {
        // tools/opcode_table.py reads the vendor's array; compare when it can run.
        let Some(dll) = ["vmbin/compiler.dll", "../vmbin/compiler.dll"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            eprintln!("skipping: compiler.dll not present");
            return;
        };
        let script = if dll.starts_with("..") { "../tools/opcode_table.py" } else { "tools/opcode_table.py" };
        let Ok(out) = std::process::Command::new("python3").args([script, dll]).output() else {
            eprintln!("skipping: python3 not available");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let rows: Vec<(u16, String)> = text
            .lines()
            .filter_map(|l| {
                let (n, s) = l.trim().split_once(' ')?;
                Some((n.parse().ok()?, s.to_string()))
            })
            .collect();
        if rows.is_empty() {
            eprintln!("skipping: opcode_table.py printed nothing ({})", String::from_utf8_lossy(&out.stderr));
            return;
        }
        eprintln!("found compiler.dll at {dll}, {} names", rows.len());
        assert_eq!(rows.len(), NAMES.len());
        for (op, s) in rows {
            assert_eq!(NAMES[(op - 0x14) as usize], s, "opcode {op}");
        }
    }
}
