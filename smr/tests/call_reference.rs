//! Check calls to built-in commands against Spike2's own compiler, word for word.
//!
//! The fixture holds, for each script, the words `compiler.dll` produced with the whole
//! command table registered, or its refusal. `tools/call_oracle.py` writes it; the DLL
//! is CED's and is not in this repository, so the file is gitignored and this test skips
//! when it is absent.
//!
//! The scripts call every command with arguments of the right types, with too few and
//! too many, and with each argument replaced by every shape an argument can take, so a
//! disagreement is a rule of the vendor's matcher that was transcribed wrongly. Calls
//! that pass a procedure to a command are refused here by name and counted apart.

use std::path::{Path, PathBuf};

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
fn the_port_compiles_calls_as_spike2_does() {
    let Some(fixture) = find("testdata/call_reference.txt") else {
        println!("skip: no testdata/call_reference.txt — run tools/call_oracle.py --emit");
        return;
    };
    let text = std::fs::read_to_string(&fixture).unwrap();
    let (mut same, mut both_refuse, mut unsupported) = (0, 0, 0);
    let mut fails = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (src, want) = line.split_once('\t').unwrap();
        let src = src.replace("\\n", "\n");
        match (parse_words(want), smr::script::compile_with_commands(&src)) {
            (Some(w), Ok(c)) if c.code == w => same += 1,
            (None, Err(_)) => both_refuse += 1,
            (_, Err(e)) if e.0.contains("not compiled yet") => unsupported += 1,
            (w, got) => fails.push(format!(
                "{}\n   vendor: {}\n   port:   {}",
                src.lines().last().unwrap_or(""),
                w.map(|w| format!("{w:?}")).unwrap_or_else(|| want.to_string()),
                match got {
                    Ok(c) => format!("{:?}", c.code),
                    Err(e) => format!("refused: {}", e.0),
                }
            )),
        }
    }
    println!(
        "{same} compiled alike, {both_refuse} refused by both, {unsupported} not compiled yet, {} differ",
        fails.len()
    );
    for f in fails.iter().take(40) {
        println!("{f}");
    }
    assert!(fails.is_empty(), "{} calls compile differently from the vendor's", fails.len());
}
