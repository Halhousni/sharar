//! A script run from the app starts in the file on screen and leaves its view changed.

use smr::script::session::{Session, SharedSession, ViewState};
use smr::script::vm::{Stop, Vm};
use std::path::Path;
use std::sync::{Arc, Mutex};

fn demo() -> Option<std::path::PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.join("testdata/Example_Data_Files/Demo.smr");
    p.exists().then_some(p)
}

#[test]
fn a_script_changes_the_view_it_started_in() {
    let Some(path) = demo() else {
        println!("skip: no Demo.smr in testdata");
        return;
    };
    let start = ViewState { hidden: vec![3], cursors: vec![(1, 1.5)], x_range: (0.0, 10.0), selected: vec![1] };
    let mut session = Session::new();
    let h = session.attach(&path, &start).unwrap();
    assert_eq!(session.state_of(h).unwrap(), start);
    let shared = Arc::new(Mutex::new(session));

    let src = "var v%, t;\n\
               v% := ChanVisible(3); ChanShow(3); ChanHide(2);\n\
               t := Cursor(1); Cursor(1, t + 2);\n\
               CursorNew(7.25);\n\
               XRange(100, 110);\n\
               ChanSelect(1, 0); ChanSelect(4, 1);\n\
               Print(\"%d %g\\n\", v%, t);";
    let compiled = smr::script::compile_with_commands(src).unwrap();
    let mut vm = Vm::new(compiled, true).unwrap().with_host(Box::new(SharedSession(shared.clone())));
    assert_eq!(vm.run(), Stop::Done);
    assert_eq!(vm.log, "0 1.5\n");

    let after = shared.lock().unwrap().state_of(h).unwrap();
    assert_eq!(after.hidden, vec![2]);
    assert_eq!(after.cursors, vec![(1, 3.5), (2, 7.25)]);
    assert_eq!(after.x_range, (100.0, 110.0));
    assert_eq!(after.selected, vec![4]);
}
