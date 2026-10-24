//! Check this crate's script compiler against Spike2's own, word for word.
//!
//! The fixture holds, for every case in `tools/compile_cases`, the code words that
//! `compiler.dll` produced for that same file. `tools/compile_oracle.py` writes it
//! by running the vendor compiler under Wine; the DLL is CED's and is not in this
//! repository, so the file is gitignored and this test skips when it is absent.
//!
//! A disagreement here is not a style question: the port claims to compile what
//! Spike2 compiles, so the same source has to produce the same words, including
//! the ones the vendor emits for its own reasons.

use std::path::{Path, PathBuf};

/// Cargo runs a test from the package directory, so the fixture is tried at both
/// the package root and the workspace root.
fn find(rel: &str) -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for base in [manifest.to_path_buf(), manifest.parent().unwrap().to_path_buf()] {
        let p = base.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn parse_words(s: &str) -> Option<Vec<u16>> {
    // "WORDS n w0 w1 ..." or "ERR -1".
    if !s.starts_with("WORDS ") {
        return None;
    }
    let mut it = s.split_whitespace().skip(1);
    let n: usize = it.next()?.parse().ok()?;
    let words: Vec<u16> = it.map(|w| w.parse().unwrap()).collect();
    assert_eq!(words.len(), n, "fixture disagrees with its own count: {s}");
    Some(words)
}

#[test]
fn the_port_compiles_what_spike2_compiles() {
    check("testdata/compile_reference.txt", smr::script::compile);
}

/// The same scripts with `runtime.s2l`'s commands registered, as Spike2 compiles them: symbols
/// start after the last command, and a script may call one.
#[test]
fn the_port_compiles_what_spike2_compiles_with_the_commands() {
    check("testdata/compile_reference_cmds.txt", smr::script::compile_with_commands);
}

fn check(
    fixture: &str,
    compile: fn(&str) -> Result<smr::script::Compiled, smr::script::CompileError>,
) {
    let Some(fixture) = find(fixture) else {
        println!("skip: no {fixture} — run tools/compile_oracle.py --emit {fixture}");
        return;
    };
    let text = std::fs::read_to_string(&fixture).unwrap();
    let cases_dir = find("tools/compile_cases").expect("the case files are in the repository");
    let mut checked = 0;
    let mut refused = 0;
    let mut fails = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap();
        let file = parts.next().unwrap();
        let want = parts.next().unwrap();
        let src = std::fs::read_to_string(cases_dir.join(file)).unwrap();
        let got = compile(&src);
        match (parse_words(want), got) {
            (Some(want), Ok(c)) => {
                checked += 1;
                if c.code != want {
                    fails.push(format!(
                        "{name}\n  want {want:?}\n  got  {:?}",
                        c.code
                    ));
                }
            }
            (None, Err(_)) => {
                // The vendor refused this one too: both sides said no.
                refused += 1;
            }
            (Some(want), Err(e)) => {
                fails.push(format!("{name}\n  want {want:?}\n  got  error: {e}"));
            }
            (None, Ok(c)) => {
                fails.push(format!("{name}\n  want an error\n  got  {:?}", c.code));
            }
        }
    }
    assert!(checked > 0, "the fixture held no compiled cases");
    println!("{checked} cases matched, {refused} refused by both sides");
    assert!(fails.is_empty(), "{} differed:\n{}", fails.len(), fails.join("\n"));
}
