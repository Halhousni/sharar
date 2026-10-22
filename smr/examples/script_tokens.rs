//! Print the token stream of a CED script.
//!
//!   cargo run -p smr --example script_tokens -- path/to/script.s2s

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: script_tokens <file.s2s>");
            std::process::exit(2);
        }
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    match smr::script::lex(&src) {
        Ok(toks) => {
            for t in &toks {
                println!("{:>4}:{:<4} {}", t.line, t.col, t.tok);
            }
            eprintln!("{} tokens", toks.len());
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    }
}
