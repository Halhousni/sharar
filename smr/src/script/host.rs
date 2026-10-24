//! What the interpreter asks of the program it runs inside.
//!
//! The language and the pure commands (strings, maths, arrays) need nothing outside the
//! script. The rest of the command table acts on Spike2's world: open files, the view that
//! is current, channels, cursors, windows. A [`Host`] is that world. The interpreter hands it
//! each command it does not run itself, together with a [`Mem`] to read arguments and write
//! the ones a command returns through.

use super::exec::RunError;
use super::vm::{Arg, Ref, V};

/// The interpreter's memory, as a command sees it.
pub trait Mem {
    /// The value of an argument.
    fn value(&self, a: &Arg) -> Result<V, RunError>;
    /// Write through a reference argument, converting to its type.
    fn store(&mut self, r: Ref, v: V) -> Result<(), RunError>;
    /// The elements of an array argument in storage order, and its dimensions.
    fn array(&self, a: &Arg) -> Option<(Vec<V>, Vec<usize>)>;
    /// Replace the elements of an array argument (same length).
    fn set_array(&mut self, a: &Arg, data: Vec<V>) -> Result<(), RunError>;
    /// Append to the script log.
    fn log(&mut self, text: &str);
}

/// What a command produced: nothing if it is not this host's to run, otherwise its value
/// (`None` for a command that returns none).
pub type Handled = Option<Option<V>>;

pub trait Host {
    /// The handle of the current view.
    fn current_view(&self) -> i64;
    /// Make `handle` the current view.
    fn set_view(&mut self, handle: i64) -> Result<(), RunError>;
    /// Run a command, or return `Ok(None)` if it is not this host's.
    fn command(
        &mut self,
        name: &str,
        variant: usize,
        args: &[Arg],
        mem: &mut dyn Mem,
    ) -> Result<Handled, RunError>;
}
