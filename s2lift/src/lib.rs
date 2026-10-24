//! Spike2's own numeric routines, translated from the machine code of its DLLs by
//! `tools/lift.py` (see docs/script.md). `mach` is the small machine model the translation runs
//! on; `msvcrt` and `special` wrap the generated modules in ordinary Rust functions.
//!
//! The generated modules are large, so they live in a crate of their own: they are compiled once.

pub mod mach;
mod msvcr110_gen;
mod spikeutl_gen;

pub mod msvcrt;
pub mod special;
