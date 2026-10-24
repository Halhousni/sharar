//! Run a script with the interpreter: `cargo run -p smr --example runscript -- file.s2s`
use smr::script::vm::{Stop, Vm};
fn main() {
    let path = std::env::args().nth(1).expect("a .s2s file");
    let src = std::fs::read_to_string(path).unwrap();
    let c = match smr::script::compile_with_commands(&src) {
        Ok(c) => c,
        Err(e) => return println!("compile error: {}", e.0),
    };
    let mut vm = Vm::new(c, true).unwrap();
    let stop = vm.run();
    print!("{}", vm.log);
    if let Some(why) = &vm.lifted_error {
        println!("translated code stopped: {why}");
    }
    println!("stopped: {stop:?}");
    if let Some(res) = vm.array("res") {
        println!("res = {res:?}");
    }
}
