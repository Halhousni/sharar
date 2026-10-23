//! CED's script language.
//!
//! The engine lives in `compiler.dll` and `runtime.s2l`, not in the viewer;
//! `docs/script.md` records what has been recovered from them. This module is the
//! front end: a lexer, and a parser for the grammar Spike2's own scripts use.

pub mod bytecode;
pub mod compile;
pub mod exec;
pub mod lexer;
pub mod parser;
pub mod print;

pub use lexer::{lex, LexError, Tok, Token};
pub use compile::{compile, CompileError, Compiled};
pub use parser::{parse, Declarator, Expr, IndexArg, ParseError, Parser, Stmt};

#[cfg(test)]
mod roundtrip {
    use super::parser::parse;
    use super::print::program;

    /// Parse, print, parse again: the two trees must be identical. Any difference
    /// is a printer that lost a distinction or a parser that invented one.
    fn round_trip(src: &str) {
        let first = parse(src).unwrap_or_else(|e| panic!("first parse: {e}"));
        let text = program(&first);
        let second = match parse(&text) {
            Ok(s) => s,
            Err(e) => panic!("re-parse failed: {e}\n--- printed ---\n{text}"),
        };
        assert_eq!(first, second, "trees differ\n--- printed ---\n{text}");
    }

    #[test]
    fn small_program_round_trips() {
        round_trip(concat!(
            "var a[10], b := 2;\n",
            "proc P(x%)\n  a[x%] := a[x%] + 1;\nend;\n",
            "func F(x)\n  return x*2 + 1;\nend;\n",
            "if b > 1 then P(1) else P(2) endif;\n",
            "while b < 10 do b := b + 1 wend;\n",
            "repeat b := b - 1 until b <= 2;\n",
            "for i% := 0 to 9 step 2 do a[i%] := i% next;\n",
            "docase\n case b = 1 then P(3);\n case b = 2 then P(4);\nendcase;\n",
        ));
    }

    #[test]
    fn slices_and_members_survive_the_round_trip() {
        // `coef[]`, `coef[:n%]`, `coef[1:2]` and `View(3).Name$(1)` all have to
        // come back as themselves; without the IndexArg distinction `coef[:n%]`
        // and `coef[n%]` are the same tree and the printer cannot tell them apart.
        round_trip("x := f(coef[], coef[:n%], coef[1:2], View(3).Name$(1));\n");
    }

    #[test]
    fn the_vendors_demo_round_trips() {
        let Some(src) = super::parser::tests::demo_script() else {
            eprintln!("skipping: demo script not present");
            return;
        };
        round_trip(&src);
    }
}
