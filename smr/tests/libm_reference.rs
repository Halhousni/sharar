//! The maths functions give the bits msvcr110.dll gives, for the arguments in
//! testdata/libm_reference.txt (made by `tools/libm_fuzz.py --emit`, which runs the DLL).

use smr::script::vm::msvcrt::call;

#[test]
fn the_maths_functions_match_the_dll() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("testdata/libm_reference.txt");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("skip: no testdata/libm_reference.txt — run tools/libm_fuzz.py --emit");
        return;
    };
    let (mut n, mut bad) = (0, Vec::new());
    for line in text.lines().filter(|l| !l.is_empty()) {
        let w: Vec<&str> = line.split(' ').collect();
        let hex = |s: &str| u64::from_str_radix(s, 16).unwrap();
        let (name, args, want) = (w[0], &w[1..w.len() - 1], hex(w[w.len() - 1]));
        let a = f64::from_bits(hex(args[0]));
        let b = args.get(1).map(|s| f64::from_bits(hex(s))).unwrap_or(0.0);
        let got = call(name, a, b).unwrap().to_bits();
        n += 1;
        if got != want {
            bad.push(format!("{name}({a:e}, {b:e}): want {want:016x} got {got:016x}"));
        }
    }
    assert!(n > 0);
    println!("{n} calls");
    assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}
