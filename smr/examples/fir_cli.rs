//! Design a filter on stdin and print it in the same shape as the CED oracle,
//! so the two can be diffed.
//!
//! ```text
//! nfilt jtype nbands lgrid flag  lo hi fx wt  [lo hi fx wt ...]
//! ```
//!
//! `jtype` is the `CFIR::SetFilt` number: 0 low pass, 4 differentiator, 5 band
//! pass, 6 band stop, 7 high pass. Output is `dev err ncoef c0 c1 ...`.

use std::io::{self, BufRead};

fn main() {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut it = line.split_whitespace();
        let nfilt: usize = match it.next().and_then(|v| v.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let jtype: i32 = it.next().unwrap().parse().unwrap();
        let nbands: usize = it.next().unwrap().parse().unwrap();
        let lgrid: i32 = it.next().unwrap().parse().unwrap();
        let flag: i32 = it.next().unwrap().parse().unwrap();

        let mut bands = Vec::with_capacity(nbands);
        for _ in 0..nbands {
            let mut v = [0.0f64; 4];
            for x in v.iter_mut() {
                *x = it.next().unwrap().parse().unwrap();
            }
            bands.push(smr::Band::new(v[0], v[1], v[2], v[3]));
        }

        let t = match jtype {
            0 | 1 | 2 | 3 => smr::DesignType::LowPass,
            4 => smr::DesignType::Differentiator,
            _ => {
                println!("-1 8 0");
                continue;
            }
        };

        match smr::design(nfilt, t, &bands, lgrid, flag != 0) {
            Ok(d) => {
                print!("{:<24} 0 {}", format!("{:.17e}", d.dev), d.coeffs.len());
                for c in &d.coeffs {
                    print!(" {:.17e}", c);
                }
                println!();
                if std::env::var("FIR_EXT").is_ok() {
                    print!("EXT {}", d.extrema.len());
                    for e in &d.extrema {
                        print!(" {:.17e}", e);
                    }
                    println!();
                }
            }
            Err(e) => println!("-1 16 0 # {e}"),
        }
    }
}
