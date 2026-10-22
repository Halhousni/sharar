#!/usr/bin/env python3
"""Compare the Rust Parks-McClellan port with CED's own designer.

Runs two implementations on the same band tables and reports the largest
difference between the coefficient sets:

  * the port, via `cargo run -p smr --example fir_cli`
  * CED's `Filters.dll`, through the small Windows harness in
    `~/spike8-re/oracle`, under Wine

The vendor DLL is not redistributable, so the harness and the DLLs live outside
this repository; point ORACLE_DIR at them. Without an oracle only the port runs,
which proves nothing about parity but keeps the script usable on a clean clone.

    ./tools/fir_oracle.py              # full sweep, needs the oracle
    ./tools/fir_oracle.py --list       # print the designs, run nothing
"""
import argparse
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ORACLE_DIR = os.environ.get("ORACLE_DIR", os.path.expanduser("~/spike8-re/oracle/dlls"))
WINE_PREFIX = os.environ.get("WINE_CFIR_PREFIX", os.path.expanduser("~/.wine-cfir"))


def low_pass(fc, width):
    return [(0.0, fc - width / 2, 1.0, 1.0), (fc + width / 2, 0.5, 0.0, 10.0)]


def high_pass(fc, width):
    return [(0.0, fc - width / 2, 0.0, 10.0), (fc + width / 2, 0.5, 1.0, 1.0)]


def band_pass(f1, f2, gap=0.02):
    return [
        (0.0, f1 - gap, 0.0, 10.0),
        (f1, f2, 1.0, 1.0),
        (f2 + gap, 0.5, 0.0, 10.0),
    ]


def band_stop(f1, f2, gap=0.02):
    return [
        (0.0, f1 - gap, 1.0, 1.0),
        (f1, f2, 0.0, 10.0),
        (f2 + gap, 0.5, 1.0, 1.0),
    ]


def differ(fc, width):
    import math
    return [(0.0, fc, 2 * math.pi, 1.0), (fc + width, 0.5, 0.0, 10.0)]


# (nfilt, jtype, lgrid, retry, bands)
DESIGNS = [
    (35, 0, 2, 1, low_pass(0.10, 0.05)),
    (19, 0, 2, 1, low_pass(0.10, 0.05)),
    (63, 0, 2, 1, low_pass(0.20, 0.02)),
    (101, 0, 2, 1, low_pass(0.05, 0.01)),
    (129, 0, 2, 1, low_pass(0.30, 0.04)),
    (36, 0, 2, 1, low_pass(0.15, 0.05)),
    (64, 0, 2, 1, low_pass(0.15, 0.05)),
    (35, 0, 2, 1, high_pass(0.10, 0.05)),
    (63, 0, 2, 1, high_pass(0.25, 0.05)),
    (64, 0, 2, 1, high_pass(0.25, 0.05)),
    # A raw band table with a stop band in the middle: the shape the filter
    # dialog's band editor produces, passed to CFIR untouched.
    (103, 0, 3, 1, [(0.0, 0.20, 1.0, 1.0), (0.22, 0.30, 0.0, 10.0), (0.32, 0.5, 1.0, 1.0)]),
    (127, 0, 3, 1, band_pass(0.10, 0.20)),
    (63, 0, 3, 1, band_pass(0.15, 0.25)),
    (127, 0, 3, 1, band_stop(0.10, 0.20)),
    (255, 0, 2, 1, low_pass(0.12, 0.02)),
    (15, 0, 2, 1, low_pass(0.20, 0.05)),
    (7, 0, 2, 1, low_pass(0.20, 0.05)),
    (3, 0, 2, 1, low_pass(0.20, 0.05)),
    (35, 4, 2, 1, differ(0.20, 0.05)),
    (63, 4, 2, 1, differ(0.30, 0.10)),
    # Band tables that do not reach Nyquist take the warped path in invfft and
    # the corner fix-up that goes with it.
    (35, 0, 2, 1, [(0.05, 0.15, 1.0, 1.0), (0.25, 0.40, 0.0, 10.0)]),
    (63, 0, 2, 1, [(0.02, 0.10, 1.0, 1.0), (0.20, 0.45, 0.0, 10.0)]),
    (64, 0, 2, 1, [(0.10, 0.20, 1.0, 1.0), (0.30, 0.40, 0.0, 10.0)]),
    (127, 0, 3, 1, [(0.05, 0.10, 0.0, 10.0), (0.12, 0.20, 1.0, 1.0), (0.25, 0.40, 0.0, 10.0)]),
    (32, 0, 2, 1, [(0.05, 0.30, 1.0, 1.0), (0.35, 0.45, 0.0, 10.0)]),
    # FIRMake's own defaults: an explicit band table, nGrid left at its reset
    # value of 16, and the two antisymmetric CFIR types that only FIRMake can
    # reach (script type% 3 -> 5, 4 -> 6).
    (41, 0, 16, 1, low_pass(0.15, 0.10)),
    (41, 5, 16, 1, [(0.0, 0.20, 1.0, 1.0), (0.25, 0.5, 0.0, 10.0)]),
    (41, 6, 16, 1, [(0.0, 0.20, 1.0, 1.0), (0.25, 0.5, 0.0, 10.0)]),
    (63, 5, 16, 1, [(0.05, 0.25, 1.0, 1.0), (0.30, 0.45, 0.0, 10.0)]),
    (64, 6, 16, 1, [(0.0, 0.30, 1.0, 1.0), (0.35, 0.5, 0.0, 10.0)]),
]


def render(designs):
    out = []
    for nfilt, jtype, lgrid, retry, bands in designs:
        fields = [nfilt, jtype, len(bands), lgrid, retry]
        for b in bands:
            fields.extend(b)
        out.append(" ".join(str(f) for f in fields))
    return "\n".join(out) + "\n"


def run_port(text):
    p = subprocess.run(
        ["cargo", "run", "-q", "-p", "smr", "--example", "fir_cli"],
        input=text,
        capture_output=True,
        text=True,
        cwd=ROOT,
    )
    if p.returncode != 0:
        sys.exit("port failed:\n" + p.stderr)
    return p.stdout.strip().splitlines()


def run_oracle(text):
    exe = os.path.join(ORACLE_DIR, "cfir_oracle.exe")
    if not os.path.exists(exe):
        sys.exit(f"no oracle at {exe}")
    env = dict(os.environ, WINEPREFIX=WINE_PREFIX, WINEDEBUG="-all")
    p = subprocess.run(
        ["wine", "cfir_oracle.exe"],
        input=text,
        capture_output=True,
        text=True,
        cwd=ORACLE_DIR,
        env=env,
    )
    if p.returncode != 0:
        sys.exit("oracle failed:\n" + p.stderr[-2000:])
    # The harness also prints an EXT line per design; the comparison uses
    # only the coefficient lines.
    return [
        l for l in p.stdout.strip().splitlines()
        if l and not l.startswith("MESA") and not l.startswith("EXT")
    ]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--tol", type=float, default=1e-12, help="tolerance for full-coverage tables")
    ap.add_argument(
        "--tol-warped",
        type=float,
        default=1e-6,
        help="tolerance for band tables that take invfft's warped path",
    )
    ap.add_argument(
        "--emit",
        metavar="PATH",
        help="write the design inputs and the vendor's coefficients as a test fixture",
    )
    args = ap.parse_args()

    args.specs = DESIGNS
    text = render(DESIGNS)
    if args.list:
        print(text, end="")
        return 0

    if args.emit:
        rows = run_oracle(text)
        with open(args.emit, "w") as f:
            f.write("# design: nfilt jtype lgrid retry bands...\n")
            f.write("# then one line per design: dev nfilt c0..cN from CED's Filters.dll\n")
            for spec, row in zip(text.strip().splitlines(), rows):
                f.write(spec + "\n")
                f.write(row + "\n")
        print(f"wrote {args.emit}")
        return 0

    port = run_port(text)
    oracle = run_oracle(text)

    if len(port) != len(oracle):
        sys.exit(f"line count differs: port {len(port)}, oracle {len(oracle)}")

    worst = 0.0
    worst_i = -1
    failures = 0
    for i, (a, b) in enumerate(zip(port, oracle)):
        pa = a.split()
        ob = b.split()
        if pa[0].startswith("-") or ob[0].startswith("-"):
            status = "both-failed" if pa[0].startswith("-") and ob[0].startswith("-") else "MISMATCH-FAIL"
            if status != "both-failed":
                failures += 1
            print(f"{i:2d} {status}")
            continue
        if len(pa) != len(ob):
            print(f"{i:2d} length {len(pa)} vs {len(ob)}")
            failures += 1
            continue
        dev_a, dev_b = float(pa[0]), float(ob[0])
        ca = [float(x) for x in pa[3:]]
        cb = [float(x) for x in ob[3:]]
        if len(ca) != len(cb):
            print(f"{i:2d} coef count {len(ca)} vs {len(cb)}")
            failures += 1
            continue
        scale = max(1e-30, max(abs(v) for v in cb))
        diff = max((abs(x - y) for x, y in zip(ca, cb)), default=0.0) / scale
        if diff > worst:
            worst, worst_i = diff, i
        warped = args.specs[i][4][0][0] != 0.0 or args.specs[i][4][-1][1] != 0.5
        tol = args.tol_warped if warped else args.tol
        flag = "ok" if diff <= tol else "DIFF"
        tag = "warp" if warped else "    "
        print(f"{i:2d} {flag:4s}{tag} n={len(ca):4d} dev {dev_a:.6e}/{dev_b:.6e} rel {diff:.3e}")
        if diff > tol:
            failures += 1

    print(f"\nworst relative coefficient difference {worst:.3e} (design {worst_i}), {failures} of {len(port)} outside {args.tol:g}")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
