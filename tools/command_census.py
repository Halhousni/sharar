#!/usr/bin/env python3
"""Run every built-in command once, with made-up arguments of the right types, in the port, and say
which ones stop because the port cannot do them yet (a command not ported, or a service of the
translated code that is not provided). Arguments the command refuses are not counted against it.

    tools/command_census.py [--bin target/debug/examples/runsession] [name ...]
"""
import os, re, subprocess, sys, collections
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
table = open(os.path.join(ROOT, "smr/src/script/commands_table.rs")).read()
rows = re.findall(r'\("([^"]+)", (\d+), (\d+), (\d+), &\[(.*?)\]\),\n', table)
BIN = os.path.join(ROOT, "target/debug/examples/runsession")
args = [a for a in sys.argv[1:] if not a.startswith("--")]
only = set(a for a in args if not a.endswith("runsession"))
if "--bin" in sys.argv:
    BIN = sys.argv[sys.argv.index("--bin") + 1]
    only.discard(BIN)

def arg_for(code, k):
    base, dims = code & 0xff & ~0x80, (code >> 8) & 7
    var = bool(code & 0x80)
    t = base & 0x1f
    scal = {1: "1", 2: "1.0", 3: '"a"', 4: "1", 6: "1", 8: "1", 9: "iv%", 10: "rv", 11: "sv$", 12: "iv%", 13: "rv"}
    if dims:
        n = {1: 1, 2: 1, 3: 1, 4: 2}.get(dims, 1)
        nm = {1: "ia%", 2: "ra", 3: "sa$", 4: "ia%"}
        kind = "sa$" if t in (3, 11) else ("ra" if t in (2, 10) else "ia%")
        return kind + "[]" * (2 if dims >= 2 and t in (4, 0x10) else 1)
    return scal.get(t, "1")

decl = 'var iv%, rv, sv$, ia%[10], ra[10], sa$[10], h%;\nh% := FileOpen("' + os.path.join(ROOT, "testdata/Example_Data_Files/Demo.smr") + '", 0, 0);\n'
out = collections.OrderedDict()
for name, mn, mx, ret, variants in rows:
    if only and name not in only:
        continue
    vs = re.findall(r'&\[([^\]]*)\]', variants)
    codes = [[int(x) for x in v.split(",") if x.strip()] for v in vs]
    code = codes[0]
    n = max(int(mn), min(len(code), int(mn)))
    call = f"{name}({', '.join(arg_for(c, i) for i, c in enumerate(code[:n]))})"
    stmt = f"iv% := {call};" if ret in ("1", "2") else (f"sv$ := {call};" if ret == "3" else f"{call};")
    if ret == "2": stmt = f"rv := {call};"
    src = decl + stmt + "\n"
    p = "/tmp/census.s2s"
    open(p, "w").write(src)
    try:
        r = subprocess.run([BIN, p], capture_output=True, text=True, timeout=60)
        txt = r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        txt = "stopped: timeout"
    why = [l for l in txt.splitlines() if "stopped" in l or "translated code" in l or "panicked" in l or "CompileError" in l]
    status = "ok"
    detail = ""
    if any("CompileError" in l for l in txt.splitlines()):
        status = "uncompilable"
    else:
        m = re.search(r"stopped: Error\((-?\d+)\)", txt)
        if m:
            code_ = int(m.group(1))
            if code_ in (-7001, -7002, -7003):
                status = "unported"
                detail = next((l.strip()[:150] for l in txt.splitlines() if "translated code" in l or "not provided" in l or "not ported" in l), "")
            else:
                status = "refused"
        elif "stopped: Done" not in txt:
            status = "crash"
            detail = (why[:1] or [""])[0][:150]
    out[name] = (status, detail, call)
cnt = collections.Counter(s for s, _, _ in out.values())
print(dict(cnt))
for name, (s, d, c) in out.items():
    if s in ("unported", "crash"):
        print(f"{s:9} {c}  {d}")
