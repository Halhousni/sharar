//! Spike2's own numeric routines, translated from the machine code of its DLLs by
//! `tools/lift.py` (see docs/script.md). `mach` is the small machine model the translation runs
//! on; `msvcrt` and `special` wrap the generated modules in ordinary Rust functions.
//!
//! The generated modules are large, so they live in a crate of their own: they are compiled once.

pub mod mach;
mod msvcr110_gen;
pub mod strings_gen;
mod rt_compiler_gen;
mod rt_msvcp110_gen;
mod rt_runtime_gen;
mod rt_sonlayer_gen;
mod rt_son64_gen;
mod rt_spikecmd_gen;
mod rt_sonview_gen;
mod rt_use1432_gen;
mod rt_filters_gen;
mod rt_ccomm_gen;
mod rt_spikeutl_gen;
mod spikeutl_gen;

/// Spike2's script commands, translated from `runtime.s2l` with the routines they call.
pub mod commands;

pub mod msvcrt;
pub mod special;

/// What translated code asks of its host: the routines it imports that were not translated.
/// `id` indexes `IMPORT_NAMES`.
pub trait Svc {
    fn import(&mut self, m: &mut mach::M, id: u32);
    /// A call through a pointer that is in none of the translated modules (a method of an
    /// object the host built). Returns false if the host does not know it either.
    fn call(&mut self, _m: &mut mach::M, _target: u64) -> bool {
        false
    }
}
