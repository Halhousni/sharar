#!/usr/bin/env python3
"""Run one case file in Spike2 and print the non-zero entries of `res`.

    tools/oracle_dump.py path/to/case.s2s
"""
import os, shutil, struct, subprocess, sys, tempfile
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
d = tempfile.mkdtemp()
name = "zz_dump"
shutil.copy(sys.argv[1], os.path.join(d, name + ".s2s"))
out = os.path.join(d, "out.txt")
env = dict(os.environ, LIBVIRT_DEFAULT_URI="qemu:///session", RUN_CASES=d)
r = subprocess.run([sys.executable, os.path.join(ROOT, "tools/run_oracle.py"), "--only", name, "--emit", out], capture_output=True, text=True, env=env)
txt = open(out).read() if os.path.exists(out) else ""
line = next((l for l in txt.splitlines() if l.startswith(name + "\t")), None)
if not line or "ERR" in line:
    print("Spike2 stopped", r.stdout[-300:], r.stderr[-300:])
    sys.exit(1)
v = [struct.unpack(">d", bytes.fromhex(x))[0] for x in line.split("\t")[1].split()]
print(" ".join(f"[{i}]={x:g}" if x == int(x) else f"[{i}]={x!r}" for i, x in enumerate(v) if x != 0))
