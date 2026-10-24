#!/usr/bin/env python3
"""Check the port's maths functions against msvcr110.dll on many arguments.

    tools/libm_fuzz.py sin cos ... [-n 200000] [--dll vmbin/msvcr110.dll]

Builds libm_oracle.exe, runs it under Wine (WINE_CFIR_PREFIX or ~/.wine-cfir), and compares with
`cargo run --example libm_check`. Arguments are random bit patterns over useful ranges plus the
awkward ones: zeros, infinities, NaNs, denormals, ends of the ranges the code branches on."""
import argparse, os, random, struct, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TWO = {"pow", "atan2"}

def bits(x): return struct.unpack("<Q", struct.pack("<d", x))[0]
def flt(b): return struct.unpack("<d", struct.pack("<Q", b))[0]

SPECIAL = [0.0, -0.0, 1.0, -1.0, 0.5, 2.0, 3.0, 10.0, 100.0, 1e-300, 5e-324, 1e300, 1.7976931348623157e308,
           float("inf"), float("-inf"), 0.7853981633974483, 1.5707963267948966, 3.141592653589793,
           500000.0, 499999.99999999994, 500000.00000000006, 1e6, 1e10, 1e15, 1e22, 2.0 ** 1023, 2.0 ** -1022,
           709.782712893384, 709.7827128933841, -745.1332191019411, -745.13321910194122, 710.0, -746.0,
           0.6931471805599453, 1e-5, 1e-8, 1e-9, 1.0000000000000002, 0.9999999999999999, 4.656612873077393e-10]

def gen(fn, n, rnd):
    out = []
    for x in SPECIAL:
        out.append((x, 1.5) if fn in TWO else (x,))
        out.append((-x, -2.5) if fn in TWO else (-x,))
    out.append((flt(0x7ff8000000000000),) + ((1.0,) if fn in TWO else ()))
    out.append((flt(0xfff8000000000000),) + ((1.0,) if fn in TWO else ()))
    out.append((flt(0x7ff0000000000001),) + ((1.0,) if fn in TWO else ()))
    def rand_arg():
        k = rnd.random()
        if k < 0.35:   # uniform in a moderate range
            return rnd.uniform(-20, 20)
        if k < 0.6:    # log-uniform magnitude
            return rnd.choice([-1, 1]) * 2.0 ** rnd.uniform(-60, 60)
        if k < 0.7:    # large
            return rnd.choice([-1, 1]) * 2.0 ** rnd.uniform(15, 1023)
        if k < 0.8:    # tiny/denormal
            return rnd.choice([-1, 1]) * 2.0 ** rnd.uniform(-1074, -500)
        if k < 0.9:    # near multiples of pi/2
            return rnd.randint(-3000, 3000) * 1.5707963267948966 + rnd.choice([-1, 1]) * 2.0 ** rnd.uniform(-60, -1)
        return flt(rnd.getrandbits(64))  # any bit pattern
    while len(out) < n:
        a = rand_arg()
        if fn in TWO:
            b = rnd.choice([rand_arg(), rnd.uniform(-10, 10), float(rnd.randint(-8, 8)), 0.5, -0.5])
            out.append((a, b))
        else:
            out.append((a,))
    return out

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("functions", nargs="+")
    ap.add_argument("-n", type=int, default=200000)
    ap.add_argument("--dll", default=os.path.join(ROOT, "vmbin", "msvcr110.dll"))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--emit", help="write the arguments and the DLL's answers here, for smr/tests/libm_reference.rs")
    args = ap.parse_args()
    tmp = tempfile.mkdtemp()
    exe = os.path.join(tmp, "libm_oracle.exe")
    subprocess.run(["x86_64-w64-mingw32-gcc", "-O1", "-o", exe, os.path.join(ROOT, "tools", "libm_oracle.c")], check=True)
    dll = os.path.join(tmp, "msvcr110.dll")
    open(dll, "wb").write(open(args.dll, "rb").read())
    env = dict(os.environ, WINEPREFIX=os.environ.get("WINE_CFIR_PREFIX", os.path.expanduser("~/.wine-cfir")), WINEDEBUG="-all", WINEDLLOVERRIDES="msvcr110=n")
    bad = 0
    emitted = []
    for fn in args.functions:
        rnd = random.Random(args.seed)
        inp = os.path.join(tmp, fn + ".in"); outp = os.path.join(tmp, fn + ".out")
        with open(inp, "w") as f:
            for t in gen(fn, args.n, rnd):
                f.write(" ".join("%016x" % bits(v) for v in t) + "\n")
        wp = lambda p: subprocess.run(["winepath", "-w", p], capture_output=True, text=True, env=env).stdout.strip()
        subprocess.run(["wine", exe, wp(dll), fn, wp(inp), wp(outp)], env=env, check=True, capture_output=True)
        if args.emit:
            for a, o in zip(open(inp).read().split("\n"), open(outp).read().split("\n")):
                if a:
                    emitted.append(f"{fn} {a} {o}")
        r = subprocess.run(["cargo", "run", "-q", "--release", "-p", "smr", "--example", "libm_check", fn, inp, outp],
                           cwd=ROOT, capture_output=True, text=True)
        print(r.stdout.strip(), r.stderr.strip()[-300:] if r.returncode else "")
        if " 0 differ" not in r.stdout: bad += 1
    if args.emit:
        open(args.emit, "w").write("\n".join(emitted) + "\n")
    sys.exit(1 if bad else 0)

main()
