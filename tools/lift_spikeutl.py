#!/usr/bin/env python3
"""Translate SpikeUtl.dll's numeric routines into Rust.

    tools/lift_spikeutl.py vmbin/SpikeUtl.dll smr/src/script/vm

Writes `spikeutl_gen.rs` and `spikeutl.dat` (see tools/lift.py). Needs `capstone`."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lift import Lifter

ROOTS = {
    "ln_gamma": (0x1800adf80, "x: f64", ["xmm:x"], "f64"),
    "gamma_p": (0x1800adfa0, "a: f64, x: f64", ["xmm:a", "xmm:x"], "f64"),
    "gamma_q": (0x1800adfd0, "a: f64, x: f64", ["xmm:a", "xmm:x"], "f64"),
    "beta": (0x18000b120, "a: f64, b: f64", ["xmm:a", "xmm:b"], "f64"),
    "beta_i": (0x18000b140, "a: f64, b: f64, x: f64", ["xmm:a", "xmm:b", "xmm:x"], "f64"),
    "binomial_c": (0x18000b160, "n: i32, k: i32", ["int:n", "int:k"], "f64"),
}

def libm1(fn):
    return f"m.set_xl(0, super::msvcrt::{fn}(f64::from_bits(m.xl(0))).to_bits());"

def libm2(fn):
    return f"m.set_xl(0, super::msvcrt::{fn}(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"

IMPORTS = {n: libm1(n) for n in ("sin", "cos", "tan", "sinh", "cosh", "exp", "log", "log10", "asin", "acos")}
IMPORTS.update({"pow": libm2("pow"), "atan2": libm2("atan2"), "_hypot": libm2("hypot")})
IMPORTS["memset"] = "m.memset();"
IMPORTS["memcpy"] = IMPORTS["memmove"] = "m.memcpy();"
IMPORTS["fmod"] = "m.set_xl(0, (f64::from_bits(m.xl(0)) % f64::from_bits(m.xl(1))).to_bits());"

# Calling any of these means a C++ exception or a stream is being built: an error report.
FAIL = (r"exception|_Xlength|_Xout|_Xbad|basic_ios|basic_ostream|basic_streambuf|setprecision|ctype|locale"
        r"|_Lockit|uncaught|bad_cast|_CxxThrow|_purecall|abort")

lifter = Lifter(sys.argv[1], [r[0] for r in ROOTS.values()], {}, IMPORTS, FAIL)
wrappers = []
for name, (addr, params, args, ret) in ROOTS.items():
    body, xi, ri = [], 0, [1, 2, 8, 9]
    for a in args:
        kind, var = a.split(":")
        if kind == "xmm":
            body.append(f"m.set_xl({xi}, {var}.to_bits());")
            xi += 1
        else:
            body.append(f"m.r[{ri.pop(0)}] = {var} as u32 as u64;")
    wrappers.append(f"""
pub fn {name}({params}) -> {ret} {{
    let mut m = M::new(&IMG);
    {' '.join(body)}
    m.push(0);
    f_{addr:x}(&mut m);
    f64::from_bits(m.xl(0))
}}""")
lifter.write(sys.argv[2], "SpikeUtl.dll", "lift_spikeutl.py", "\n".join(wrappers))
