#!/usr/bin/env python3
"""Compare this crate's script compiler with Spike2's own, word for word.

Spike2's compiler lives in `compiler.dll`. `compile_oracle.c`, kept outside this
repository, loads that DLL under Wine and prints the code words the vendor's
compiler produces for a `.s2s` file. This script runs it over every case in
`tools/compile_cases/` and writes the answers as a test fixture:

    ./tools/compile_oracle.py --emit testdata/compile_reference.txt

The DLL is not redistributable, so the harness lives outside the tree; point
ORACLE_DIR at the directory holding `compile_oracle.exe`, `compiler.dll` and
`SpikeUtl.dll`. Without it the fixture cannot be regenerated, which is why the
test skips when the fixture is absent.

One case per line in the fixture, tab-separated:

    <case name> TAB <name of the file> TAB <WORDS n w0 w1 ...>|[<ERR ...>]
"""
import argparse
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = os.path.join(ROOT, "tools", "compile_cases")
ORACLE_DIR = os.environ.get("ORACLE_DIR", os.path.expanduser("~/spike8-re/oracle/dlls"))
WINE_PREFIX = os.environ.get("WINE_CFIR_PREFIX", os.path.expanduser("~/.wine-cfir"))


def cases():
    for name in sorted(os.listdir(CASES)):
        if name.endswith(".s2s"):
            yield os.path.join(CASES, name)


def run(files):
    """One vendor process over every case; the pools do not leak between files."""
    env = dict(os.environ, WINEPREFIX=WINE_PREFIX, WINEDEBUG="-all")
    out = subprocess.run(
        ["wine", "compile_oracle.exe"] + files,
        cwd=ORACLE_DIR,
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    ).stdout.splitlines()
    lines = [l.strip() for l in out if l.startswith(("WORDS", "ERR"))]
    if len(lines) != len(files):
        sys.exit(f"expected {len(files)} results, got {len(lines)}: {out[:400]}")
    return lines


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--emit", help="write the fixture here")
    args = ap.parse_args()
    files = list(cases())
    if not files:
        sys.exit(f"no cases in {CASES}")
    if not os.path.exists(os.path.join(ORACLE_DIR, "compile_oracle.exe")):
        sys.exit(f"no harness at {ORACLE_DIR}/compile_oracle.exe")
    got = run(files)
    rows = []
    ok = 0
    for path, line in zip(files, got):
        name = os.path.basename(path)
        rows.append(f"{name[:-4]}\t{name}\t{line}")
        ok += line.startswith("WORDS")
    text = "\n".join(rows) + "\n"
    if args.emit:
        with open(args.emit, "w") as f:
            f.write(text)
        print(f"wrote {args.emit}: {ok} of {len(rows)} compiled")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
