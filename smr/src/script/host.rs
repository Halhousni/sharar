//! What the interpreter asks of the program it runs inside.
//!
//! The language and the pure commands (strings, maths, arrays) need nothing outside the
//! script. The rest of the command table acts on Spike2's world: open files, the view that
//! is current, channels, cursors, windows. A [`Host`] is that world. The interpreter hands it
//! each command it does not run itself, together with a [`Mem`] to read arguments and write
//! the ones a command returns through.

use super::exec::RunError;
use super::text::{ReadCfg, Target};
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
    /// What kind of place an argument is, for a command that fills it.
    fn target(&self, a: &Arg) -> Option<Target>;
    /// Write element `index` of an array argument.
    fn store_element(&mut self, a: &Arg, index: usize, v: V) -> Result<(), RunError>;
    /// `ReadSetup`'s state, shared by `Read` and `ReadStr`.
    fn read_cfg(&mut self) -> &mut ReadCfg;
    /// Append to the script log.
    fn log(&mut self, text: &str);
}

/// What a command produced: nothing if it is not this host's to run, otherwise its value
/// (`None` for a command that returns none).
pub type Handled = Option<Option<V>>;

pub trait Host {
    /// Offer text that `Print` produced: a host with a text file as its current view takes it
    /// (and returns true); otherwise it goes to the script log.
    fn print(&mut self, _text: &str) -> bool {
        false
    }
    /// The handle of the current view.
    fn current_view(&self) -> i64;
    /// Make `handle` the current view.
    fn set_view(&mut self, handle: i64) -> Result<(), RunError>;
    /// What kind of view `handle` is (the number `ViewKind` gives) and, for a data file, its path.
    fn view_info(&self, _handle: i64) -> Option<(i64, Option<std::path::PathBuf>)> {
        None
    }
    /// Run a command, or return `Ok(None)` if it is not this host's.
    fn command(
        &mut self,
        name: &str,
        variant: usize,
        args: &[Arg],
        mem: &mut dyn Mem,
    ) -> Result<Handled, RunError>;
}
