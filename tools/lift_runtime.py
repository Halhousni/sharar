#!/usr/bin/env python3
"""Translate Spike2's script commands (runtime.s2l) into Rust, with the routines they call.

    tools/lift_runtime.py vmbin s2lift/src cmd1 cmd2 ...

Each command's `Execute` is translated as it stands, so its argument checks, its error codes and
the order of its arithmetic are Spike2's. The routines it calls are translated too wherever they
are Spike2's own code (compiler.dll's arrays, SpikeUtl.dll's maths); what is left is asked of the
host through `Svc::import` and answered by `smr/src/script/lifted.rs`: the script's arguments and
result (CRunInfo), the windows and the data file, the C++ runtime. Needs `capstone`."""
import json, os, re, struct, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lift import Lifter
from command_table import Image, MD

args = [a for a in sys.argv[1:] if not a.startswith("--")]
REPORT = "--report" in sys.argv
vmbin, out_dir, wanted = args[0], args[1], args[2:]
HERE = os.path.dirname(os.path.abspath(__file__))
table = json.loads(subprocess.run([sys.executable, os.path.join(HERE, "command_table.py"),
                                   os.path.join(vmbin, "runtime.s2l")], capture_output=True, text=True).stdout)
execs = {c["name"]: c["exec"] for c in table}
if "--all" in sys.argv:
    wanted = [c["name"] for c in table]


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
MODULES = {"runtime": "runtime.s2l", "compiler": "compiler.dll", "spikeutl": "SpikeUtl.dll", "msvcp110": "msvcp110.dll",
           "sonlayer": "sonlayer.dll", "son64": "son64.dll", "spikecmd": "spikecmd.dll", "sonview": "SONVIEW.exe", "use1432": "USE1432_vm.dll"}
def find(f):
    for d in (vmbin, os.environ.get("ORACLE_DIR", "")):
        if d and os.path.exists(os.path.join(d, f)):
            return os.path.join(d, f)
    raise SystemExit(f"{f} not found in {vmbin} or $ORACLE_DIR")
EXPORTS = {m: exports(find(f)) for m, f in MODULES.items() if m != "runtime"}

# Imports the host answers even where a translated DLL exports them: they reach into the script
# interpreter's own structures, which the host keeps.
HOST = re.compile(r"@CRunInfo@@|^\?DoInit@@|^_CxxThrowException$|^\?\?[23]@YA|^\?_X[a-z_]+@std@@|^__|^_amsg_exit$"
                  r"|^_purecall$|^abort$|GError|ArrayError@|^_malloc_crt$|^_calloc_crt$|^malloc$|^free$|^calloc$|^realloc$")

# The maths library, in Rust.
def libm1(fn):
    return f"m.set_xl(0, crate::msvcrt::{fn}(f64::from_bits(m.xl(0))).to_bits());"
def libm2(fn):
    return f"m.set_xl(0, crate::msvcrt::{fn}(f64::from_bits(m.xl(0)), f64::from_bits(m.xl(1))).to_bits());"
NATIVE = {n: libm1(n) for n in ("sin", "cos", "tan", "sinh", "cosh", "tanh", "atan", "exp", "log", "log10", "asin", "acos")}
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
entries_extra = {}
# Each module's addresses are moved apart, so that a pointer says which module's code it is.
BIAS = {"runtime": 0, "compiler": 1 << 32, "spikeutl": 2 << 32, "msvcp110": 3 << 32, "sonlayer": 4 << 32, "son64": 5 << 32, "spikecmd": 6 << 32, "sonview": 7 << 32, "use1432": 8 << 32}


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
    return (f"rt_{r[0]}_gen", r[1] + BIAS[r[0]]) if r else None


for mod, f in MODULES.items():
    roots = [execs[n] for n in wanted] if mod == "runtime" else []
    lifters[mod] = Lifter(find(f), roots, {}, NATIVE, r"$^", svc=True, default_import=service, cross=cross, bias=BIAS[mod])
    lifters[mod].import_names = import_names
    lifters[mod].pointer_hooks = "--pointer-hooks" in sys.argv

# The functions that build the command objects: each makes a run of commands (`new`, the base
# constructor, the vtable) and hands every one to `DoInit`. Running them at start gives the host the
# objects, and each object says which command it is by its vtable.
def registrars():
    rt = lifters["runtime"]
    im = rt.im
    slot = [a for a, n in rt.iat.items() if n == "?DoInit@@YAFPEAVCBaseCmd@@@Z"]
    out = []
    for st in im.starts:
        end = im.ends.get(st)
        if end is None:
            continue
        for i in MD.disasm(im.b[im.off(st):im.off(st) + end - st], st):
            if i.mnemonic == "call" and i.operands[0].type == 3 and i.operands[0].mem.base == 41 \
                    and i.address + i.size + i.operands[0].mem.disp in slot:
                out.append(st)
                break
    return out

def init_tables(mod):
    """The static initialisers of a module: the compiler's small `dynamic initializer for ...`
    routines, each of which builds one global in the data section (`lea rcx,[global]; call ctor;
    lea rcx,[destructor]; jmp atexit`). They are found by that shape and run in address order, which
    is the order the compiler emitted them in."""
    from capstone import x86_const as X
    im = lifters[mod].im
    data = [(im.base + v, im.base + v + max(vs, rs)) for n, v, vs, rp, rs in im.secs if n == b".data"]
    out = []
    for st in sorted(im.starts):
        end = im.ends.get(st)
        if end is None or end - st > 0x40:
            continue
        ins = list(MD.disasm(im.b[im.off(st):im.off(st) + end - st], st))
        if not (3 <= len(ins) <= 12) or ins[-1].mnemonic not in ("jmp", "ret"):
            continue
        leas = [i.address + i.size + i.operands[1].mem.disp for i in ins
                if i.mnemonic == "lea" and i.operands[1].type == X.X86_OP_MEM and i.operands[1].mem.base == X.X86_REG_RIP]
        calls = [i for i in ins if i.mnemonic in ("call", "jmp")]
        if len(leas) >= 2 and len(calls) >= 2 and any(lo <= a < hi for a in leas for lo, hi in data):
            out.append(st)
    return out

INIT_ORDER = ["son64", "spikeutl", "compiler", "sonlayer", "spikecmd", "runtime"]
INITS = {m: init_tables(m) for m in INIT_ORDER}
for m_, fs in INITS.items():
    for a in fs:
        if a not in lifters[m_].roots:
            lifters[m_].roots.append(a)
        entries_extra.setdefault(m_, set()).add(a)

REGISTRARS = registrars() if "--no-registrars" not in sys.argv else []
for a in REGISTRARS:
    if a not in lifters["runtime"].roots:
        lifters["runtime"].roots.append(a)
entries_extra.setdefault("runtime", set()).update(REGISTRARS)

# `--addr=module:0x180012340`: a routine only ever reached through a pointer.
for a in sys.argv:
    if a.startswith("--addr="):
        mod, ad = a[len("--addr="):].split(":", 1)
        ad = int(ad, 16)
        if ad not in lifters[mod].roots:
            lifters[mod].roots.append(ad)
        entries_extra.setdefault(mod, set()).add(ad)

# `--vslot=sonview:0x14028e1a8:0x398,0x4e0`: some slots of a vtable (and the table itself, so a
# stand-in object can point at it).
for a in sys.argv:
    if a.startswith("--vslot="):
        mod, vt, slots = a[len("--vslot="):].split(":")
        lf = lifters[mod]
        vt = int(vt, 16)
        lf.data_refs.add(vt)
        lf.table_bases.add(vt)
        for sl in slots.split(","):
            ptr, = struct.unpack_from("<Q", lf.im.b, lf.im.off(vt + int(sl, 16)))
            if ptr not in lf.roots:
                lf.roots.append(ptr)
            entries_extra.setdefault(mod, set()).add(ptr)

# `--vall=sonview:0x1402a7de8`: every method of a vtable.
for a in sys.argv:
    if a.startswith("--vall="):
        mod, vt = a[len("--vall="):].split(":")
        lf = lifters[mod]
        vt = int(vt, 16)
        lf.data_refs.add(vt)
        lf.table_bases.add(vt)
        for ptr in lf.vtable_methods(vt):
            if ptr not in lf.roots:
                lf.roots.append(ptr)
            entries_extra.setdefault(mod, set()).add(ptr)

# Extra routines to translate whatever the commands call: `--root module:regex` (the exports of
# that module whose names match), entered from the host.
for a in sys.argv:
    if a.startswith("--root="):
        mod, pat = a[len("--root="):].split(":", 1)
        for n, addr in EXPORTS[mod].items():
            if re.search(pat, n) and addr not in lifters[mod].roots:
                lifters[mod].roots.append(addr)
                entries_extra.setdefault(mod, set()).add(addr)

# `--msg=0x244e,0x2261`: the SONVIEW handlers of window messages, found in the message maps
# (AFX_MSGMAP_ENTRY: message, code, id, last id, signature, function), so SendMessage can reach them.
MSG_HANDLERS = {}
for a in sys.argv:
    if a.startswith("--msg="):
        im_sv = lifters["sonview"].im
        want = {int(x, 16) for x in a[len("--msg="):].split(",")}
        text_lo, text_hi = im_sv.tva, im_sv.tva + im_sv.text[2]
        for i in range(0, len(im_sv.b) - 32, 8):
            msg, code, i1, i2 = struct.unpack_from("<IIII", im_sv.b, i)
            if msg in want and (code, i1, i2) == (0, 0, 0):
                pfn = struct.unpack_from("<Q", im_sv.b, i + 24)[0]
                if text_lo <= pfn < text_hi:
                    MSG_HANDLERS.setdefault(msg, []).append(pfn)
for msg, fs in MSG_HANDLERS.items():
    for f in fs:
        if f not in lifters["sonview"].roots:
            lifters["sonview"].roots.append(f)
        entries_extra.setdefault("sonview", set()).add(f)

# The 1401 library's error text (Error$ asks for it); its other routines drive hardware.
for n in ("U14GetErrorString",):
    a = EXPORTS["use1432"][n]
    lifters["use1432"].roots.append(a)
    entries_extra.setdefault("use1432", set()).add(a)

# Follow the imports: a routine another module supplies becomes a root there, until nothing is new.
entries = {m: set(entries_extra.get(m, ())) for m in MODULES}
entries["runtime"] |= {execs[n] for n in wanted}
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

if REPORT:
    # which commands each service is needed by: walk the call graph from each command
    import collections
    need = collections.defaultdict(set)
    calls = {}
    for mod, lf in lifters.items():
        for fn in lf.known:
            ins, _ = lf.disasm(fn)
            tg, im_ = set(), set()
            for i in ins:
                if i.mnemonic in ("call", "jmp") and i.operands[0].type.__class__ and i.operands[0].type == 2 and False:
                    pass
            calls[(mod, fn)] = (lf, ins)
    print(f"{sum(len(lf.known) for lf in lifters.values())} routines translated")
    from capstone import x86_const as X
    def walk(root):
        seen, work, svcs = set(), [("runtime", root)], set()
        while work:
            mod, fn = work.pop()
            if (mod, fn) in seen or (mod, fn) not in calls:
                continue
            seen.add((mod, fn))
            lf, ins = calls[(mod, fn)]
            for i in ins:
                if i.mnemonic in ("call", "jmp"):
                    op = i.operands[0]
                    if op.type == X.X86_OP_IMM:
                        work.append((mod, op.imm))
                    elif op.type == X.X86_OP_MEM:
                        _, imp = lf.import_of(i)
                        if imp:
                            r = resolve(imp)
                            if r:
                                work.append((r[0], r[1]))
                            elif imp not in NATIVE:
                                svcs.add(imp)
        return svcs
    freq = collections.Counter()
    for n in wanted:
        sv = walk(execs[n])
        for x in sv:
            need[x].add(n)
        print(n, len(sv), "services")
    json.dump({x: sorted(cs) for x, cs in need.items()}, open("/tmp/services.json", "w"), indent=0)
    for x, cs in sorted(need.items(), key=lambda kv: -len(kv[1])):
        print(f"{len(cs):4d} {x}   {' '.join(sorted(cs)[:6])}")
    sys.exit(0)

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
          "static IMAGES: &[&crate::mach::Img] = &[",
          "    &crate::rt_runtime_gen::IMG, &crate::rt_compiler_gen::IMG,",
          "    &crate::rt_spikeutl_gen::IMG, &crate::rt_msvcp110_gen::IMG,",
          "    &crate::rt_sonlayer_gen::IMG, &crate::rt_son64_gen::IMG, &crate::rt_spikecmd_gen::IMG, &crate::rt_sonview_gen::IMG, &crate::rt_use1432_gen::IMG,",
          "];", "",
          "pub fn machine() -> crate::mach::M {",
          "    crate::mach::M::big(IMAGES)",
          "}", "",
          "/// A call through a pointer that is not in the module making it: whichever module has it.",
          "pub(crate) fn call_other(m: &mut crate::mach::M, s: &mut dyn crate::Svc, t: u64) {",
          "    let done = crate::rt_runtime_gen::try_call(m, s, t) || crate::rt_compiler_gen::try_call(m, s, t)",
          "        || crate::rt_spikeutl_gen::try_call(m, s, t) || crate::rt_msvcp110_gen::try_call(m, s, t)",
          "        || crate::rt_sonlayer_gen::try_call(m, s, t) || crate::rt_son64_gen::try_call(m, s, t)",
          "        || crate::rt_spikecmd_gen::try_call(m, s, t) || crate::rt_sonview_gen::try_call(m, s, t) || crate::rt_use1432_gen::try_call(m, s, t);",
          "    if !done && !s.call(m, t) {",
          "        m.fail(&format!(\"a call through a pointer to {t:#x}\"));",
          "    }",
          "}", "",
          "/// Run command `name`'s Execute: rcx is the command object, rdx the run information.",
          "pub fn run(name: &str, m: &mut crate::mach::M, s: &mut dyn crate::Svc) -> bool {",
          "    match name {"]
lines += [f"        {json.dumps(n)} => crate::rt_runtime_gen::enter_{execs[n]:x}(m, s)," for n in wanted]
lines += ["        _ => return false,", "    }", "    true", "}", "",
          "/// Run the functions that build every command object (each reports its objects to `DoInit`).",
          "pub fn register_all(m: &mut crate::mach::M, s: &mut dyn crate::Svc) {"]
lines += [f"    crate::rt_runtime_gen::enter_{a:x}(m, s);" for a in REGISTRARS]
lines += ["}", "",
          "/// The number of static initialisers, in the order they run (dependencies first).",
          f"pub const INIT_COUNT: usize = {sum(len(INITS[m_]) for m_ in INIT_ORDER)};", "",
          "/// Run static initialiser number `i`.",
          "pub fn init_one(i: usize, m: &mut crate::mach::M, s: &mut dyn crate::Svc) {",
          "    match i {"]
k = 0
for m_ in INIT_ORDER:
    for a in INITS[m_]:
        lines.append(f"        {k} => crate::rt_{m_}_gen::enter_{a + BIAS[m_]:x}(m, s),")
        k += 1
lines += ["        _ => {}", "    }"]
lines += ["}", "", "/// Each command's vtable, which says which command an object is.", "pub const VTABLES: &[(&str, u64)] = &["]
lines += [f"    ({json.dumps(c['name'])}, {c['vtable']:#x})," for c in table if c["name"] in set(wanted)]
lines += ["];", "",
          "/// Call an exported routine that was asked for with `--root` by its decorated name.",
          "pub fn call_export(name: &str, m: &mut crate::mach::M, s: &mut dyn crate::Svc) -> bool {",
          "    match name {"]
for mod_, addrs in sorted(entries_extra.items()):
    if mod_ not in EXPORTS:
        continue
    for n, a in sorted(EXPORTS[mod_].items()):
        if a in addrs:
            lines.append(f"        {json.dumps(n)} => crate::rt_{mod_}_gen::enter_{a + BIAS[mod_]:x}(m, s),")
lines += ["        _ => return false,", "    }", "    true", "}", "",
          "/// The message-map functions of window message `msg` (several classes may handle it).",
          "pub fn msg_handlers(msg: u32) -> &'static [u64] {", "    match msg {"]
for msg, fs in sorted(MSG_HANDLERS.items()):
    lines.append(f"        {msg:#x} => &[{', '.join(hex(f + BIAS['sonview']) for f in fs)}],")
lines += ["        _ => &[],", "    }", "}", "",
          "/// Run the translated routine at the biased address `t`.",
          "pub fn call_at(m: &mut crate::mach::M, s: &mut dyn crate::Svc, t: u64) {", "    call_other(m, s, t)", "}"]
from lift import write_if_changed
write_if_changed(os.path.join(out_dir, "commands.rs"), ("\n".join(lines) + "\n").encode())
print({m: len(lf.known) for m, lf in lifters.items()}, "routines;", len(import_names), "services")
