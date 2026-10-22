//! Check the Parks-McClellan port against CED's own designer.
//!
//! `tools/fir_oracle.py --emit testdata/fir_reference.txt` records a set of
//! designs together with the coefficients CED's `Filters.dll` produces for
//! them. The DLL is not redistributable, so that fixture is not in the
//! repository: without it this test prints a skip and returns, and a fresh
//! clone still goes green. The self-consistency tests in `smr/src/fir.rs` run
//! either way.
//!
//! Every design in the fixture agrees to within 1e-12 relative, which for these
//! coefficient sizes is the last two or three bits of the mantissa.

use std::path::Path;

use smr::{design, Band, DesignType};

/// Relative tolerance. The exchange is a fixed sequence of floating-point
/// operations, so agreement is far tighter than this; the margin is there for
/// a libm that rounds `cos` or `acos` differently at the last bit.
const TOL: f64 = 1e-12;

fn parse_type(jtype: i32) -> Option<DesignType> {
    // SetFilt collapses everything below 4 to 0, so only the differentiator
    // needs its own case.
    match jtype {
        0 | 1 | 2 | 3 | 6 => Some(DesignType::LowPass),
        4 => Some(DesignType::Differentiator),
        _ => None,
    }
}

#[test]
fn coefficients_match_ceds_designer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata/fir_reference.txt");
    if !path.exists() {
        println!(
            "skip: no {} — run tools/fir_oracle.py --emit {} with CED's Filters.dll",
            path.display(),
            path.display()
        );
        return;
    }
    let text = std::fs::read_to_string(&path).expect("read fixture");
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert!(lines.len() % 2 == 0, "fixture should be pairs of lines");

    let mut checked = 0;
    let mut worst: f64 = 0.0;
    for pair in lines.chunks(2) {
        let spec: Vec<f64> = pair[0]
            .split_whitespace()
            .map(|v| v.parse().expect("number"))
            .collect();
        let nfilt = spec[0] as usize;
        let jtype = spec[1] as i32;
        let nbands = spec[2] as usize;
        let lgrid = spec[3] as i32;
        let retry = spec[4] != 0.0;
        let mut bands = Vec::with_capacity(nbands);
        for b in 0..nbands {
            let o = 5 + b * 4;
            bands.push(Band::new(spec[o], spec[o + 1], spec[o + 2], spec[o + 3]));
        }

        let expect: Vec<f64> = pair[1]
            .split_whitespace()
            .map(|v| v.parse().expect("number"))
            .collect();
        // Columns are dev, err, n, then the coefficients.
        let want_dev = expect[0];
        let want = &expect[3..];

        // A design the vendor rejects is recorded as dev 1.0 with no usable
        // coefficients; skip those, they are not a coefficient comparison.
        if !want_dev.is_finite() || want_dev == 1.0 || want.iter().any(|v| !v.is_finite()) {
            continue;
        }

        let Some(t) = parse_type(jtype) else { continue };
        let got = match design(nfilt, t, &bands, lgrid, retry) {
            Ok(d) => d,
            Err(e) => panic!("design {checked} ({nfilt} taps, jtype {jtype}, lgrid {lgrid}): {e}"),
        };
        assert_eq!(got.coeffs.len(), want.len(), "coefficient count for {nfilt} taps");
        let scale = want.iter().fold(1e-30f64, |a, v| a.max(v.abs()));
        for (i, (g, w)) in got.coeffs.iter().zip(want).enumerate() {
            let rel = (g - w).abs() / scale;
            worst = worst.max(rel);
            assert!(rel <= TOL, "design {checked}, tap {i}: {g} vs {w} (rel {rel:.3e})");
        }
        checked += 1;
    }
    println!("{checked} designs matched, worst relative difference {worst:.3e}");
    assert!(checked > 0, "fixture had no usable designs");
}
