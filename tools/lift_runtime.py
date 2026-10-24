#!/usr/bin/env python3
"""Translate Spike2's script commands (runtime.s2l) into Rust, with the routines they call.

    tools/lift_runtime.py vmbin s2lift/src cmd1 cmd2 ...

Each command's `Execute` is translated as it stands, so its argument checks, its error codes and
the order of its arithmetic are Spike2's. The routines it calls are translated too wherever they
are Spike2's own code (compiler.dll's arrays, SpikeUtl.dll's maths); what is left is asked of the
host through `Svc::import` and answered by `smr/src/script/lifted.rs`: the script's arguments and
result (CRunInfo), the windows and the data file, the C++ runtime. Needs `capstone`."""
import json, os, re, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lift import Lifter
from command_table import Image

vmbin, out_dir, wanted = sys.argv[1], sys.argv[2], sys.argv[3:]
HERE = os.path.dirname(os.path.abspath(__file__))
table = json.loads(subprocess.run([sys.executable, os.path.join(HERE, "command_table.py"),
                                   os.path.join(vmbin, "runtime.s2l")], capture_output=True, text=True).stdout)
execs = {c["name"]: c["exec"] for c in table}


def exports(path):
    """The exported routines (not the exported data) of a DLL."""
    im = Image(path)
    lo, hi = im.tva, im.tva + im.text[2]
    out = {}
    for line in subprocess.run([sys.executable, os.path.join(HERE, "pe_exports.py"), path],
                               capture_output=True, text=True, check=True).stdout.splitlines():
        a, n = line.split(" ", 1)
        if lo <= int(a, 16) < hi:
            out[n] = int(a, 16)
    return out


# The DLLs whose code is translated, and what they export.
MODULES = {"runtime": "runtime.s2l", "compiler": "compiler.dll", "spikeutl": "SpikeUtl.dll"}
EXPORTS = {m: exports(os.path.join(vmbin, f)) for m, f in MODULES.items() if m != "runtime"}

# Imports the host answers even where a translated DLL exports them: they reach into the script
# interpreter's own structures, which the host keeps.
HOST = re.compile(r"@CRunInfo@@|@CBaseCmd@@|^_CxxThrowException$|^\?\?[23]@YA|^\?_X[a-z_]+@std@@|^__|^_amsg_exit$"
                  r"|^_purecall$|^abort$|GError|ArrayError@|^_malloc_crt$|^_calloc_crt$|^malloc$|^free$|^calloc$|^realloc$")

# The maths library, in Rust.
def libm1(fn):
    return f"m.set_xl(0, crate::msvcrt::{fn}(f64::from_bits(m.xl(0))).to_bits());"
def libm2(fn):
    return f"m.set_xl(0, crate::msvcrt::{fn}(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"
NATIVE = {n: libm1(n) for n in ("sin", "cos", "tan", "sinh", "cosh", "exp", "log", "log10", "asin", "acos")}
NATIVE.update({"pow": libm2("pow"), "atan2": libm2("atan2"), "_hypot": libm2("hypot")})
NATIVE["fmod"] = "m.set_xl(0, (f64::from_bits(m.xl(0)) % f64::from_bits(m.xl(1))).to_bits());"
NATIVE["memset"] = "m.memset();"
NATIVE["memcpy"] = NATIVE["memmove"] = "m.memcpy();"
NATIVE["?LnGamma@@YANN@Z"] = "m.set_xl(0, crate::special::ln_gamma(f64::from_bits(m.xl(0))).to_bits());"
NATIVE["?GammaP@@YANNN@Z"] = "m.set_xl(0, crate::special::gamma_p(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"
NATIVE["?GammaQ@@YANNN@Z"] = "m.set_xl(0, crate::special::gamma_q(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"
NATIVE["?Beta@@YANNN@Z"] = "m.set_xl(0, crate::special::beta(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"
NATIVE["?BetaI@@YANNNN@Z"] = ("m.set_xl(0, crate::special::beta_i(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1)), "
                              "f64::from_bits(m.xl(2))).to_bits());")
NATIVE["?BinomialC@@YANHH@Z"] = "m.set_xl(0, crate::special::binomial_c(m.r[1] as u32 as i32, m.r[2] as u32 as i32).to_bits());"

import_names = []
lifters = {}


def service(lifter, name):
    if name not in import_names:
        import_names.append(name)
    return f"s.import(m, {import_names.index(name)});"


def resolve(name):
    """(module, address) of the translated routine that answers import `name`, or None."""
    if name in NATIVE or HOST.search(name):
        return None
    for mod, ex in EXPORTS.items():
        if name in ex:
            return mod, ex[name]
    return None


def cross(name):
    r = resolve(name)
    return (f"rt_{r[0]}_gen", r[1]) if r else None


for mod, f in MODULES.items():
    roots = [execs[n] for n in wanted] if mod == "runtime" else []
    lifters[mod] = Lifter(os.path.join(vmbin, f), roots, {}, NATIVE, r"$^", svc=True, default_import=service, cross=cross)
    lifters[mod].import_names = import_names

# Follow the imports: a routine another module supplies becomes a root there, until nothing is new.
entries = {m: set() for m in MODULES}
entries["runtime"] = {execs[n] for n in wanted}
changed = True
while changed:
    changed = False
    for mod, lf in lifters.items():
        lf.discover()
        for name in sorted(lf.imports_used):
            r = resolve(name)
            if r and r[1] not in lifters[r[0]].roots:
                lifters[r[0]].roots.append(r[1])
                entries[r[0]].add(r[1])
                changed = True

for mod, lf in lifters.items():
    if lf.roots:
        lf.write(out_dir, MODULES[mod], "lift_runtime.py", entries=sorted(entries[mod]), module=f"rt_{mod}")

lines = ["//! Generated by tools/lift_runtime.py. Do not edit.", "",
         "/// The routines the translated commands import and the host must answer, by `Svc::import` id.",
         "pub const IMPORT_NAMES: &[&str] = &["]
lines += [f"    {json.dumps(n)}," for n in import_names]
lines += ["];", "", "/// The translated commands.", "pub const COMMANDS: &[&str] = &["]
lines += [f"    {json.dumps(n)}," for n in wanted]
lines += ["];", "",
          "/// A machine to run commands on.",
          "pub fn machine() -> crate::mach::M {",
          "    crate::mach::M::new(&crate::rt_runtime_gen::IMG)",
          "}", "",
          "/// Run command `name`'s Execute: rcx is the command object, rdx the run information.",
          "pub fn run(name: &str, m: &mut crate::mach::M, s: &mut dyn crate::Svc) -> bool {",
          "    match name {"]
lines += [f"        {json.dumps(n)} => crate::rt_runtime_gen::enter_{execs[n]:x}(m, s)," for n in wanted]
lines += ["        _ => return false,", "    }", "    true", "}"]
open(os.path.join(out_dir, "commands.rs"), "w").write("\n".join(lines) + "\n")
print({m: len(lf.known) for m, lf in lifters.items()}, "routines;", len(import_names), "services")
