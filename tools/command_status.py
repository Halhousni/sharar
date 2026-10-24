#!/usr/bin/env python3
"""Which of the 503 commands the port runs: names that appear as match arms in the interpreter.

    tools/command_status.py [--missing]
"""
import json, os, re, subprocess, sys
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
table = open(os.path.join(ROOT, "smr/src/script/commands_table.rs")).read()
names = re.findall(r'^\s+\("([^"]+)", \d+, \d+, \d+,', table, re.M)
src = ""
for f in ["smr/src/script/vm/builtins.rs", "smr/src/script/session.rs", "smr/src/script/vm.rs"]:
    src += open(os.path.join(ROOT, f)).read()
arms = set(re.findall(r'"([A-Za-z0-9$]+)"', src))
done = [n for n in names if n in arms]
print(f"{len(done)} of {len(names)} commands have behaviour in the port")
if "--missing" in sys.argv:
    print(" ".join(n for n in names if n not in arms))
