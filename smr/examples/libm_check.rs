//! Compare the port's maths functions with the DLL's results.
//!
//!   libm_check <function> <inputs> <outputs>
//!
//! `inputs` and `outputs` are the files tools/libm_oracle.c and tools/util_oracle.c read and write. Prints the first
//! differences and how many calls differed.
use smr::script::vm::{msvcrt, special};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (name, inputs, outputs) = (&a[1], &a[2], &a[3]);
    let ins = std::fs::read_to_string(inputs).unwrap();
    let outs = std::fs::read_to_string(outputs).unwrap();
    let (mut n, mut bad) = (0, 0);
    for (line, want) in ins.lines().zip(outs.lines()) {
        let mut it = line.split_whitespace().map(|h| u64::from_str_radix(h, 16).unwrap());
        let words: Vec<u64> = it.by_ref().collect();
        let int_args = name == "BinomialC";
        let arg = |k: usize| -> f64 {
            let w = words.get(k).copied().unwrap_or(0);
            if int_args { w as u32 as i32 as f64 } else { f64::from_bits(w) }
        };
        let (x, y, z) = (arg(0), arg(1), arg(2));
        let want = u64::from_str_radix(want, 16).unwrap();
        let Some(got) = msvcrt::call(name, x, y).or_else(|| special::call(name, x, y, z)) else {
            eprintln!("no function {name}");
            std::process::exit(2);
        };
        n += 1;
        // Any NaN answers for any other; a sign or payload that differs is reported too.
        if got.to_bits() != want {
            bad += 1;
            if bad <= std::env::var("SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(8) {
                println!(
                    "{name}({x:e} [{:016x}], {y:e}, {z:e}) want {:016x} got {:016x}",
                    x.to_bits(),
                    want,
                    got.to_bits()
                );
            }
        }
    }
    println!("{name}: {n} calls, {bad} differ");
}
