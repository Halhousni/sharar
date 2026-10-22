//! Parse a CED script and print a summary, or the error if it does not parse.
//!
//!   cargo run -p smr --example script_parse -- path/to/script.s2s

fn count(stmts: &[smr::script::Stmt], acc: &mut [usize; 10]) {
    use smr::script::Stmt::*;
    for s in stmts {
        match s {
            Var(_) => acc[0] += 1,
            Const(_) => acc[1] += 1,
            Proc(..) => acc[2] += 1,
            Func(..) => acc[3] += 1,
            If(..) => acc[4] += 1,
            While(..) | Repeat(..) | For { .. } => acc[5] += 1,
            Case(_) => acc[6] += 1,
            Assign(..) => acc[7] += 1,
            Expr(_) => acc[8] += 1,
            Return(_) | Halt | Keyword(_) => acc[9] += 1,
        }
        match s {
            If(_, a, b) => {
                count(a, acc);
                count(b, acc);
            }
            While(_, b) | For { body: b, .. } => count(b, acc),
            Repeat(b, _) => count(b, acc),
            Proc(_, _, b) | Func(_, _, b) => count(b, acc),
            Case(arms) => {
                for (_, b) in arms {
                    count(b, acc);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: script_parse <file.s2s>");
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
    match smr::script::parse(&src) {
        Ok(stmts) => {
            let mut acc = [0usize; 10];
            count(&stmts, &mut acc);
            println!("parsed {path}");
            println!("  {} top-level items", stmts.len());
            println!("  var {}  const {}", acc[0], acc[1]);
            println!("  proc {}  func {}", acc[2], acc[3]);
            println!("  if {}", acc[4]);
            println!("  loops {}", acc[5]);
            println!("  case {}", acc[6]);
            println!("  assign {}  expr-stmt {}", acc[7], acc[8]);
            println!("  return/halt/break {}", acc[9]);
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    }
}
