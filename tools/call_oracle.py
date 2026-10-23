#!/usr/bin/env python3
"""Compare this crate's compiler with Spike2's on calls to built-in commands.

`compile_oracle.py` covers the language. This covers what it does with a command: which
variant of it a call fits, what the call compiles to, and when the vendor refuses. It
builds scripts from the command table, runs the vendor compiler over them with the table
registered in registration order (see `compile_oracle.c`), and writes the answers as a
fixture that `smr/tests/call_reference.rs` replays through `compile_with_commands`:

    ./tools/call_oracle.py --runtime vmbin/runtime.s2l --emit testdata/call_reference.txt

For every command and every form it accepts, a script calls it with arguments of the
right types, with fewer, with one too many, and with each argument in turn replaced by a
literal, a variable, an array, an element or an expression of each type. That is what
exercises the matcher: the vendor's rules for what fits a type word are not documented
anywhere, and the port transcribes them from the compiler.

One case per line in the fixture, tab-separated, newlines in the source written as `\\n`:

    <source> TAB <WORDS n w0 w1 ...>|<ERR code>

Needs `capstone` (for tools/command_table.py), wine, and ORACLE_DIR holding
`compile_oracle.exe` (built from `compile_oracle.c`), `compiler.dll` and `SpikeUtl.dll`.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORACLE_DIR = os.environ.get("ORACLE_DIR", os.path.expanduser("~/spike8-re/oracle/dlls"))
WINE_PREFIX = os.environ.get("WINE_CFIR_PREFIX", os.path.expanduser("~/.wine-cfir"))

PRELUDE = (
    "var i%; var r; var s$; const c%:=1; "
    "var ai%[3]; var ar[3]; var as$[3]; var ai2%[3][3]; var ar2[3][3]; var as2$[3][3]; "
    "var ar3[3][3][3];\n"
)
# What fills an argument slot, by the name of the shape.
SHAPES = {
    "lit_i": "1", "lit_r": "1.5", "lit_s": '"x"',
    "var_i": "i%", "var_r": "r", "var_s": "s$", "const": "c%",
    "el_i": "ai%[0]", "el_r": "ar[0]", "el_s": "as$[0]", "el2": "ar2[0][1]",
    "arr_i": "ai%[]", "arr_r": "ar[]", "arr_s": "as$[]",
    "arr2_i": "ai2%[][]", "arr2_r": "ar2[][]", "arr3": "ar3[][][]",
    "ex_i": "i%+1", "ex_r": "r*2", "ex_s": 's$+"x"', "paren": "(i%)", "call": "Abs(r)",
}
# The expression that suits each type word when a call should be well formed.
SCALAR = {1: "1", 2: "1.5", 3: '"x"'}
VARS = {1: "i%", 2: "r", 3: "s$"}
ARRS = {
    (1, 1): "ai%[]", (2, 1): "ar[]", (3, 1): "as$[]",
    (1, 2): "ai2%[][]", (2, 2): "ar2[][]", (3, 2): "as2$[][]",
    (2, 3): "ar3[][][]",
}


def good_arg(word):
    """An expression that fits `word`, or None when no single one is known."""
    t, dims, flag = word & 0x7f, (word >> 8) & 0x1f, word & 0x80
    if dims:
        rank = next(k for k in range(1, 6) if dims & (1 << (k - 1)))
        elem = {1: 1, 2: 2, 3: 3, 6: 2, 4: 2, 0x11: 2}.get(t, 2)
        if (elem, rank) in ARRS:
            return ARRS[(elem, rank)]
        return ARRS.get((elem, 1))
    if t in (1, 2, 3):
        return SCALAR[t]
    if t == 4:
        return "1"
    if t in (6, 0x11, 0x10):
        return "1.5"
    if t in (8, 9, 0xa, 0xb, 0xc):
        return VARS[{8: 1, 9: 1, 0xa: 2, 0xb: 3, 0xc: 1}[t]]
    if t == 0x12:
        return "ar[]"
    if t == 0x13:
        return "as$[]"
    return "1"


def call(cmd, args):
    c = f"{cmd['name']}({','.join(args)})"
    return {0: c + ";", 1: "i% := " + c + ";", 2: "r := " + c + ";", 3: "s$ := " + c + ";"}[cmd["ret"]]


def cases(cmds):
    seen = set()

    def add(stmt):
        src = PRELUDE + stmt + "\n"
        if src not in seen:
            seen.add(src)
            yield src

    for cmd in cmds:
        for v in cmd["variants"][:4]:
            words = [w for w in v if w not in (0xe, 0xf)]
            if len(words) != len(v):
                # a procedure is passed: only the plain call
                yield from add(f"{cmd['name']}();")
                continue
            base = [good_arg(w) for w in words]
            if any(b is None for b in base):
                continue
            yield from add(call(cmd, base))
            lo = cmd["min"]
            yield from add(call(cmd, base[:lo]))
            yield from add(call(cmd, base[:max(lo - 1, 0)])) if lo else iter(())
            yield from add(call(cmd, base + ["1"]))
            for k in range(min(len(base), 3)):
                for name, expr in SHAPES.items():
                    yield from add(call(cmd, base[:k] + [expr] + base[k + 1:]))


def write_cmds(cmds, path):
    with open(path, "w") as f:
        for c in cmds:
            words = [c["ret"]]
            for k, v in enumerate(c["variants"]):
                words += v + [7 if k == len(c["variants"]) - 1 else 5]
            f.write(" ".join(map(str, [c["name"], c["min"], c["max"], len(words)] + words)) + "\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runtime", required=True, help="runtime.s2l, to read the command table from")
    ap.add_argument("--emit", help="write the fixture here")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()
    cmds = json.loads(
        subprocess.run([sys.executable, os.path.join(ROOT, "tools", "command_table.py"), args.runtime],
                       capture_output=True, text=True, check=True).stdout
    )
    srcs = list(cases(cmds))
    if args.limit:
        srcs = srcs[:args.limit]
    print(f"{len(cmds)} commands, {len(srcs)} scripts", file=sys.stderr)
    env = dict(os.environ, WINEPREFIX=WINE_PREFIX, WINEDEBUG="-all")
    rows = []
    with tempfile.TemporaryDirectory() as tmp:
        cmdfile = os.path.join(tmp, "commands.txt")
        write_cmds(cmds, cmdfile)
        env["CMDS"] = cmdfile
        for a in range(0, len(srcs), 300):
            chunk = srcs[a:a + 300]
            files = []
            for k, s in enumerate(chunk):
                p = os.path.join(tmp, f"{a + k:06d}.s2s")
                open(p, "w").write(s)
                files.append(p)
            out = subprocess.run(["wine", os.path.join(ORACLE_DIR, "compile_oracle.exe")] + files,
                                 cwd=ORACLE_DIR, env=env, capture_output=True, text=True, timeout=900).stdout
            lines = [l.strip() for l in out.splitlines() if l.startswith(("WORDS", "ERR"))]
            if len(lines) != len(chunk):
                sys.exit(f"expected {len(chunk)} results, got {len(lines)}")
            for s, l in zip(chunk, lines):
                rows.append(s.replace("\n", "\\n") + "\t" + l)
    ok = sum(1 for r in rows if r.split("\t")[1].startswith("WORDS"))
    if args.emit:
        with open(args.emit, "w") as f:
            f.write("\n".join(rows) + "\n")
        print(f"wrote {args.emit}: {ok} compiled, {len(rows) - ok} refused", file=sys.stderr)
    else:
        print("\n".join(rows))


if __name__ == "__main__":
    main()
