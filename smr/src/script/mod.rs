//! CED's script language.
//!
//! The engine lives in `compiler.dll` and `runtime.s2l`, not in the viewer;
//! `docs/script.md` records what has been recovered from them. This module starts
//! at the front: a lexer, and nothing that claims to run anything yet.

pub mod lexer;

pub use lexer::{lex, LexError, Tok, Token};
