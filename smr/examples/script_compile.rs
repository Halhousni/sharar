//! Compile a script and print the words, for checking the generator by hand.
//!
//!     cargo run -p smr --example script_compile -- path/to/script.s2s

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: script_compile <file.s2s>");
        std::process::exit(2);
    };
    let src = std::fs::read_to_string(&path).unwrap();
    match smr::script::compile(&src) {
        Ok(c) => {
            println!("WORDS {}", c.code.len());
            println!("{:?}", c.code);
            println!("symbols {:?}", c.symbols);
            println!("strings {:?} ints {:?} floats {:?}", c.strings, c.ints, c.floats);
        }
        Err(e) => {
            println!("ERR {e}");
            std::process::exit(1);
        }
    }
}
