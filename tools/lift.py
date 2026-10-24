#!/usr/bin/env python3
"""Translate routines of a DLL into Rust, instruction by instruction.

A decompiler's output cannot be trusted for the order in which a chain of floating-point
operations is done, and the order decides the last bit; the instructions can. This reads the
machine code of the routines named in a configuration (tools/lift_libm.py, tools/lift_spikeutl.py)
and everything they call, and writes a Rust module in which each instruction is one call on the
machine model in s2lift/src/mach.rs, plus the constant tables they read. Results are checked
against the DLL itself (tools/libm_fuzz.py, tools/util_oracle.py).

Needs `capstone`."""
import os, re, struct, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import capstone
from capstone import x86_const as X
from command_table import Image, MD

MD.detail = True

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


class Unsupported(Exception):
    pass


def xmm_index(name):
    m = re.fullmatch(r"xmm(\d+)", name)
    return int(m.group(1)) if m else None


class Lifter:
    """
    `roots`   {address: True} the routines callers enter by; everything they call is translated too.
    `stubs`   {address: Rust statements} calls that are replaced by what they amount to.
    `imports` {import name: Rust statements} calls through the import table.
    `fail_imports` regex: a routine that calls an import matching it is an error path, and calls
              to it become `m.fail(..)` instead of being translated.
    """

    def __init__(self, dll, roots, stubs, imports, fail_imports):
        self.im = Image(dll)
        self.roots = list(roots)
        self.stubs = stubs
        self.imports = imports
        self.fail_re = re.compile(fail_imports)
        self.iat = self.im.imports()
        self.data_refs = set()
        self.table_bases = set()
        self.known = set()
        self.failing = set()
        self.warnings = []
        self.build_ranges()

    def build_ranges(self):
        """Compilers split a function into pieces, each with its own unwind entry chained to the
        first; a function is the entry plus its chained pieces."""
        im = self.im
        pd = next(sec for sec in im.secs if sec[0] == b".pdata")
        ents = []
        for i in range(pd[2] // 12):
            ents.append(struct.unpack_from("<III", im.b, pd[3] + 12 * i))
        parent = {}
        for b, e, u in ents:
            o = im.rva_off(u)
            flags, count = im.b[o] >> 3, im.b[o + 2]
            if flags & 4:
                chain = o + 4 + ((count + 1) & ~1) * 2
                pb, _, _ = struct.unpack_from("<III", im.b, chain)
                parent[b] = pb
        self.ranges = {}
        for b, e, u in ents:
            r = b
            while r in parent:
                r = parent[r]
            self.ranges.setdefault(im.base + r, []).append((im.base + b, im.base + e))
        for r in self.ranges.values():
            r.sort()

    # ---- discovery ------------------------------------------------------------------------

    def disasm(self, s):
        """All the instructions of the function at `s`, its pieces in address order, and its ranges."""
        if s in self.ranges:
            rngs = self.ranges[s]
            out = []
            for b, e in rngs:
                out += list(MD.disasm(self.im.b[self.im.off(b):self.im.off(b) + (e - b)], b))
            return out, rngs
        return self.descend(s)

    def descend(self, s):
        """A function with no unwind entry (a leaf): follow its branches from the start."""
        seen, work = {}, [s]
        while work:
            a = work.pop()
            while a not in seen:
                o = self.im.off(a)
                if o is None:
                    break
                ins = next(MD.disasm(self.im.b[o:o + 16], a), None)
                if ins is None:
                    break
                seen[a] = ins
                a += ins.size
                if ins.mnemonic in ("ret", "repz ret", "int3", "ud2"):
                    break
                if ins.mnemonic == "jmp":
                    if ins.operands[0].type == X.X86_OP_IMM:
                        work.append(ins.operands[0].imm)
                    elif ins.operands[0].type == X.X86_OP_REG:
                        # a jump table: its targets follow from the instructions before it
                        pre = sorted(seen.values(), key=lambda i: i.address)
                        k = pre.index(ins)
                        try:
                            work.extend(self.jump_table(pre, k, [(s, s + 0x10000)]))
                        except Unsupported:
                            pass
                    break
                if ins.mnemonic.startswith("j") and ins.operands[0].type == X.X86_OP_IMM:
                    work.append(ins.operands[0].imm)
        out = sorted(seen.values(), key=lambda i: i.address)
        rngs = []
        for i in out:
            if rngs and rngs[-1][1] == i.address:
                rngs[-1] = (rngs[-1][0], i.address + i.size)
            else:
                rngs.append((i.address, i.address + i.size))
        return out, rngs

    def import_of(self, ins):
        op = ins.operands[0]
        if op.type == X.X86_OP_MEM and op.mem.base == X.X86_REG_RIP:
            a = (ins.address + ins.size + op.mem.disp) & 0xffffffffffffffff
            return a, self.iat.get(a)
        return None, None

    def discover(self):
        work = list(self.roots)
        while work:
            s = work.pop()
            if s in self.known or s in self.stubs:
                continue
            ins, rngs = self.disasm(s)
            self.ranges.setdefault(s, rngs)
            if any(i.mnemonic == "call" and (self.import_of(i)[1] or "") and self.fail_re.search(self.import_of(i)[1])
                   for i in ins):
                self.failing.add(s)
                continue
            self.known.add(s)
            for i in ins:
                if i.mnemonic in ("call", "jmp") and i.operands[0].type == X.X86_OP_IMM:
                    t = i.operands[0].imm
                    if not any(b <= t < e for b, e in rngs):
                        work.append(t)

    # ---- operands -------------------------------------------------------------------------

    def ea(self, ins, op):
        m = op.mem
        if m.segment != 0:
            raise Unsupported(f"segment override at {ins.address:x}")
        if m.base == X.X86_REG_RIP:
            addr = (ins.address + ins.size + m.disp) & 0xffffffffffffffff
            self.data_refs.add(addr)
            if ins.mnemonic == "lea":
                self.table_bases.add(addr)
            return f"{addr:#x}u64"
        base = -1 if m.base == 0 else GPR[ins.reg_name(m.base)][0]
        index = -1 if m.index == 0 else GPR[ins.reg_name(m.index)][0]
        return f"m.ea({base}, {index}, {m.scale}, {m.disp})"

    def val(self, ins, op, size=None):
        size = size or op.size
        if op.type == X.X86_OP_IMM:
            return f"{op.imm & ((1 << (8 * size)) - 1):#x}u64"
        if op.type == X.X86_OP_REG:
            i, sz, hi = GPR[ins.reg_name(op.reg)]
            return f"m.gr({i}, {sz}, {str(hi).lower()})"
        if op.type == X.X86_OP_MEM:
            return f"m.ld({self.ea(ins, op)}, {size})"
        raise Unsupported(ins.op_str)

    def put(self, ins, op, expr, size=None):
        size = size or op.size
        if op.type == X.X86_OP_REG:
            i, sz, hi = GPR[ins.reg_name(op.reg)]
            return f"{{ let v = {expr}; m.sr({i}, {sz}, {str(hi).lower()}, v); }}"
        if op.type == X.X86_OP_MEM:
            return f"{{ let v = {expr}; let a = {self.ea(ins, op)}; m.st(a, {size}, v); }}"
        raise Unsupported(ins.op_str)

    def x64(self, ins, op):
        if op.type == X.X86_OP_REG:
            return f"m.xl({xmm_index(ins.reg_name(op.reg))})"
        return f"m.ld({self.ea(ins, op)}, 8)"

    def x128(self, ins, op):
        if op.type == X.X86_OP_REG:
            return f"m.x[{xmm_index(ins.reg_name(op.reg))}]"
        return f"m.ld128({self.ea(ins, op)})"

    # ---- instructions ---------------------------------------------------------------------

    def jump_table(self, ins_list, k, rngs):
        """The targets of the indirect jump at ins_list[k]: found from the table load before it."""
        for j in range(k - 1, max(k - 12, -1), -1):
            i = ins_list[j]
            if i.mnemonic in ("mov", "movsxd", "movzx") and len(i.operands) == 2 and i.operands[1].type == X.X86_OP_MEM:
                mem = i.operands[1].mem
                if mem.index != 0 and mem.scale == 4 and mem.disp:
                    # entries are 32-bit offsets from the image base, scaled by 4
                    table = self.im.base + (mem.disp & 0xffffffff)
                    out = []
                    n = 0
                    while True:
                        o = self.im.off(table + 4 * n)
                        if o is None:
                            break
                        t = self.im.base + struct.unpack_from("<I", self.im.b, o)[0]
                        if not any(b <= t < e for b, e in rngs):
                            break
                        out.append(t)
                        n += 1
                    if out:
                        self.data_refs.add(table)
                        self.table_bases.add(table)
                        return out
        raise Unsupported(f"jump table for the jump at {ins_list[k].address:x}")

    def lift_ins(self, ins, rng, ins_list, k):
        inside = lambda t: any(b <= t < e for b, e in rng)
        mn, ops = ins.mnemonic, ins.operands
        if mn.startswith("lock "):
            mn = mn[5:]
        if mn in ("nop", "int3", "fnop") or mn.startswith("nop") or mn in ("rep nop", "pause"):
            return "", False
        val, put, ea = self.val, self.put, self.ea
        if mn in ("mov", "movabs"):
            return put(ins, ops[0], val(ins, ops[1], ops[0].size)), False
        if mn == "movsxd":
            return put(ins, ops[0], f"crate::mach::sx({val(ins, ops[1], 4)}, 4)"), False
        if mn == "movzx":
            return put(ins, ops[0], val(ins, ops[1], ops[1].size)), False
        if mn == "movsx":
            return put(ins, ops[0], f"crate::mach::sx({val(ins, ops[1], ops[1].size)}, {ops[1].size})"), False
        if mn == "lea":
            a = ea(ins, ops[1])
            return put(ins, ops[0], a), False
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
            sz = ops[0].size
            if len(ops) == 2:
                return put(ins, ops[0], f"m.imul({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz})"), False
            return put(ins, ops[0], f"m.imul({val(ins, ops[1], sz)}, {val(ins, ops[2], sz)}, {sz})"), False
        if mn == "mul":
            if ops[0].size != 8:
                raise Unsupported("mul r/m32 at %x" % ins.address)
            return f"m.mul64({val(ins, ops[0])});", False
        if mn in ("bt", "bts", "btr", "btc"):
            sz = ops[0].size
            bit = val(ins, ops[1], 1)
            expr = {"bt": None, "bts": "v | (1u64 << b)", "btr": "v & !(1u64 << b)", "btc": "v ^ (1u64 << b)"}[mn]
            new = ""
            if expr:
                new = put(ins, ops[0], expr.replace("v", "old").replace("b", "bit"))
            return (f"{{ let old = {val(ins, ops[0])}; let bit = ({bit}) % {8 * sz}; m.cf = old >> bit & 1 != 0; {new} }}"), False
        if mn == "cdq":
            return "m.r[2] = if m.r[0] & 0x8000_0000 != 0 { 0xffff_ffff } else { 0 };", False
        if mn == "cqo":
            return "m.r[2] = if m.r[0] >> 63 != 0 { u64::MAX } else { 0 };", False
        if mn == "cdqe":
            return "m.r[0] = crate::mach::sx(m.r[0] & 0xffff_ffff, 4);", False
        if mn.startswith("set"):
            return put(ins, ops[0], f"m.cc(\"{mn[3:]}\") as u64"), False
        if mn.startswith("cmov"):
            sz = ops[0].size
            i, _, _ = GPR[ins.reg_name(ops[0].reg)]
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
            si = xmm_index(ins.reg_name(s.reg))
            n = 4 if mn == "movd" else 8
            return put(ins, d, f"(m.xl({si}) & {(1 << 8 * n) - 1:#x})", n), False
        if mn in ("movaps", "movapd", "movdqa", "movups", "movdqu", "movupd"):
            d, s = ops
            if d.type == X.X86_OP_REG:
                return f"m.x[{xmm_index(ins.reg_name(d.reg))}] = {self.x128(ins, s)};", False
            return f"{{ let v = {self.x128(ins, s)}; let a = {ea(ins, d)}; m.st128(a, v); }}", False
        if mn in ("addsd", "subsd", "mulsd", "divsd", "sqrtsd"):
            return f"m.{mn}({xmm_index(ins.reg_name(ops[0].reg))}, {self.x64(ins, ops[1])});", False
        if mn in ("maxsd", "minsd"):
            di = xmm_index(ins.reg_name(ops[0].reg))
            # when either is a NaN, or both are zero, the source operand is the answer
            cmp = ">" if mn == "maxsd" else "<"
            return (f"{{ let a = f64::from_bits(m.xl({di})); let b = f64::from_bits({self.x64(ins, ops[1])}); "
                    f"m.set_xl({di}, if a {cmp} b {{ a.to_bits() }} else {{ b.to_bits() }}); }}"), False
        if mn == "sqrtpd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let s = {self.x128(ins, ops[1])}; m.x[{di}] = (f64::from_bits(s as u64).sqrt().to_bits() as u128) | "
                    f"((f64::from_bits((s >> 64) as u64).sqrt().to_bits() as u128) << 64); }}"), False
        if mn in ("comisd", "ucomisd"):
            return f"m.comisd({self.x64(ins, ops[0])}, {self.x64(ins, ops[1])});", False
        if mn in ("xorpd", "xorps", "pxor", "andpd", "andps", "pand", "orpd", "orps", "por"):
            op = "^" if "xor" in mn else "|" if mn in ("orpd", "orps", "por") else "&"
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"m.x[{di}] = m.x[{di}] {op} {self.x128(ins, ops[1])};", False
        if mn in ("andnpd", "andnps", "pandn"):
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"m.x[{di}] = !m.x[{di}] & {self.x128(ins, ops[1])};", False
        if mn == "unpcklpd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"m.x[{di}] = (m.x[{di}] & u64::MAX as u128) | (({self.x128(ins, ops[1])} & u64::MAX as u128) << 64);", False
        if mn == "unpckhpd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"m.x[{di}] = (m.x[{di}] >> 64) | ({self.x128(ins, ops[1])} & (u64::MAX as u128) << 64);", False
        if mn in ("shufpd",):
            di = xmm_index(ins.reg_name(ops[0].reg))
            imm = ops[2].imm
            lo = "m.x[%d]" % di
            return (f"{{ let d = m.x[{di}]; let s = {self.x128(ins, ops[1])}; "
                    f"let l = if {imm} & 1 == 0 {{ d as u64 }} else {{ (d >> 64) as u64 }}; "
                    f"let h = if {imm} & 2 == 0 {{ s as u64 }} else {{ (s >> 64) as u64 }}; "
                    f"m.x[{di}] = l as u128 | (h as u128) << 64; }}"), False
        if mn in ("movlpd", "movlps"):
            d, sop = ops
            if d.type == X.X86_OP_REG:
                di = xmm_index(ins.reg_name(d.reg))
                return f"m.x[{di}] = (m.x[{di}] & !(u64::MAX as u128)) | m.ld({self.ea(ins, sop)}, 8) as u128;", False
            return f"{{ let v = m.xl({xmm_index(ins.reg_name(sop.reg))}); let a = {self.ea(ins, d)}; m.st(a, 8, v); }}", False
        if mn in ("movhpd", "movhps"):
            d, sop = ops
            if d.type == X.X86_OP_REG:
                di = xmm_index(ins.reg_name(d.reg))
                return f"m.x[{di}] = (m.x[{di}] & (u64::MAX as u128)) | ((m.ld({self.ea(ins, sop)}, 8) as u128) << 64);", False
            return f"{{ let v = (m.x[{xmm_index(ins.reg_name(sop.reg))}] >> 64) as u64; let a = {self.ea(ins, d)}; m.st(a, 8, v); }}", False
        if mn == "psrldq":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"m.x[{di}] >>= {8 * ops[1].imm};", False
        if mn == "movmskpd":
            return put(ins, ops[0], f"m.movmskpd({xmm_index(ins.reg_name(ops[1].reg))})"), False
        if mn == "psrlq":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let s = m.x[{di}]; let n = {ops[1].imm}; m.x[{di}] = (((s as u64) >> n) as u128) | "
                    f"((((s >> 64) as u64) >> n) as u128) << 64; }}"), False
        if mn == "psubq":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let a = m.x[{di}]; let b = {self.x128(ins, ops[1])}; "
                    f"m.x[{di}] = ((a as u64).wrapping_sub(b as u64) as u128) | "
                    f"(((a >> 64) as u64).wrapping_sub((b >> 64) as u64) as u128) << 64; }}"), False
        if mn == "psubd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let a = m.x[{di}]; let b = {self.x128(ins, ops[1])}; let mut r = 0u128; "
                    f"for k in 0..4 {{ r |= ((((a >> (32 * k)) as u32).wrapping_sub((b >> (32 * k)) as u32)) as u128) << (32 * k); }} "
                    f"m.x[{di}] = r; }}"), False
        if mn == "cvtdq2pd":
            return f"m.cvtdq2pd({xmm_index(ins.reg_name(ops[0].reg))}, {self.x64(ins, ops[1])});", False
        if mn in ("cvtpd2dq", "cvttpd2dq"):
            return (f"m.cvtpd2dq({xmm_index(ins.reg_name(ops[0].reg))}, {self.x128(ins, ops[1])}, "
                    f"{str(mn == 'cvttpd2dq').lower()});"), False
        if mn in ("cvttsd2si",):
            return put(ins, ops[0], f"m.cvttsd2si({self.x64(ins, ops[1])}, {ops[0].size})"), False
        if mn == "cvtsi2sd":
            sz = ops[1].size
            return f"m.cvtsi2sd({xmm_index(ins.reg_name(ops[0].reg))}, {val(ins, ops[1], sz)}, {sz});", False
        # ---- control flow ----
        if mn in ("ret", "repz ret"):
            return "m.pop(); return;", True
        if mn == "jmp":
            if ops[0].type == X.X86_OP_IMM:
                t = ops[0].imm
                if inside(t):
                    return f"pc = {t:#x}; continue;", True
                if t in self.stubs:
                    return f"{self.stubs[t]} m.pop(); return;", True
                if t in self.known:
                    return f"f_{t:x}(m); return;", True
                if t in self.failing:
                    return f"m.fail(\"an error path at {t:#x}\");", True
                raise Unsupported(f"jump out of the function to {t:#x} at {ins.address:x}")
            a, name = self.import_of(ins)
            if name is not None:
                code = self.imports.get(name)
                if code is None:
                    return f"m.fail(\"the import {name}\");", True
                return f"{code} m.pop(); return;", True
            if ops[0].type == X.X86_OP_REG:
                targets = self.jump_table(ins_list, k, rng)
                reg = GPR[ins.reg_name(ops[0].reg)][0]
                arms = " ".join(f"{t:#x} => {t:#x}," for t in targets)
                return f"pc = m.r[{reg}]; continue;", True
            raise Unsupported("indirect jump at %x" % ins.address)
        if mn.startswith("j"):
            t = ops[0].imm
            if not inside(t):
                if t in self.failing or t in self.stubs or t in self.known:
                    # a conditional tail call: branch to a stub that does the call and returns
                    return f"if m.cc(\"{mn[1:]}\") {{ {self.call_stmt(t)} m.pop(); return; }}", False
                raise Unsupported(f"conditional jump out of the function at {ins.address:x}")
            return f"if m.cc(\"{mn[1:]}\") {{ pc = {t:#x}; continue; }}", False
        if mn == "call":
            if ops[0].type == X.X86_OP_IMM:
                return self.call_stmt(ops[0].imm), False
            a, name = self.import_of(ins)
            if name is not None:
                code = self.imports.get(name)
                if code is None:
                    return f"m.fail(\"the import {name}\");", False
                return code, False
            return f"m.fail(\"an indirect call at {ins.address:#x}\");", False
        raise Unsupported(f"{mn} {ins.op_str} at {ins.address:x}")

    def call_stmt(self, t):
        if t in self.stubs:
            return self.stubs[t]
        if t in self.known:
            return f"m.push(0); f_{t:x}(m);"
        if t in self.failing:
            return f"m.fail(\"an error path at {t:#x}\");"
        raise Unsupported(f"call to {t:#x}")

    def read16(self, va):
        """16 bytes of the image at `va`; the part of a section beyond its file data reads as zeros."""
        r = va - self.im.base
        for n, v, vs, rp, rs in self.im.secs:
            if v <= r < v + max(vs, rs):
                have = max(0, min(16, rs - (r - v)))
                return self.im.b[rp + r - v:rp + r - v + have] + bytes(16 - have)
        return None

    # ---- output ---------------------------------------------------------------------------

    def lift_function(self, s):
        ins_list, rng = self.disasm(s)
        inside = lambda t: any(b <= t < e for b, e in rng)
        leaders = {s}
        jt = {}
        for k, i in enumerate(ins_list):
            if i.mnemonic == "jmp" and i.operands[0].type == X.X86_OP_IMM and inside(i.operands[0].imm):
                leaders.add(i.operands[0].imm)
            elif i.mnemonic.startswith("j") and i.mnemonic != "jmp" and i.operands[0].type == X.X86_OP_IMM:
                if inside(i.operands[0].imm):
                    leaders.add(i.operands[0].imm)
            elif i.mnemonic == "jmp" and i.operands[0].type == X.X86_OP_REG:
                jt[k] = self.jump_table(ins_list, k, rng)
                leaders.update(jt[k])
            if i.mnemonic in ("jmp", "ret", "repz ret") or (i.mnemonic.startswith("j") and i.mnemonic != "jmp"):
                leaders.add(i.address + i.size)
        lines = [f"pub(super) fn f_{s:x}(m: &mut M) {{", f"    let mut pc: u64 = {s:#x};", "    loop {", "        match pc {"]
        cur, body, terminated = None, [], True

        def close(next_addr):
            nonlocal cur, body
            if cur is None:
                return
            lines.append(f"            {cur:#x} => {{")
            lines.extend("                " + b for b in body if b)
            if not terminated:
                lines.append(f"                pc = {next_addr:#x}; continue;")
            lines.append("            }")
            cur, body = None, []

        for k, i in enumerate(ins_list):
            if i.address in leaders:
                close(i.address)
                cur, body, terminated = i.address, [], False
            if cur is None:
                continue
            try:
                text, term = self.lift_ins(i, rng, ins_list, k)
            except Unsupported as why:
                # Reached only on paths a test would show: running it stops with this message.
                self.warnings.append(str(why))
                text, term = f'm.fail("{str(why)}");', False
            body.append(f"// {i.address:x} {i.mnemonic} {i.op_str}")
            body.append(text)
            if term:
                terminated = True
                close(0)
                cur = None
        close(0)
        lines += ["            _ => unreachable!(\"jump to {pc:#x}\"),", "        }", "    }", "}", ""]
        return "\n".join(lines)

    def write(self, out_dir, name, header, trailer=""):
        self.discover()
        parts = [f"//! Generated by tools/{header} from the machine code of {name}. Do not edit.", "",
                 "#![allow(clippy::all, unused_variables, unused_assignments, unused_mut, unused_parens)]",
                 "use crate::mach::*;", ""]
        for s in sorted(self.known):
            parts.append(self.lift_function(s))
        spans = [(a, a + 16) for a in self.data_refs] + [(a, a + 8192) for a in self.table_bases]
        spans.sort()
        merged = []
        for a, b in spans:
            if merged and a <= merged[-1][1]:
                merged[-1][1] = max(merged[-1][1], b)
            else:
                merged.append([a, b])
        blob, ranges = bytearray(), []
        for a, b in merged:
            chunk = bytearray()
            for va in range(a, b, 16):
                got = self.read16(va)
                if got is None:
                    break
                chunk += got
            ranges.append((a, len(blob), len(chunk)))
            blob += chunk
        # Data imported from another DLL (_Nan, _Inf): the import slot holds a pointer to the value.
        data_imports = {"_Nan": 0x7ff8000000000000, "_LNan": 0x7ff8000000000000,
                        "_Inf": 0x7ff0000000000000, "_LInf": 0x7ff0000000000000}
        fake = 0x5000_0000_0000
        for slot, imp in sorted(self.iat.items()):
            if imp in data_imports and slot in self.data_refs:
                at = next(i for i, (a, o, n) in enumerate(ranges) if a <= slot < a + n)
                a, o, n = ranges[at]
                struct.pack_into("<Q", blob, o + slot - a, fake)
                ranges.append((fake, len(blob), 16))
                blob += struct.pack("<QQ", data_imports[imp], 0)
                fake += 16
        stem = name.lower().replace(".dll", "").replace(".s2l", "")
        parts.append(f'static BLOB: &[u8] = include_bytes!("{stem}.dat");')
        parts.append("/// (address in the DLL, offset in the blob, length)")
        parts.append("const RANGES: &[(u64, u32, u32)] = &[")
        for a, o, n in ranges:
            parts.append(f"    ({a:#x}, {o}, {n}),")
        parts.append("];")
        parts.append("pub(super) static IMG: Img = Img { blob: BLOB, ranges: RANGES };")
        parts.append(trailer)
        open(os.path.join(out_dir, f"{stem}_gen.rs"), "w").write("\n".join(parts) + "\n")
        open(os.path.join(out_dir, f"{stem}.dat"), "wb").write(blob)
        for w in self.warnings:
            print("warning:", w)
        print(f"{len(self.known)} routines translated, {len(self.failing)} error paths cut, "
              f"{len(blob)} bytes of tables in {len(ranges)} ranges")
