//! Running `.s2s` scripts from the Script menu.
//!
//! The interpreter lives in `smr::script`. A run happens on a worker thread so the window
//! keeps drawing; its log comes back when it ends. What a script can do is whatever the
//! interpreter and its `Session` have ported: a script that reaches a command that is not
//! ported stops there and the log says which.

use smr::script::session::Session;
use smr::script::vm::{Stop, Vm};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

struct Run {
    done: Receiver<String>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ScriptUi {
    pub open: bool,
    pub log: String,
    pub path: Option<PathBuf>,
    run: Option<Run>,
    picker: Option<Receiver<Option<PathBuf>>>,
}

/// Compile and run a script to its end, returning the log and a line saying how it stopped.
pub fn run_script(src: &str, cancel: &AtomicBool) -> String {
    let compiled = match smr::script::compile_with_commands(src) {
        Ok(c) => c,
        Err(e) => return format!("Compile error: {}\n", e.0),
    };
    let vm = match Vm::new(compiled, true) {
        Ok(vm) => vm,
        Err(e) => return format!("Cannot start: {e}\n"),
    };
    let mut vm = vm.with_host(Box::new(Session::new()));
    let stop = vm.run_cancellable(cancel);
    let mut out = std::mem::take(&mut vm.log);
    out.push_str(&match stop {
        Stop::Done => "Script finished.\n".to_string(),
        Stop::Error(code) => format!("Script stopped on error {code}.\n"),
        Stop::Command(name) => format!("Script stopped: the command {name} is not ported yet.\n"),
        Stop::Unported { op, at } => format!("Script stopped: opcode {op:#x} at {at} is not ported yet.\n"),
        Stop::Cancelled => "Script cancelled.\n".to_string(),
    });
    out
}

impl ScriptUi {
    pub fn start(&mut self, path: &Path) {
        if self.run.is_some() {
            return;
        }
        let src = match std::fs::read(path) {
            // Spike2 scripts are Windows text, not always UTF-8.
            Ok(b) => b.iter().map(|&c| c as char).collect::<String>(),
            Err(e) => {
                self.log.push_str(&format!("Cannot read {}: {e}\n", path.display()));
                self.open = true;
                return;
            }
        };
        self.path = Some(path.to_path_buf());
        self.log.clear();
        self.log.push_str(&format!("Running {}\n", path.display()));
        self.open = true;
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run_script(&src, &flag));
        });
        self.run = Some(Run { done: rx, cancel });
    }

    pub fn pick(&mut self, ctx: &egui::Context) {
        if self.picker.is_some() {
            return;
        }
        let start = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .set_title("Run script")
                .add_filter("Spike2 script (.s2s)", &["s2s"])
                .add_filter("All files", &["*"]);
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_file());
            ctx.request_repaint();
        });
        self.picker = Some(rx);
    }

    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Collect a finished run or a chosen file, and draw the log window.
    pub fn ui(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.picker {
            match rx.try_recv() {
                Ok(choice) => {
                    self.picker = None;
                    if let Some(p) = choice {
                        self.start(&p);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.picker = None,
            }
        }
        if let Some(run) = &self.run {
            match run.done.try_recv() {
                Ok(text) => {
                    self.log.push_str(&text);
                    self.run = None;
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(TryRecvError::Disconnected) => self.run = None,
            }
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Script log").open(&mut open).default_size([520.0, 300.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.running() {
                    ui.spinner();
                    if ui.button("Cancel").clicked() {
                        if let Some(r) = &self.run {
                            r.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                } else if let Some(p) = self.path.clone() {
                    if ui.button("Run again").clicked() {
                        self.start(&p);
                    }
                }
                if ui.button("Clear").clicked() {
                    self.log.clear();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&self.log).monospace()).selectable(true));
            });
        });
        self.open = open;
    }
}
