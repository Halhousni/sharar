#!/usr/bin/env python3
"""Run a snippet of script inside Spike2 and print what it stored in `res`.

    tools/probe.py <<'S'
    res[0] := MaxTime();
    res[1] := ChanKind(1);
    S

The snippet runs after a prelude that declares `res[400]`, the `Enc(s$, at%)` helper (a string as
its length then its character codes) and opens `data\\Demo.smr` as `h%` (use `--no-open`
to skip that, `--file NAME` for another recording). Strings are decoded when a result looks
like one: pass `--str AT` to decode the one stored at AT.
"""
import argparse
import os
import struct
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = r"C:\\Users\\Hilal\\Documents\\Spike8\\data\\"
OUT = "C:\\\\Users\\\\Hilal\\\\Documents\\\\Spike8\\\\out\\\\"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-open", action="store_true")
    ap.add_argument("--file", default="Demo.smr")
    ap.add_argument("--str", type=int, action="append", default=[])
    ap.add_argument("-n", type=int, default=40, help="how many results to show")
    args = ap.parse_args()
    # Programs separated by a line of dashes run independently, so one that Spike2 refuses
    # does not hide the others.
    bodies = [b for b in sys.stdin.read().split("\n----\n")]
    prelude = (
        "var res[400], h%, k%, t, v;\n"
        "proc Enc(s$, at%)\n  var i%;\n  res[at%] := Len(s$);\n"
        "  for i% := 1 to Len(s$) do res[at% + i%] := Asc(Mid$(s$, i%, 1)) next\nend;\n"
    )
    if not args.no_open:
        prelude += f'h% := FileOpen("{DATA}{args.file}", 0, 0);\n'
    with tempfile.TemporaryDirectory() as d:
        for k, body in enumerate(bodies):
            body = body.replace("%P%", f"p{k:02d}")
            src = prelude + body + ("\nView(h%); FileClose();\n" if not args.no_open else "")
            open(os.path.join(d, f"p{k:02d}.s2s"), "w").write(src)
            chk = subprocess.run(["cargo", "run", "-q", "-p", "smr", "--example", "check", "--",
                                  os.path.join(d, f"p{k:02d}.s2s")], capture_output=True, text=True, cwd=ROOT)
            verdict = chk.stdout.strip().splitlines()[-1] if chk.stdout.strip() else chk.stderr[-300:]
            if verdict != "ok":
                print(f"p{k:02d}: does not compile: {verdict}")
                os.remove(os.path.join(d, f"p{k:02d}.s2s"))
        env = dict(os.environ, RUN_CASES=d, LIBVIRT_DEFAULT_URI="qemu:///session")
        out = subprocess.run([sys.executable, os.path.join(ROOT, "tools", "run_oracle.py")],
                             env=env, capture_output=True, text=True)
    for line in out.stdout.splitlines():
        if "\t" not in line:
            continue
        name, rest = line.split("\t")
        if name.startswith("@file "):
            data = bytes.fromhex(rest)
            print(f"   {name[6:]}: {len(data)} bytes {data[:64]!r}")
            continue
        if rest == "ERR":
            print(f"{name}: Spike2 stopped on a script error")
            continue
        v = [struct.unpack("<d", bytes.fromhex(h)[::-1])[0] for h in rest.split()]
        shown = [round(x, 6) if x == x else x for x in v[:args.n]]
        print(name, shown)
        for at in args.str:
            n = int(v[at])
            print(f"   str@{at}:", repr("".join(chr(int(c)) for c in v[at + 1:at + 1 + n])))
    if out.stderr.strip():
        print(out.stderr.strip()[-300:], file=sys.stderr)


if __name__ == "__main__":
    main()
