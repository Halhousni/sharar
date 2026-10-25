#!/usr/bin/env python3
"""Find which statement of a case makes Spike2 stop: each line after the line `k% := 0;` is run
as a program of its own by tools/probe.py (which has opened Demo.smr as h%).

    tools/stmt_oracle.py tools/run_cases/NAME.s2s 'var extra, names;'
"""
import os, subprocess, sys
src = open(sys.argv[1]).read().splitlines()
extra = sys.argv[2] if len(sys.argv) > 2 else ""
at = next(i for i, l in enumerate(src) if l.strip() == "k% := 0;")
progs = [extra + "\nk% := 0;\n" + l for l in src[at + 1:] if l.strip()]
text = "\n----\n".join(progs)
r = subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "probe.py")], input=text, capture_output=True, text=True,
                   env=dict(os.environ, LIBVIRT_DEFAULT_URI="qemu:///session"))
out = r.stdout.splitlines()
for i, l in enumerate(out):
    if "stopped" in l or "compile" in l:
        print(l, "<-", progs[int(l[1:3])].splitlines()[-1] if l.startswith("p") and l[1:3].isdigit() else "")
print(len(progs), "statements;", sum(1 for l in out if "stopped" in l or "compile" in l), "stopped")
