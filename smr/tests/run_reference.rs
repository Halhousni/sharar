//! Check what scripts compute against Spike2 itself.
//!
//! `tools/run_oracle.py` runs every program in `tools/run_cases` inside Spike2 and records
//! the global array `res` each one fills. This runs the same programs through the port's
//! compiler and interpreter and compares every element bit for bit. The fixture is
//! gitignored because it needs a Spike2 licence to produce; the test skips without it.
//!
//! A program that stops Spike2 with a script error is recorded as `ERR`; the port has to
//! stop on it too. A program the port cannot run yet (a command not ported) is counted
//! apart, so the number of those is visible rather than hidden in a pass.

use smr::script::session::Session;
use smr::script::vm::{Stop, Vm, V};
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

fn bits(v: &V) -> u64 {
    match v {
        V::Int(i) => (*i as f64).to_bits(),
        V::Flt(f) => f.to_bits(),
        V::Str(_) => u64::MAX,
    }
}

#[test]
fn programs_compute_what_spike2_computes() {
    let Some(fixture) = find("testdata/run_reference.txt") else {
        println!("skip: no testdata/run_reference.txt — run tools/run_oracle.py --emit");
        return;
    };
    let cases = find("tools/run_cases").expect("the programs are in the repository");
    let text = std::fs::read_to_string(fixture).unwrap();
    let (mut same, mut both_stop, mut unported) = (0, 0, Vec::new());
    let mut fails = Vec::new();
    // Files the programs wrote in Spike2, and where the port's runs write theirs.
    let out_dir = std::env::temp_dir().join(format!("sharar_run_out_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (name, want) = line.split_once('\t').unwrap();
        if let Some(file) = name.strip_prefix("@file ") {
            let bytes = (0..want.len() / 2).map(|i| u8::from_str_radix(&want[2 * i..2 * i + 2], 16).unwrap()).collect();
            files.push((file.to_string(), bytes));
            continue;
        }
        let src = std::fs::read_to_string(cases.join(format!("{name}.s2s"))).unwrap();
        let ran = smr::script::compile_with_commands(&src)
            .map_err(|e| format!("does not compile: {}", e.0))
            .and_then(|c| Vm::new(c, true))
            .map(|vm| {
                // The scripts name the guest's data folder; the same recordings are here.
                let mut session = Session::new();
                if let Some(dir) = find("testdata/Example_Data_Files") {
                    session.map_path(r"C:\Users\Hilal\Documents\Spike8\data\", dir);
                }
                session.map_path(r"C:\Users\Hilal\Documents\Spike8\out\", out_dir.clone());
                vm.with_host(Box::new(session))
            })
            .and_then(|mut vm| match vm.run() {
                Stop::Done => Ok(Some(vm)),
                Stop::Error(_) => Ok(None),
                Stop::Command(c) => Err(format!("unported command {c}")),
                other => Err(format!("{other:?}")),
            });
        match (want, ran) {
            ("ERR", Ok(None)) => both_stop += 1,
            (_, Err(why)) => {
                if why.starts_with("unported") {
                    unported.push(format!("{name}: {why}"));
                } else {
                    fails.push(format!("{name}: {why}"));
                }
            }
            ("ERR", Ok(Some(_))) => fails.push(format!("{name}: Spike2 stopped, the port ran to the end")),
            (_, Ok(None)) => fails.push(format!("{name}: the port stopped on an error, Spike2 did not")),
            (want, Ok(Some(vm))) => {
                let want: Vec<u64> =
                    want.split_whitespace().map(|h| u64::from_str_radix(h, 16).unwrap()).collect();
                let got: Vec<u64> = vm.array("res").unwrap().iter().map(bits).collect();
                let nan = |b: u64| f64::from_bits(b).is_nan();
                let diff: Vec<usize> = (0..want.len().max(got.len()))
                    .filter(|&i| match (want.get(i), got.get(i)) {
                        (Some(&w), Some(&g)) => {
                            w != g
                                && !(nan(w) && nan(g))
                        }
                        _ => true,
                    })
                    .collect();
                if diff.is_empty() {
                    same += 1;
                } else {
                    let show = |v: &[u64], i: usize| v.get(i).map(|&b| f64::from_bits(b).to_string()).unwrap_or("-".into());
                    fails.push(format!(
                        "{name}: res differs at {}",
                        diff.iter()
                            .take(6)
                            .map(|&i| format!("[{i}] spike2 {} port {}", show(&want, i), show(&got, i)))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            }
        }
    }
    for (name, want) in &files {
        match std::fs::read(out_dir.join(name)) {
            Ok(got) if &got == want => same += 1,
            Ok(got) => fails.push(format!(
                "file {name}: Spike2 wrote {} bytes, the port {} bytes{}",
                want.len(),
                got.len(),
                first_difference(want, &got)
            )),
            Err(_) => fails.push(format!("file {name}: Spike2 wrote it ({} bytes), the port did not", want.len())),
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    println!(
        "{same} programs agree, {both_stop} stop in both, {} use a command not ported yet, {} differ",
        unported.len(),
        fails.len()
    );
    for u in unported.iter().take(20) {
        println!("  not ported: {u}");
    }
    for f in &fails {
        println!("  DIFFERS {f}");
    }
    assert!(fails.is_empty(), "{} programs behave differently from Spike2", fails.len());
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("; first differ at byte {i} ({:#04x} vs {:#04x})", a[i], b[i]),
        None => String::new(),
    }
}
