//! Compile a script with the command table registered and say whether it compiles:
//! `cargo run -q -p smr --example check -- file.s2s`
fn main() {
    let src = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    match smr::script::compile_with_commands(&src) {
        Ok(_) => println!("ok"),
        Err(e) => println!("{}", e.0),
    }
}
