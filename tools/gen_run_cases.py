#!/usr/bin/env python3
"""Write the generated test programs in tools/run_cases (those named g_*.s2s).

Each calls a command over a grid of inputs and stores every result in the global array
`res`, which `tools/run_oracle.py` reads back from Spike2 and `smr/tests/run_reference.rs`
compares with this crate's interpreter. A string result is stored as its length followed by
its character codes, by the `Enc` procedure each program includes.

Inputs that make Spike2 raise a script error (a negative length, the log of zero) stop its
whole batch, so they are kept out of the grids and tried one program each in `risky_*`.

    ./tools/gen_run_cases.py
"""
import os

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools", "run_cases")

ENC = """proc Enc(s$, at%)
  var i%;
  res[at%] := Len(s$);
  for i% := 1 to Len(s$) do res[at% + i%] := Asc(Mid$(s$, i%, 1)) next
end;
"""


def q(s):
    return '"' + s + '"'


def write(name, body, size, enc=False):
    src = f"var res[{size}];\n" + (ENC if enc else "") + body
    with open(os.path.join(OUT, f"{name}.s2s"), "w") as f:
        f.write(src)


def numeric(name, exprs):
    """res[k] := expr_k for each expression."""
    body = "".join(f"res[{k}] := {e};\n" for k, e in enumerate(exprs))
    write(name, body, max(len(exprs), 1))


def strings(name, exprs, width=40):
    """Each expression is a string; stored with Enc at k * width."""
    body = "".join(f"Enc({e}, {k * width});\n" for k, e in enumerate(exprs))
    write(name, body, len(exprs) * width, enc=True)


S = ["", "a", "Hello", "Hello, World", "  pad  ", "MiXeD 123"]
X = [0, 0.5, 1, 2, 2.5, -0.5, -1, -2.5, 3.75, 10, 100, 1e6, 0.001, 3.14159265358979, 1e10, -7.5]
POS = [1, 2, 3, 5, 20]
CNT = [0, 1, 2, 3, 50]


def formatting():
    xs = [0, 1, -1, 0.5, 2.5, -0.5, 3.75, 10, 100, 1234.5678, 1e6, 1e10, 1e-3, 1e-6, 123456789, 0.1, 1/3]
    strings("g_str1", [f"Str$({x!r})" for x in xs], 32)
    strings("g_str2", [f"Str$({x!r}, {w})" for x in [3.14159, -2.5, 42, 1234567.891] for w in [1, 4, 8, 12]], 32)
    strings("g_str3", [f"Str$({x!r}, {w}, {d})" for x in [3.14159, -2.5, 42, 0.000123] for w in [1, 8, 12] for d in [0, 2, 5]], 32)
    strings("g_print1", [f'Print$("{f}", {v})' for f, v in [
        ("%d", 5), ("%d", -42), ("%5d", 42), ("%-5d|", 42), ("%05d", 42), ("%x", 255), ("%X", 255), ("%o", 8),
        ("%f", 3.14159), ("%.2f", 3.14159), ("%8.3f", -2.5), ("%e", 12345.678), ("%g", 1e10), ("%g", 0.0001),
        ("%g", 123456789), ("%c", 65), ("%s", '"abc"'), ("%5s|", '"ab"'), ("%-5s|", '"ab"'), ("%%", 1),
        ("val=%d", 7), ("%d%%", 50)]], 32)
    strings("g_print2", [f'Print$({a})' for a in ['"plain"', '"two\\nlines"', '"a%d b%d", 1, 2', '"%s=%g", "x", 2.5',
                                                  '"%d %d %d", 1, 2, 3', '"%5.1f|%-6d|", 2.25, 17']], 32)


def formatting2():
    items = ['"a", 1', '"a", 1, 2', '"a", 1.5', '"a", "b"', '"%d", 1, 2', '"%d %d", 1', '"%s", 5', '"%d", 3.7', '"%f", 3',
             '"%5c|", 66', '"%+d", 5', '"% d", 5', '"%#x", 255', '"%.3s", "abcdef"', '"%10.4e|", 1234.5678', '"%G", 1e-10',
             '"%E", 1.5', '"%5.3d|", 7', '"%lf", 2.5', '"%i", 9', '"%u", 12', '"%.3g", 1234.5678', '"%#g", 1.5', '"%08.3f", -3.14159',
             '"%x", -1', '"%d", 1e10', '"%s", 2.5', '"%c%c", 72, 105', '"[%*d]", 5, 3']
    strings("g_print3", [f"Print$({a})" for a in items], 40)


def main():
    for f in os.listdir(OUT):
        if f.startswith("g_"):
            os.remove(os.path.join(OUT, f))
    formatting()
    formatting2()

    strings("g_left_right", [f"Left$({q(s)}, {n})" for s in S for n in [0, 1, 3, 50]]
            + [f"Right$({q(s)}, {n})" for s in S for n in [0, 1, 3, 50]], 16)
    strings("g_mid", [f"Mid$({q(s)}, {p})" for s in S for p in POS]
            + [f"Mid$({q(s)}, {p}, {n})" for s in S for p in [1, 3, 20] for n in CNT], 16)
    strings("g_delstr", [f"DelStr$({q(s)}, {p}, {n})" for s in S for p in [1, 3, 20] for n in CNT], 16)
    strings("g_case", [f"LCase$({q(s)})" for s in S] + [f"UCase$({q(s)})" for s in S], 16)
    strings("g_chr", [f"Chr$({n})" for n in [65, 97, 48, 32, 126, 200, 255]], 4)
    numeric("g_instr", [f"InStr({q(s)}, {q(f)})" for s in S for f in ["", "a", "l", "lo", "World", "zz", " "]]
            + [f"InStr({q(s)}, {q(f)}, {p})" for s in ["Hello, World", "aaaa"] for f in ["a", "l", "o"] for p in [1, 2, 4, 9]])
    numeric("g_asc_len", [f"Asc({q(s)})" for s in S] + [f"Len({q(s)})" for s in S])
    numeric("g_val", [f"Val({q(v)})" for v in
                      ["12", "3.5abc", "-2e2x", "abc", "  7", ".5", "1e", "0x10", "+4", "- 3", "1,5", "", "1e5", "1.5E-2", "007"]])

    unary = {
        "Abs": X, "Trunc": X, "Floor": X, "Ceil": X, "Frac": X, "Round": X,
        "Sin": X, "Cos": X, "Tan": [x for x in X if abs(x) < 1e7], "Sinh": X[:12], "Cosh": X[:12], "Tanh": X,
        "Exp": [x for x in X if x < 50], "ATan": X,
        "Sqrt": [x for x in X if x >= 0], "Ln": [x for x in X if x > 0], "Log": [x for x in X if x > 0],
    }
    for fn, xs in unary.items():
        numeric(f"g_{fn.lower()}", [f"{fn}({x!r})" for x in xs])
    numeric("g_atan2", [f"ATan({y!r}, {x!r})" for y in [-2, -0.5, 0, 0.5, 3] for x in [-2, -0.5, 0.5, 3]])
    numeric("g_pow", [f"Pow({x!r}, {y!r})" for x in [0.5, 1, 2, 2.5, 10] for y in [-2, -0.5, 0, 0.5, 1, 3]])
    numeric("g_minmax", [f"Max({a!r}, {b!r})" for a in [-1, 0, 2.5] for b in [-3, 0, 2.5, 7]]
            + [f"Min({a!r}, {b!r})" for a in [-1, 0, 2.5] for b in [-3, 0, 2.5, 7]]
            + ["Max(1, 5, 3)", "Min(4, 2, 8, 6)", "Max(2.5, 1)", "Min(7, 7.5)"])

    # in-place forms on arrays
    body = ("var a[4];\nvar k%;\n"
            "a[0] := 0.5; a[1] := 1; a[2] := 2.5; a[3] := -3;\n"
            "Abs(a[]);\nfor k% := 0 to 3 do res[k%] := a[k%] next\n"
            "Cos(a[]);\nfor k% := 0 to 3 do res[4 + k%] := a[k%] next\n"
            "a[0] := 0.5; a[1] := 1; a[2] := 2.5; a[3] := -3;\n"
            "Max(a[]);\nres[8] := Max(a[]); res[9] := Min(a[]);\n")
    write("g_array_math", body, 10)
    # Max and Min of an array
    body = ("var a[5], b%[4], c[1];\n"
            "a[0] := 1.5; a[1] := 7; a[2] := -2; a[3] := 7; a[4] := -2;\n"
            "b%[0] := 3; b%[1] := 9; b%[2] := -4; b%[3] := 9;\n"
            "c[0] := 4.5;\n"
            "res[0] := Max(a[]); res[1] := Min(a[]); res[2] := Max(b%[]); res[3] := Min(b%[]);\n"
            "res[4] := Max(c[]); res[5] := Min(c[]);\n")
    write("g_array_minmax", body, 6)


if __name__ == "__main__":
    main()
