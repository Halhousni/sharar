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
    /// For a channel of a data view: the seconds per tick and the kind number `ChanKind` gives.
    fn wave_info(&self, _view: i64, _chan: i64) -> Option<(f64, i64)> {
        None
    }
    /// The channels of a data view: (channel number, kind number `ChanKind` gives), in order.
    fn view_channels(&self, _view: i64) -> Vec<(i64, i64)> {
        Vec::new()
    }
    /// Guest folders (lower case, back slashes, ending in one) and the folders here that stand for them.
    fn path_maps(&self) -> Vec<(String, std::path::PathBuf)> {
        Vec::new()
    }
    /// Element `i` of channel `ch` of result view `vh`; or the script error to stop with.
    fn view_elem(&self, _vh: i64, _ch: i64, _i: i64) -> Result<f64, i32> {
        Err(-7001)
    }
    /// The time view a result view reads.
    fn result_source(&self, _vh: i64) -> Option<i64> {
        None
    }
    /// Whether the result view's data is held by the translated code.
    fn view_lifted(&self, _vh: i64) -> bool {
        false
    }
    /// Set it.
    fn set_view_elem(&mut self, _vh: i64, _ch: i64, _i: i64, _v: f64) -> Result<(), i32> {
        Err(-7001)
    }
    /// A result view for translated code to fill in: a new view handle, which becomes the current view.
    fn new_result_view(&mut self) -> i64 {
        -1
    }
    /// The title of a data view's window.
    fn view_title(&self, _view: i64) -> Option<String> {
        None
    }
    /// The text of a script file, for `ScriptRun`.
    fn read_script(&self, _name: &str) -> Option<String> {
        None
    }
    /// The handle of the log window.
    fn log_handle(&self) -> i64 {
        11
    }
    /// The text of a text window other than the log (newlines as `\n`).
    fn text_content(&self, _view: i64) -> Option<String> {
        None
    }
    /// Replace the text of a text window other than the log.
    fn set_text_content(&mut self, _view: i64, _text: String) {}
    /// Add text to a text window other than the log; false if `view` is not one.
    fn text_append(&mut self, _view: i64, _text: &str) -> bool {
        false
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
