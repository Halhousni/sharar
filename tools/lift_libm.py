#!/usr/bin/env python3
"""Translate msvcr110.dll's maths routines into Rust, instruction by instruction.

    tools/lift_libm.py vmbin/msvcr110.dll smr/src/script/vm

Writes `msvcrt_gen.rs` (one Rust function per routine, each instruction one call on the machine
model in `mach.rs`) and `msvcrt.dat` (the constant tables the routines read). A decompiler's
output cannot be trusted for the order in which a chain of floating-point operations is done, and
the order decides the last bit; the instructions can. The result is checked against the DLL
itself by tools/libm_fuzz.py.

Needs `capstone`."""
import os, re, struct, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capstone
from capstone import x86_const as X
from command_table import Image, MD

MD.detail = True

# The routines that are translated, by the name Rust gives them, and the helpers they call.
ROOTS = {
    "sin": 0x1800867d0, "cos": 0x180082660, "tan": 0x180087adc, "exp": 0x1800836b0,
    "log": 0x1800847c0, "log10": 0x180084a80, "pow": 0x1800853e0, "atan": 0x180081398,
    "atan2": 0x1800815f8, "sinh": 0x180086fe0, "cosh": 0x180082df0, "tanh": 0x180088678,
}
HELPERS = [0x18008319c, 0x1800873d8, 0x180087da8, 0x180088550, 0x18008c3e0]

# Callees that are not translated, and what they amount to here. They are the DLL's error
# reporting (set errno, raise an FP exception, call a user `_matherr`), and each answers with the
# value it was given to return.
STUBS = {
    0x18008b808: "",                                             # _set_statfp
    0x18006d8a0: "",                                             # __security_check_cookie
    0x18008cce8: "m.set_xl(0, m.xl(1));",                        # exp error
    0x18008cee0: "m.set_xl(0, m.xl(1));",                        # log error
    0x18008cea0: "m.set_xl(0, m.xl(1));",                        # log10 error
    0x18008d100: "m.set_xl(0, m.xl(2));",                        # pow error
    0x180088db4: "m.set_xl(0, m.r[8]);",                         # the general error report
    0x18008c8e0: "m.set_xl(0, super::msvcrt::invalid(m.xl(0)).to_bits());",  # sin of inf/nan
    0x18008c8b8: "m.set_xl(0, super::msvcrt::invalid(m.xl(0)).to_bits());",  # cos of inf/nan
}

GPR = {}
for i, names in enumerate([("rax", "eax", "ax", "al"), ("rcx", "ecx", "cx", "cl"), ("rdx", "edx", "dx", "dl"),
                           ("rbx", "ebx", "bx", "bl"), ("rsp", "esp", "sp", "spl"), ("rbp", "ebp", "bp", "bpl"),
                           ("rsi", "esi", "si", "sil"), ("rdi", "edi", "di", "dil")]):
    for n, sz in zip(names, (8, 4, 2, 1)):
        GPR[n] = (i, sz, False)
for i in range(8, 16):
    for suffix, sz in (("", 8), ("d", 4), ("w", 2), ("b", 1)):
        GPR[f"r{i}{suffix}"] = (i, sz, False)
for i, n in enumerate(("ah", "ch", "dh", "bh")):
    GPR[n] = (i, 1, True)

im = Image(sys.argv[1])
out_dir = sys.argv[2]


class Unsupported(Exception):
    pass


def xmm_index(name):
    m = re.fullmatch(r"xmm(\d+)", name)
    return int(m.group(1)) if m else None


def disasm(s):
    e = im.ends[s]
    return list(MD.disasm(im.b[im.off(s):im.off(s) + (e - s)], s)), e


data_refs = set()    # rip-relative targets
table_bases = set()  # addresses loaded with lea, which index tables


def ea(ins, op):
    m = op.mem
    if m.segment != 0:
        raise Unsupported(f"segment override at {ins.address:x}")
    if m.base == X.X86_REG_RIP:
        addr = (ins.address + ins.size + m.disp) & 0xffffffffffffffff
        data_refs.add(addr)
        if ins.mnemonic == "lea":
            table_bases.add(addr)
        return f"{addr:#x}"
    base = -1 if m.base == 0 else GPR[ins.reg_name(m.base)][0]
    index = -1 if m.index == 0 else GPR[ins.reg_name(m.index)][0]
    if ins.mnemonic == "lea" and m.base == 0:
        pass
    return f"m.ea({base}, {index}, {m.scale}, {m.disp})"


def mem_load(ins, op, n):
    a = ea(ins, op)
    if a.startswith("0x"):
        return f"m.ld({a}, {n})"
    return f"m.ld({a}, {n})"


def val(ins, op, size=None):
    """An expression for the integer value of operand `op` at `size` bytes."""
    size = size or op.size
    if op.type == X.X86_OP_IMM:
        return f"{op.imm & ((1 << (8 * size)) - 1):#x}u64"
    if op.type == X.X86_OP_REG:
        i, sz, hi = GPR[ins.reg_name(op.reg)]
        return f"m.gr({i}, {sz}, {str(hi).lower()})"
    if op.type == X.X86_OP_MEM:
        return mem_load(ins, op, size)
    raise Unsupported(ins.op_str)


def put(ins, op, expr, size=None):
    size = size or op.size
    if op.type == X.X86_OP_REG:
        i, sz, hi = GPR[ins.reg_name(op.reg)]
        return f"{{ let v = {expr}; m.sr({i}, {sz}, {str(hi).lower()}, v); }}"
    if op.type == X.X86_OP_MEM:
        return f"{{ let v = {expr}; m.st({ea(ins, op)}, {size}, v); }}"
    raise Unsupported(ins.op_str)


def x64(ins, op):
    """The low 64 bits of an XMM register or a memory operand."""
    if op.type == X.X86_OP_REG:
        return f"m.xl({xmm_index(ins.reg_name(op.reg))})"
    return f"m.ld({ea(ins, op)}, 8)"


def x128(ins, op):
    if op.type == X.X86_OP_REG:
        return f"m.x[{xmm_index(ins.reg_name(op.reg))}]"
    return f"m.ld128({ea(ins, op)})"


def jcc_name(mn):
    return mn[1:]


def lift_ins(ins, fn_range, labels, known):
    """Rust statements for one instruction; returns (text, terminates)."""
    mn, ops = ins.mnemonic, ins.operands
    nxt = ins.address + ins.size
    if mn in ("nop", "int3", "fnop") or mn.startswith("nop"):
        return "", False
    if mn == "mov" or mn == "movabs":
        return put(ins, ops[0], val(ins, ops[1], ops[0].size)), False
    if mn == "movsxd":
        return put(ins, ops[0], f"super::mach::sx({val(ins, ops[1], 4)}, 4)"), False
    if mn == "movzx":
        return put(ins, ops[0], val(ins, ops[1], ops[1].size)), False
    if mn == "movsx":
        return put(ins, ops[0], f"super::mach::sx({val(ins, ops[1], ops[1].size)}, {ops[1].size})"), False
    if mn == "lea":
        return put(ins, ops[0], ea(ins, ops[1]) if ea(ins, ops[1]).startswith("m.") else f"{ea(ins, ops[1])}u64"), False
    if mn == "push":
        return f"m.push({val(ins, ops[0], 8)});", False
    if mn == "pop":
        return put(ins, ops[0], "m.pop()"), False
    if mn in ("add", "sub", "and", "or", "xor"):
        sz = ops[0].size
        return put(ins, ops[0], f"m.{mn}({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz})"), False
    if mn == "cmp":
        sz = ops[0].size
        return f"m.sub({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz});", False
    if mn == "test":
        sz = ops[0].size
        return f"m.and({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz});", False
    if mn in ("inc", "dec", "neg"):
        sz = ops[0].size
        return put(ins, ops[0], f"m.{mn}({val(ins, ops[0])}, {sz})"), False
    if mn == "not":
        sz = ops[0].size
        return put(ins, ops[0], f"!{val(ins, ops[0])} & {(1 << 8 * sz) - 1:#x}"), False
    if mn in ("shl", "shr", "sar"):
        sz = ops[0].size
        cnt = "1" if len(ops) == 1 else val(ins, ops[1], 1)
        return put(ins, ops[0], f"m.{mn}({val(ins, ops[0])}, {cnt}, {sz})"), False
    if mn == "imul":
        if len(ops) == 1:
            if ops[0].size != 4:
                raise Unsupported("imul r/m64 at %x" % ins.address)
            return f"m.imul1_32({val(ins, ops[0])});", False
        if len(ops) == 2:
            sz = ops[0].size
            return put(ins, ops[0], f"m.imul({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz})"), False
        sz = ops[0].size
        return put(ins, ops[0], f"m.imul({val(ins, ops[1], sz)}, {val(ins, ops[2], sz)}, {sz})"), False
    if mn == "cdq":
        return "m.r[2] = if m.r[0] & 0x8000_0000 != 0 { 0xffff_ffff } else { 0 };", False
    if mn == "cdqe":
        return "m.r[0] = super::mach::sx(m.r[0] & 0xffff_ffff, 4);", False
    if mn.startswith("set"):
        return put(ins, ops[0], f"m.cc(\"{mn[3:]}\") as u64"), False
    if mn.startswith("cmov"):
        sz = ops[0].size
        i, _, _ = GPR[ins.reg_name(ops[0].reg)]
        # The source is read even when the move does not happen; a 32-bit move clears the top half either way.
        return (f"{{ let s = {val(ins, ops[1], sz)}; let d = m.gr({i}, {sz}, false); "
                f"m.sr({i}, {sz}, false, if m.cc(\"{mn[4:]}\") {{ s }} else {{ d }}); }}"), False
    # ---- SSE ----
    if mn in ("movsd", "movq", "movd"):
        d, s = ops
        if d.type == X.X86_OP_REG and xmm_index(ins.reg_name(d.reg)) is not None:
            di = xmm_index(ins.reg_name(d.reg))
            if s.type == X.X86_OP_REG and xmm_index(ins.reg_name(s.reg)) is not None:
                si = xmm_index(ins.reg_name(s.reg))
                if mn == "movsd":
                    return f"m.set_xl({di}, m.xl({si}));", False
                return f"m.set_xl_zero({di}, m.xl({si}));", False
            n = 4 if mn == "movd" else 8
            return f"m.set_xl_zero({di}, {val(ins, s, n)});", False
        # store, or move from xmm to a general register
        si = xmm_index(ins.reg_name(s.reg))
        n = 4 if mn == "movd" else 8
        return put(ins, d, f"(m.xl({si}) & {(1 << 8 * n) - 1:#x})", n), False
    if mn in ("movaps", "movapd", "movdqa", "movups", "movdqu"):
        d, s = ops
        if d.type == X.X86_OP_REG:
            return f"m.x[{xmm_index(ins.reg_name(d.reg))}] = {x128(ins, s)};", False
        return f"{{ let v = {x128(ins, s)}; m.st128({ea(ins, d)}, v); }}", False
    if mn in ("addsd", "subsd", "mulsd", "divsd", "sqrtsd"):
        return f"m.{mn}({xmm_index(ins.reg_name(ops[0].reg))}, {x64(ins, ops[1])});", False
    if mn in ("comisd", "ucomisd"):
        return f"m.comisd({x64(ins, ops[0])}, {x64(ins, ops[1])});", False
    if mn in ("xorpd", "xorps", "pxor", "andpd", "andps", "pand", "orpd", "orps", "por", "andnpd", "andnps", "pandn"):
        op = {"xor": "^", "and": "&", "or": "|"}[re.sub(r"(p[ds]|ps|^p)", "", mn) if False else
                                                  ("xor" if "xor" in mn else "or" if mn in ("orpd", "orps", "por") else "and")]
        di = xmm_index(ins.reg_name(ops[0].reg))
        if "andn" in mn or mn == "pandn":
            return f"m.x[{di}] = !m.x[{di}] & {x128(ins, ops[1])};", False
        return f"m.x[{di}] = m.x[{di}] {op} {x128(ins, ops[1])};", False
    if mn == "psrlq":
        return f"{{ let s = m.x[{xmm_index(ins.reg_name(ops[0].reg))}]; let n = {ops[1].imm}; m.x[{xmm_index(ins.reg_name(ops[0].reg))}] = (((s as u64) >> n) as u128) | ((((s >> 64) as u64) >> n) as u128) << 64; }}", False
    if mn == "psubq":
        di = xmm_index(ins.reg_name(ops[0].reg))
        return (f"{{ let a = m.x[{di}]; let b = {x128(ins, ops[1])}; "
                f"m.x[{di}] = ((a as u64).wrapping_sub(b as u64) as u128) | (((a >> 64) as u64).wrapping_sub((b >> 64) as u64) as u128) << 64; }}"), False
    if mn == "psubd":
        di = xmm_index(ins.reg_name(ops[0].reg))
        return (f"{{ let a = m.x[{di}]; let b = {x128(ins, ops[1])}; let mut r = 0u128; "
                f"for k in 0..4 {{ r |= ((((a >> (32 * k)) as u32).wrapping_sub((b >> (32 * k)) as u32)) as u128) << (32 * k); }} m.x[{di}] = r; }}"), False
    if mn == "cvtdq2pd":
        return f"m.cvtdq2pd({xmm_index(ins.reg_name(ops[0].reg))}, {x64(ins, ops[1])});", False
    if mn in ("cvtpd2dq", "cvttpd2dq"):
        return f"m.cvtpd2dq({xmm_index(ins.reg_name(ops[0].reg))}, {x128(ins, ops[1])}, {str(mn == 'cvttpd2dq').lower()});", False
    if mn in ("cvttsd2si", "cvtsd2si"):
        sz = ops[0].size
        return put(ins, ops[0], f"m.cvttsd2si({x64(ins, ops[1])}, {sz})"), False
    if mn == "cvtsi2sd":
        sz = ops[1].size
        return f"m.cvtsi2sd({xmm_index(ins.reg_name(ops[0].reg))}, {val(ins, ops[1], sz)}, {sz});", False
    # ---- control flow ----
    if mn == "ret":
        return "m.pop(); return;", True
    if mn == "jmp":
        if ops[0].type != X.X86_OP_IMM:
            raise Unsupported("indirect jump at %x" % ins.address)
        t = ops[0].imm
        if fn_range[0] <= t < fn_range[1]:
            return f"pc = {t:#x}; continue;", True
        if t in STUBS:
            return f"{STUBS[t]} m.pop(); return;", True
        if t in known:
            return f"f_{t:x}(m); return;", True
        raise Unsupported(f"jump out of the function to {t:#x} at {ins.address:x}")
    if mn.startswith("j"):
        t = ops[0].imm
        if not (fn_range[0] <= t < fn_range[1]):
            raise Unsupported(f"conditional jump out of the function at {ins.address:x}")
        return f"if m.cc(\"{jcc_name(mn)}\") {{ pc = {t:#x}; continue; }}", False
    if mn == "call":
        if ops[0].type != X.X86_OP_IMM:
            raise Unsupported("indirect call at %x" % ins.address)
        t = ops[0].imm
        if t in STUBS:
            return STUBS[t], False
        if t in known:
            return f"m.push(0); f_{t:x}(m);", False
        raise Unsupported(f"call to {t:#x} at {ins.address:x}")
    raise Unsupported(f"{mn} {ins.op_str} at {ins.address:x}")


def lift_function(s, known):
    ins_list, e = disasm(s)
    rng = (s, e)
    by_addr = {i.address: i for i in ins_list}
    leaders = {s}
    for i in ins_list:
        if i.mnemonic == "jmp" and i.operands[0].type == X.X86_OP_IMM and rng[0] <= i.operands[0].imm < rng[1]:
            leaders.add(i.operands[0].imm)
        elif i.mnemonic.startswith("j") and i.mnemonic != "jmp":
            leaders.add(i.operands[0].imm)
        if i.mnemonic in ("jmp", "ret") or (i.mnemonic.startswith("j") and i.mnemonic != "jmp"):
            leaders.add(i.address + i.size)
    lines = [f"pub(super) fn f_{s:x}(m: &mut M) {{", f"    let mut pc: u64 = {s:#x};", "    loop {", "        match pc {"]
    cur = None
    body = []
    terminated = True

    def close(next_addr):
        nonlocal cur, body, terminated
        if cur is None:
            return
        lines.append(f"            {cur:#x} => {{")
        lines.extend("                " + b for b in body if b)
        if not terminated:
            lines.append(f"                pc = {next_addr:#x}; continue;")
        lines.append("            }")
        cur, body = None, []

    for i in ins_list:
        if i.address in leaders:
            close(i.address)
            cur, body, terminated = i.address, [], False
        if cur is None:
            continue  # padding after a block that ended
        text, term = lift_ins(i, rng, {}, known)
        body.append(f"// {i.address:x} {i.mnemonic} {i.op_str}")
        body.append(text)
        if term:
            terminated = True
            close(0)
            cur = None
    close(e)
    lines += ["            _ => unreachable!(\"jump to {pc:#x}\"),", "        }", "    }", "}", ""]
    return "\n".join(lines)


def main():
    known = set(ROOTS.values()) | set(HELPERS)
    parts = ["//! Generated by tools/lift_libm.py from msvcr110.dll. Do not edit.", "",
             "#![allow(clippy::all, unused_variables, unused_assignments, unused_mut, unused_parens)]",
             "use super::mach::*;", ""]
    for s in sorted(known):
        parts.append(lift_function(s, known))
    # the data
    spans = []
    for a in data_refs:
        spans.append((a, a + 16))
    for a in table_bases:
        spans.append((a, a + 8192))
    spans.sort()
    merged = []
    for a, b in spans:
        if merged and a <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], b)
        else:
            merged.append([a, b])
    blob = bytearray()
    ranges = []
    for a, b in merged:
        # clip to what the image holds
        chunk = bytearray()
        for va in range(a, b, 16):
            o = im.off(va)
            if o is None:
                break
            chunk += im.b[o:o + 16]
        ranges.append((a, len(blob), len(chunk)))
        blob += chunk
    parts.append("pub(super) static BLOB: &[u8] = include_bytes!(\"msvcrt.dat\");")
    parts.append("/// (address in the DLL, offset in the blob, length)")
    parts.append("pub(super) const RANGES: &[(u64, u32, u32)] = &[")
    for a, o, n in ranges:
        parts.append(f"    ({a:#x}, {o}, {n}),")
    parts.append("];")
    # entry points
    for name, s in ROOTS.items():
        two = name in ("pow", "atan2")
        parts.append(f"""
pub fn {name}({'x: f64, y: f64' if two else 'x: f64'}) -> f64 {{
    let mut m = M::new();
    m.set_xl(0, x.to_bits());{chr(10) + '    m.set_xl(1, y.to_bits());' if two else ''}
    m.push(0);
    f_{s:x}(&mut m);
    f64::from_bits(m.xl(0))
}}""")
    open(os.path.join(out_dir, "msvcrt_gen.rs"), "w").write("\n".join(parts) + "\n")
    open(os.path.join(out_dir, "msvcrt.dat"), "wb").write(blob)
    print(f"{len(known)} routines, {len(blob)} bytes of tables in {len(ranges)} ranges")


main()
