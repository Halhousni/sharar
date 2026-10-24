#!/usr/bin/env python3
"""Run script programs inside Spike2 and record what they compute.

The compiler is checked word for word against `compiler.dll`; this checks what the words
*do*. Each program in `tools/run_cases/` declares a global array `res` and fills it. This
appends code that writes `res` to a file with `BWrite` and then runs the next program, so
one launch of Spike2 in the Windows guest runs a whole batch (it has no way to run scripts
one after another from outside, but `ScriptRun` at the end of a script does chain). A
program that raises a script error leaves a dialog open and stops the chain; the program
that never wrote its file is the one that failed, and the batch restarts after it.

    ./tools/run_oracle.py --emit testdata/run_reference.txt

`smr/tests/run_reference.rs` replays the same programs through this crate's interpreter and
compares `res` bit for bit. One line per program in the fixture, tab-separated:

    <name> TAB <res[0]> <res[1]> ...      (each as a 16-digit hex IEEE double)
    <name> TAB ERR                        (Spike2 stopped on a script error)

Needs the guest from tools/vm.sh (libvirt domain in DOM, the sharar_spike scheduled task
that starts SONVIEW.exe in the user's session) and LIBVIRT_DEFAULT_URI=qemu:///session.
"""
import argparse
import os
import struct
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = os.environ.get("RUN_CASES", os.path.join(ROOT, "tools", "run_cases"))
VM = os.path.join(ROOT, "tools", "vm.sh")
GUEST = r"C:\Users\Hilal\Documents\Spike8"
INSTALL = r"C:\Program Files\CED\Spike8"
ENV = dict(os.environ, LIBVIRT_DEFAULT_URI=os.environ.get("LIBVIRT_DEFAULT_URI", "qemu:///session"))


def vm(*args, check=True):
    r = subprocess.run(["bash", VM, *args], capture_output=True, text=True, env=ENV)
    if check and r.returncode:
        sys.exit(f"vm.sh {' '.join(args[:2])} failed: {r.stdout} {r.stderr}")
    return r.stdout


def guest(path):
    return GUEST + "\\" + path


def esc(path):
    return path.replace("\\", "\\\\")


def programs(only=None):
    out = []
    for name in sorted(os.listdir(CASES)):
        if name.endswith(".s2s") and (not only or name[:-4] in only):
            out.append((name[:-4], open(os.path.join(CASES, name)).read()))
    return out


def wrap(k, src, nxt):
    tail = (
        f'\nvar f_k%;\nf_k% := FileOpen("{esc(guest(f"r{k}.bin"))}", 9, 1);\nBWrite(res[]);\nFileClose();\n'
    )
    if nxt is not None:
        tail += f'ScriptRun("{esc(guest(f"r{nxt}.s2s"))}");\n'
    return src.rstrip() + "\n" + tail


def have(n):
    out = vm("ps", f"Get-ChildItem '{GUEST}\\r*.bin' -EA SilentlyContinue | ForEach-Object {{ $_.Name }}", check=False)
    return {l.strip() for l in out.splitlines() if l.strip().endswith(".bin")}


def run_batch(progs, start, tmp):
    """Run progs[start:] chained; returns the index of the first program that did not finish,
    or len(progs)."""
    for k in range(start, len(progs)):
        nxt = k + 1 if k + 1 < len(progs) else None
        p = os.path.join(tmp, f"r{k}.s2s")
        open(p, "w").write(wrap(k, progs[k][1], nxt))
        vm("put", p, guest(f"r{k}.s2s"))
    startup = os.path.join(tmp, "startup.s2s")
    open(startup, "w").write(f'ScriptRun("{esc(guest(f"r{start}.s2s"))}");\r\n')
    vm("put", startup, INSTALL + r"\startup.s2s")
    vm("ps", "Get-Process sonview -EA SilentlyContinue | ForEach-Object { $_.Kill() }; "
             f"Remove-Item '{GUEST}\\r*.bin' -EA SilentlyContinue; Start-Sleep 2; Start-ScheduledTask sharar_spike")
    seen, last_change = set(), time.time()
    while True:
        time.sleep(3)
        now = have(0)
        if now != seen:
            seen, last_change = now, time.time()
        if f"r{len(progs) - 1}.bin" in seen:
            break
        if time.time() - last_change > 25:
            break
    for k in range(start, len(progs)):
        if f"r{k}.bin" not in seen:
            return k
    return len(progs)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--emit")
    ap.add_argument("--only", nargs="*")
    args = ap.parse_args()
    progs = programs(args.only)
    results = {}
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        start = 0
        while start < len(progs):
            sub = progs[start:]
            # indices inside run_batch are relative to the full list, so keep one list
            stop = run_batch(progs, start, tmp)
            for k in range(start, stop):
                local = os.path.join(tmp, f"o{k}.bin")
                vm("get", guest(f"r{k}.bin"), local)
                b = open(local, "rb").read()
                results[progs[k][0]] = [struct.unpack("<Q", b[i:i + 8])[0] for i in range(0, len(b) - len(b) % 8, 8)]
            if stop < len(progs):
                print(f"{progs[stop][0]}: Spike2 stopped", file=sys.stderr)
                results[progs[stop][0]] = None
                vm("key", "KEY_ENTER", check=False)
                stop += 1
            start = stop
    rows = []
    for name, _ in progs:
        r = results.get(name)
        rows.append(f"{name}\t" + ("ERR" if r is None else " ".join(f"{w:016x}" for w in r)))
    text = "\n".join(rows) + "\n"
    if args.emit:
        open(args.emit, "w").write(text)
        print(f"wrote {args.emit}: {sum(1 for r in results.values() if r is not None)} of {len(progs)} ran", file=sys.stderr)
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
