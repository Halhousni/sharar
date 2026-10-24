#!/usr/bin/env python3
"""Check the port's special functions against SpikeUtl.dll on many arguments.

    tools/util_fuzz.py LnGamma GammaP ... [-n 100000] [--dll vmbin/SpikeUtl.dll] [--deps DIR]

Runs the real DLL under Wine (the DLLs it needs, MFC and the C++ runtime, are looked for in DIR,
default $ORACLE_DIR) through tools/util_oracle.c, and compares with
`cargo run --example libm_check`."""
import argparse, os, random, shutil, struct, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def bits(x): return struct.unpack("<Q", struct.pack("<d", x))[0]

# name: (decorated symbol, signature, argument kinds)
FUNCS = {
    "LnGamma": ("?LnGamma@@YANN@Z", "d>d", "x"),
    "GammaP": ("?GammaP@@YANNN@Z", "dd>d", "ax"),
    "GammaQ": ("?GammaQ@@YANNN@Z", "dd>d", "ax"),
    "Beta": ("?Beta@@YANNN@Z", "dd>d", "ab"),
    "BetaI": ("?BetaI@@YANNNN@Z", "ddd>d", "abx"),
    "BinomialC": ("?BinomialC@@YANHH@Z", "ii>d", "nk"),
}

def val(kind, r):
    if kind == "x":   # any real, mostly of moderate size
        k = r.random()
        if k < 0.1: return float(r.randint(-12, 40))
        if k < 0.3: return r.uniform(-30, 30)
        if k < 0.6: return 2.0 ** r.uniform(-30, 12) * r.choice([1, 1, -1])
        if k < 0.7: return r.uniform(-1e-5, 1e-5)
        return r.uniform(0, 200)
    if kind == "a":   # a positive shape parameter
        k = r.random()
        if k < 0.2: return float(r.randint(1, 60))
        if k < 0.3: return r.choice([0.5, 1.5, 2.5, 0.1, 1e-3])
        return 2.0 ** r.uniform(-8, 10)
    if kind == "b":
        return val("a", r)
    raise ValueError(kind)

def gen(name, n, r):
    kinds = FUNCS[name][2]
    out = []
    specials = [0.0, 1.0, 2.0, 0.5, -0.5, -1.0, -2.0, 10.0, 100.0, 171.0, 172.0, 1e-300, 1e300, float("inf"), float("nan")]
    if name == "BinomialC":
        for n_ in (0, 1, 2, 5, 10, 50, 100, 169, 170, 171, 172, 200, 500, 1000, 5000):
            for k in (0, 1, 2, 3, n_ // 2, n_ - 1, n_, n_ + 1, -1):
                out.append((n_, k))
        while len(out) < n:
            top = r.choice([10, 100, 170, 175, 300, 1000, 100000])
            nn = r.randint(0, top)
            out.append((nn, r.randint(0, nn + 1)))
        return [tuple(w & 0xffffffff for w in t) for t in out]
    if name == "LnGamma":
        for s in specials: out.append((bits(s),))
    if name in ("GammaP", "GammaQ", "Beta"):
        for s in specials[:10]:
            for t in specials[:10]:
                out.append((bits(s), bits(t)))
    while len(out) < n:
        ws = []
        for k in kinds:
            if name == "BetaI" and k == "x":
                ws.append(bits(r.choice([0.0, 1.0, 0.5, r.random(), r.random() ** 3, 1 - r.random() ** 3])))
            else:
                ws.append(bits(val(k, r)))
        out.append(tuple(ws))
    return out

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("functions", nargs="+")
    ap.add_argument("-n", type=int, default=100000)
    ap.add_argument("--dll", default=os.path.join(ROOT, "vmbin", "SpikeUtl.dll"))
    ap.add_argument("--deps", default=os.environ.get("ORACLE_DIR"))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--emit", help="write the arguments and the DLL's answers here")
    args = ap.parse_args()
    tmp = tempfile.mkdtemp()
    exe = os.path.join(tmp, "util_oracle.exe")
    subprocess.run(["x86_64-w64-mingw32-gcc", "-O1", "-o", exe, os.path.join(ROOT, "tools", "util_oracle.c")], check=True)
    if args.deps:
        for f in os.listdir(args.deps):
            if f.lower().endswith(".dll"):
                shutil.copy(os.path.join(args.deps, f), tmp)
    shutil.copy(args.dll, os.path.join(tmp, "SpikeUtl.dll"))
    env = dict(os.environ, WINEPREFIX=os.environ.get("WINE_CFIR_PREFIX", os.path.expanduser("~/.wine-cfir")),
               WINEDEBUG="-all", WINEDLLOVERRIDES="msvcr110,msvcp110,mfc110u=n")
    wp = lambda p: subprocess.run(["winepath", "-w", p], capture_output=True, text=True, env=env).stdout.strip()
    bad, emitted = 0, []
    for fn in args.functions:
        sym, sig, _ = FUNCS[fn]
        r = random.Random(args.seed)
        inp, outp = os.path.join(tmp, fn + ".in"), os.path.join(tmp, fn + ".out")
        with open(inp, "w") as f:
            for t in gen(fn, args.n, r):
                f.write(" ".join("%x" % w for w in t) + "\n")
        res = subprocess.run(["wine", exe, wp(os.path.join(tmp, "SpikeUtl.dll")), sym, sig, wp(inp), wp(outp)],
                             env=env, capture_output=True, text=True)
        if res.returncode:
            print(fn, "oracle failed:", res.stderr[-300:]); bad += 1; continue
        if args.emit:
            for a, o in zip(open(inp).read().split("\n"), open(outp).read().split("\n")):
                if a: emitted.append(f"{fn} {a} {o}")
        c = subprocess.run(["cargo", "run", "-q", "--release", "-p", "smr", "--example", "libm_check", fn, inp, outp],
                           cwd=ROOT, capture_output=True, text=True)
        print(c.stdout.strip(), c.stderr.strip()[-400:] if c.returncode else "")
        if " 0 differ" not in c.stdout: bad += 1
    if args.emit:
        open(args.emit, "w").write("\n".join(emitted) + "\n")
    sys.exit(1 if bad else 0)

main()
