//! CED's script language.
//!
//! The engine lives in `compiler.dll` and `runtime.s2l`, not in the viewer;
//! `docs/script.md` records what has been recovered from them. This module is the
//! front end: a lexer, and a parser for the grammar Spike2's own scripts use.

pub mod lexer;
pub mod parser;

pub use lexer::{lex, LexError, Tok, Token};
pub use parser::{parse, Declarator, Expr, ParseError, Parser, Stmt};
