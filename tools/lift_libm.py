#!/usr/bin/env python3
"""Translate msvcr110.dll's maths routines into Rust.

    tools/lift_libm.py vmbin/msvcr110.dll smr/src/script/vm

Writes `msvcr110_gen.rs` and `msvcr110.dat` (see tools/lift.py). Needs `capstone`."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lift import Lifter

ROOTS = {
    "sin": 0x1800867d0, "cos": 0x180082660, "tan": 0x180087adc, "exp": 0x1800836b0,
    "log": 0x1800847c0, "log10": 0x180084a80, "pow": 0x1800853e0, "atan": 0x180081398,
    "atan2": 0x1800815f8, "sinh": 0x180086fe0, "cosh": 0x180082df0, "tanh": 0x180088678,
    "asin": 0x180080e7c, "acos": 0x180080908, "hypot": 0x18007f6f4,
}

# Callees that are not translated, and what they amount to here. They are the DLL's error
# reporting (set errno, raise an FP exception, call a user `_matherr`), and each answers with the
# value it was given to return.
STUBS = {
    0x18008b808: "",                                             # _set_statfp
    0x18006d8a0: "",                                             # __security_check_cookie
    0x18008cce8: "m.set_xl(0, m.xl(1));",                        # exp error
    0x18008cee0: "m.set_xl(0, m.xl(1));",                        # log error
    0x18008cea0: "m.set_xl(0, m.xl(1));",                        # log10 error
    0x18008d100: "m.set_xl(0, m.xl(2));",                        # pow error
    0x180088db4: "m.set_xl(0, m.r[8]);",                         # the general error report
    0x18008c8e0: "m.set_xl(0, super::msvcrt::invalid(m.xl(0)).to_bits());",  # sin of inf/nan
    0x18008c8b8: "m.set_xl(0, super::msvcrt::invalid(m.xl(0)).to_bits());",  # cos of inf/nan
}

lifter = Lifter(sys.argv[1], ROOTS.values(), STUBS, {}, r"$^")
wrappers = []
for name, s in ROOTS.items():
    two = name in ("pow", "atan2", "hypot")
    wrappers.append(f"""
pub fn {name}({'x: f64, y: f64' if two else 'x: f64'}) -> f64 {{
    let mut m = M::new(&IMG);
    m.set_xl(0, x.to_bits());{chr(10) + '    m.set_xl(1, y.to_bits());' if two else ''}
    m.push(0);
    f_{s:x}(&mut m);
    f64::from_bits(m.xl(0))
}}""")
lifter.write(sys.argv[2], "msvcr110.dll", "lift_libm.py", "\n".join(wrappers))
