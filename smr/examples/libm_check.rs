//! Compare the port's maths functions with the DLL's results.
//!
//!   libm_check <function> <inputs> <outputs>
//!
//! `inputs` and `outputs` are the files tools/libm_oracle.c reads and writes. Prints the first
//! differences and how many calls differed.
use smr::script::vm::msvcrt::call;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (name, inputs, outputs) = (&a[1], &a[2], &a[3]);
    let ins = std::fs::read_to_string(inputs).unwrap();
    let outs = std::fs::read_to_string(outputs).unwrap();
    let (mut n, mut bad) = (0, 0);
    for (line, want) in ins.lines().zip(outs.lines()) {
        let mut it = line.split_whitespace().map(|h| u64::from_str_radix(h, 16).unwrap());
        let x = f64::from_bits(it.next().unwrap());
        let y = f64::from_bits(it.next().unwrap_or(0));
        let want = u64::from_str_radix(want, 16).unwrap();
        let Some(got) = call(name, x, y) else {
            eprintln!("no function {name}");
            std::process::exit(2);
        };
        n += 1;
        // Any NaN answers for any other; a sign or payload that differs is reported too.
        if got.to_bits() != want {
            bad += 1;
            if bad <= std::env::var("SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(8) {
                println!(
                    "{name}({x:e} [{:016x}]{}) want {:016x} got {:016x}",
                    x.to_bits(),
                    if it.next().is_some() || y != 0.0 { format!(", {y:e}") } else { String::new() },
                    want,
                    got.to_bits()
                );
            }
        }
    }
    println!("{name}: {n} calls, {bad} differ");
}
