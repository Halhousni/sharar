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


def exports_by_ordinal(path):
    """{ordinal: name} of a DLL's named exports."""
    b = open(path, "rb").read()
    pe = struct.unpack_from("<I", b, 0x3c)[0]
    ns, = struct.unpack_from("<H", b, pe + 6)
    osz, = struct.unpack_from("<H", b, pe + 20)
    secs, o = [], pe + 24 + osz
    for _ in range(ns):
        vs, va, rs, rp = struct.unpack_from("<IIII", b, o + 8)
        secs.append((va, max(vs, rs), rp)); o += 40
    off = lambda rva: next(rp + rva - va for va, sz, rp in secs if va <= rva < va + sz)
    exp_rva, = struct.unpack_from("<I", b, pe + 24 + 112)
    e = off(exp_rva)
    base, nfun, nnames, afun, anames, aords = struct.unpack_from("<I", b, e + 16)[0], *struct.unpack_from("<IIIII", b, e + 20)
    names = struct.unpack_from("<%dI" % nnames, b, off(anames))
    ords = struct.unpack_from("<%dH" % nnames, b, off(aords))
    out = {}
    for n, ordv in zip(names, ords):
        o2 = off(n)
        out[base + ordv] = b[o2:b.index(b"\0", o2)].decode()
    return out


def write_if_changed(path, data):
    """Leave a file alone if it already says this, so that cargo does not rebuild it."""
    try:
        if open(path, "rb").read() == data:
            return
    except OSError:
        pass
    open(path, "wb").write(data)


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

    def __init__(self, dll, roots, stubs, imports, fail_imports, svc=False, default_import=None, cross=None, bias=0, cross_data=None):
        # `svc`: functions take a second argument, the services the host provides.
        # `default_import(name)` gives the statements for an import `imports` does not list.
        # `cross(name)` gives (module, address) for an import that another translated module
        # supplies, or None.
        self.bias = bias
        self.svc = svc
        self.pointer_hooks = False
        self.default_import = default_import
        self.cross = cross
        self.cross_data = cross_data
        self.import_names = []
        self.dll_path = dll
        self.im = Image(dll)
        self.roots = list(roots)
        self.stubs = stubs
        self.imports = imports
        self.fail_re = re.compile(fail_imports)
        self.args = "m, s" if svc else "m"
        self.iat = self.im.imports(ordinals=True)
        self.resolve_ordinals()
        self.data_refs = set()
        self.table_bases = set()
        self.known = set()
        self.failing = set()
        self.warnings = []
        self.eh_cache = {}
        self.build_ranges()

    def resolve_ordinals(self):
        """Name the imports by ordinal from the exporting DLL's table, looked for in the directory
        of this DLL and in $ORACLE_DIR."""
        dirs = [os.path.dirname(os.path.abspath(self.dll_path))]
        if os.environ.get("ORACLE_DIR"):
            dirs.append(os.environ["ORACLE_DIR"])
        cache = {}
        for slot, name in list(self.iat.items()):
            if "!#" not in name:
                continue
            dll, ordinal = name.split("!#")
            if dll not in cache:
                cache[dll] = {}
                for d in dirs:
                    path = os.path.join(d, dll)
                    if os.path.exists(path):
                        try:
                            cache[dll] = exports_by_ordinal(path)
                        except (StopIteration, struct.error):
                            pass
                        break
                if not cache[dll]:
                    # DLLs such as MFC export by ordinal only; Ghidra ships the names (win64/<dll>.exports)
                    for d in [os.environ.get("GHIDRA_SYMBOLS", ""), os.path.expanduser(
                            "~/spike8-re/tools/ghidra_12.1.4_PUBLIC/Ghidra/Features/Base/data/symbols/win64")]:
                        path = os.path.join(d, dll[:-4] + ".exports")
                        if d and os.path.exists(path):
                            text = open(path, encoding="utf-8", errors="replace").read()
                            cache[dll] = {int(o): n for o, n in re.findall(r'ORDINAL="(\d+)" NAME="([^"]*)"', text)}
                            break
            self.iat[slot] = cache[dll].get(int(ordinal), name)

    def tls_info(self):
        """(template bytes, zero fill, address of the slot index) of the module's thread-local data."""
        im = self.im
        rva, size = struct.unpack_from("<II", im.b, im.dd + 9 * 8)
        if not rva:
            return b"", 0, 0
        o = im.rva_off(rva)
        start, end, index, _, zero = struct.unpack_from("<QQQQI", im.b, o)
        so = im.off(start)
        return im.b[so:so + (end - start)], zero, index

    def dir64_relocs(self):
        """Addresses (in the image) of the 64-bit absolute pointers the loader would relocate."""
        im = self.im
        rva, size = struct.unpack_from("<II", im.b, im.dd + 5 * 8)
        out, o, end = [], im.rva_off(rva), im.rva_off(rva) + size if rva else 0
        while rva and o < end:
            page, blk = struct.unpack_from("<II", im.b, o)
            if blk < 8:
                break
            for k in range((blk - 8) // 2):
                e, = struct.unpack_from("<H", im.b, o + 8 + 2 * k)
                if e >> 12 == 10:
                    out.append(im.base + page + (e & 0xfff))
            o += blk
        return out

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

    def eh_of(self, s):
        """The C++ exception handling of the routine at `s` when it has one try block (the
        __CxxFrameHandler3 FuncInfo): how much its prologue allocates, and for each catch the
        frame offset of the exception pointer, the funclet and the address it resumes at."""
        if s in self.eh_cache:
            return self.eh_cache[s]
        self.eh_cache[s] = None
        im = self.im
        pd = next(sec for sec in im.secs if sec[0] == b".pdata")
        for i in range(pd[2] // 12):
            b, e, u = struct.unpack_from("<III", im.b, pd[3] + 12 * i)
            if im.base + b != s:
                continue
            o = im.rva_off(u)
            flags, prolog, count, fr = im.b[o] >> 3, im.b[o + 1], im.b[o + 2], im.b[o + 3]
            if not flags & 3 or fr & 15:
                return None
            alloc, k = 0, 0
            codes = struct.unpack_from(f"<{count * 2}B", im.b, o + 4)
            while k < count:
                op, info = codes[2 * k + 1] & 15, codes[2 * k + 1] >> 4
                if op == 0:
                    alloc += 8; k += 1
                elif op == 1:
                    if info == 0:
                        alloc += 8 * (codes[2 * k + 2] | codes[2 * k + 3] << 8); k += 2
                    else:
                        alloc += struct.unpack_from("<I", bytes(codes[2 * k + 2:2 * k + 6]))[0]; k += 3
                elif op == 2:
                    alloc += info * 8 + 8; k += 1
                elif op in (4, 8): k += 2
                elif op in (5, 9): k += 3
                else:
                    return None
            hd = o + 4 + ((count + 1) & ~1) * 2
            fi = struct.unpack_from("<I", im.b, hd + 4)[0]
            fo = im.rva_off(fi)
            if fo is None or struct.unpack_from("<I", im.b, fo)[0] & 0xfffffff0 != 0x19930520:
                return None
            ntry, tryrva = struct.unpack_from("<II", im.b, fo + 12)
            nip, iprva = struct.unpack_from("<II", im.b, fo + 20)
            # a funclet carries its parent's FuncInfo; only the parent's state map starts at it
            if ntry != 1 or not nip or struct.unpack_from("<I", im.b, im.rva_off(iprva))[0] != b:
                return None
            lo, hi, chi, ncatch, hrva = struct.unpack_from("<iiiiI", im.b, im.rva_off(tryrva))
            catches = []
            for j in range(ncatch):
                adj, typ, disp, fn, fsz = struct.unpack_from("<IIiII", im.b, im.rva_off(hrva) + 20 * j)
                f = im.base + fn
                resume = None
                for ins in MD.disasm(im.b[im.off(f):im.off(f) + 0x200], f):
                    if ins.mnemonic == "lea" and ins.operands[0].type == X.X86_OP_REG and ins.reg_name(ins.operands[0].reg) == "rax" \
                            and ins.operands[1].mem.base == X.X86_REG_RIP:
                        resume = ins.address + ins.size + ins.operands[1].mem.disp
                    if ins.mnemonic == "ret":
                        break
                if resume is None:
                    return None
                catches.append((disp, f, resume))
            self.eh_cache[s] = {"alloc": alloc, "catches": catches}
            return self.eh_cache[s]
        return None

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
                        # a jump to another routine is a tail call, not part of this one
                        if ins.operands[0].imm == s or ins.operands[0].imm not in self.ranges:
                            work.append(ins.operands[0].imm)
                    elif ins.operands[0].type == X.X86_OP_REG:
                        # a jump table: its targets follow from the instructions before it
                        pre = sorted(seen.values(), key=lambda i: i.address)
                        k = pre.index(ins)
                        try:
                            work.extend(self.jump_table(pre, k, [(max(s - 0x10000, 0), s + 0x10000)]))
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

    def vtable_methods(self, t):
        """If `t` is a table of routine pointers (a vtable), the routines."""
        im = self.im
        out = []
        r = t - im.base
        for n, v, vs, rp, rs in im.secs:
            if n == b".text" or not (v <= r < v + rs):
                continue
            k, misses = 0, 0
            while v <= r + 8 * k < v + rs - 7 and k < 1024:
                ptr, = struct.unpack_from("<Q", im.b, rp + r - v + 8 * k)
                k += 1
                # a vtable slot or an initialiser: an address in the code. A few zeros may come
                # first or between (the markers of the initialiser tables).
                if im.tva <= ptr < im.tva + im.text[2]:
                    out.append(ptr)
                    misses = 0
                elif ptr == 0 and (out or misses < 1):
                    misses += 1
                    if out and misses > 2:
                        break
                else:
                    break
        return out

    def is_function_start(self, t):
        """Is `t` the start of a routine, so that a pointer to it may be called?"""
        im = self.im
        if not (im.tva <= t < im.tva + im.text[2]):
            return False
        if t in self.ranges:
            return True
        # a thunk: jmp qword ptr [rip + import]
        o = im.off(t)
        if im.b[o:o + 2] == b"\xff\x25":
            return True
        # a routine too small to need unwind data (a leaf) is not in the table: it starts after padding
        return im.b[o - 1] in (0xcc, 0x90, 0xc3)

    def A(self, x):
        """An address of this module as the generated code writes it (the module's addresses are
        moved by `bias`, so that no two modules share one)."""
        return x + self.bias

    def import_stmt(self, name, tail=False):
        """Statements that do what a call through the import table to `name` does."""
        if name in self.imports:
            return self.imports[name]
        if self.cross:
            c = self.cross(name)
            if c:
                mod, addr = c
                return f"crate::{mod}::enter_{addr:x}({self.args});"  # addr already biased by cross()
        if self.default_import:
            return self.default_import(self, name)
        return None

    def import_of(self, ins):
        op = ins.operands[0]
        if op.type == X.X86_OP_MEM and op.mem.base == X.X86_REG_RIP:
            a = (ins.address + ins.size + op.mem.disp) & 0xffffffffffffffff
            return a, self.iat.get(a)
        return None, None

    def pointer_roots(self):
        """Every address in the code that the module's data points at (vtable slots, callbacks,
        hooks): the loader would relocate those pointers, so the list is in the relocation table."""
        im = self.im
        out = []
        for ptr_at in self.dir64_relocs():
            o = im.off(ptr_at)
            if o is None:
                continue
            v, = struct.unpack_from("<Q", im.b, o)
            if im.tva <= v < im.tva + im.text[2]:
                out.append(v)
        return out

    def discover(self):
        self.known, self.failing, self.imports_used = set(), set(), set()
        work = list(self.roots)
        if self.pointer_hooks:
            work += self.pointer_roots()
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
            eh = self.eh_of(s)
            if eh:
                work.extend(f for _, f, _ in eh["catches"])
            for i in ins:
                for op in i.operands:
                    if op.type == X.X86_OP_MEM and op.mem.base == X.X86_REG_RIP and i.mnemonic not in ("call", "jmp"):
                        slot = (i.address + i.size + op.mem.disp) & 0xffffffffffffffff
                        if slot in self.iat and not self.iat[slot].startswith("_") and not self.iat[slot] in ("_Nan",):
                            self.imports_used.add(self.iat[slot])
                if i.mnemonic == "lea" and i.operands[1].type == X.X86_OP_MEM and i.operands[1].mem.base == X.X86_REG_RIP:
                    t = (i.address + i.size + i.operands[1].mem.disp) & 0xffffffffffffffff
                    if self.is_function_start(t):
                        work.append(t)
                    else:
                        work.extend(self.vtable_methods(t))
                if i.mnemonic in ("call", "jmp") and i.operands[0].type == X.X86_OP_MEM:
                    _, imp = self.import_of(i)
                    if imp:
                        self.imports_used.add(imp)
                if i.mnemonic in ("call", "jmp") and i.operands[0].type == X.X86_OP_IMM:
                    t = i.operands[0].imm
                    if not any(b <= t < e for b, e in rngs):
                        work.append(t)

    # ---- operands -------------------------------------------------------------------------

    def ea(self, ins, op):
        m = op.mem
        gs = ""
        if m.segment == X.X86_REG_GS:
            gs = ".wrapping_add(m.teb)"
        elif m.segment != 0:
            raise Unsupported(f"segment override at {ins.address:x}")
        if m.base == X.X86_REG_RIP:
            addr = (ins.address + ins.size + m.disp) & 0xffffffffffffffff
            self.data_refs.add(addr)
            if ins.mnemonic == "lea":
                self.table_bases.add(addr)
            return f"{self.A(addr):#x}u64"
        base = -1 if m.base == 0 else GPR[ins.reg_name(m.base)][0]
        index = -1 if m.index == 0 else GPR[ins.reg_name(m.index)][0]
        return f"m.ea({base}, {index}, {m.scale}, {m.disp}){gs}"

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
        if mn in ("adc", "sbb"):
            sz = ops[0].size
            return put(ins, ops[0], f"m.{mn}({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz})"), False
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
        if mn in ("shl", "shr", "sar", "rol", "ror"):
            sz = ops[0].size
            cnt = "1" if len(ops) == 1 else val(ins, ops[1], 1)
            return put(ins, ops[0], f"m.{mn}({val(ins, ops[0])}, {cnt}, {sz})"), False
        if mn == "imul":
            if len(ops) == 1:
                if ops[0].size == 4:
                    return f"m.imul1_32({val(ins, ops[0])});", False
                if ops[0].size == 8:
                    return f"m.imul1_64({val(ins, ops[0])});", False
                raise Unsupported("imul r/m%d at %x" % (8 * ops[0].size, ins.address))
            sz = ops[0].size
            if len(ops) == 2:
                return put(ins, ops[0], f"m.imul({val(ins, ops[0])}, {val(ins, ops[1], sz)}, {sz})"), False
            return put(ins, ops[0], f"m.imul({val(ins, ops[1], sz)}, {val(ins, ops[2], sz)}, {sz})"), False
        if mn in ("idiv", "div"):
            sz = ops[0].size
            return f"m.{mn}({val(ins, ops[0])}, {sz});", False
        if mn == "mul":
            if ops[0].size == 4:
                return f"m.mul32({val(ins, ops[0])});", False
            if ops[0].size != 8:
                raise Unsupported("mul r/m%d at %x" % (8 * ops[0].size, ins.address))
            return f"m.mul64({val(ins, ops[0])});", False
        if mn in ("bt", "bts", "btr", "btc"):
            sz = ops[0].size
            bit = val(ins, ops[1], 1)
            expr = {"bt": None, "bts": "v | (1u64 << b)", "btr": "v & !(1u64 << b)", "btc": "v ^ (1u64 << b)"}[mn]
            new = ""
            if expr:
                new = put(ins, ops[0], expr.replace("v", "old").replace("b", "bit"))
            return (f"{{ let old = {val(ins, ops[0])}; let bit = ({bit}) % {8 * sz}; m.cf = old >> bit & 1 != 0; {new} }}"), False
        m_ = re.fullmatch(r"(rep )?(stos|movs)([bwdq])", mn)
        if m_ and not any(o.type == X.X86_OP_REG and xmm_index(ins.reg_name(o.reg)) is not None for o in ops):
            n = {"b": 1, "w": 2, "d": 4, "q": 8}[m_.group(3)]
            rep = bool(m_.group(1))
            loop = "m.r[1]" if rep else "1"
            body = ("let v = m.r[0] & %#x; m.st(m.r[7], %d, v);" % ((1 << 8 * n) - 1, n)) if m_.group(2) == "stos" else \
                   ("let v = m.ld(m.r[6], %d); m.st(m.r[7], %d, v); m.r[6] = m.r[6].wrapping_add(%d);" % (n, n, n))
            return (f"{{ let count: u64 = {loop}; for _ in 0..count {{ {body} m.r[7] = m.r[7].wrapping_add({n}); }} "
                    f"{'m.r[1] = 0;' if rep else ''} }}"), False
        if mn == "cwde":
            return "m.r[0] = crate::mach::sx(m.r[0] & 0xffff, 2) & 0xffff_ffff;", False
        if mn == "cbw":
            return "m.r[0] = m.r[0] & !0xffff | crate::mach::sx(m.r[0] & 0xff, 1) & 0xffff;", False
        if mn == "cwd":
            return "m.r[2] = m.r[2] & !0xffff | if m.r[0] & 0x8000 != 0 { 0xffff } else { 0 };", False
        if mn == "xchg":
            a, b = ops
            sz = a.size
            return (f"{{ let x = {val(ins, a)}; let y = {val(ins, b)}; {put(ins, a, 'y')} {put(ins, b, 'x')} }}"), False
        if mn == "cmpxchg":
            a, b = ops
            sz = a.size
            return (f"{{ let d = {val(ins, a)}; let acc = m.gr(0, {sz}, false); m.sub(acc, d, {sz}); "
                    f"if d == acc {{ {put(ins, a, val(ins, b))} }} else {{ m.sr(0, {sz}, false, d); }} }}"), False
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
        if mn in ("movss",):
            d, sop = ops
            if d.type == X.X86_OP_REG and xmm_index(ins.reg_name(d.reg)) is not None:
                di = xmm_index(ins.reg_name(d.reg))
                if sop.type == X.X86_OP_REG:
                    si = xmm_index(ins.reg_name(sop.reg))
                    return f"m.x[{di}] = (m.x[{di}] & !0xffff_ffffu128) | (m.x[{si}] & 0xffff_ffffu128);", False
                return f"m.set_xl_zero({di}, m.ld({ea(ins, sop)}, 4));", False
            si = xmm_index(ins.reg_name(sop.reg))
            return f"{{ let v = m.xl({si}) & 0xffff_ffff; let a = {ea(ins, d)}; m.st(a, 4, v); }}", False
        if mn in ("addss", "subss", "mulss", "divss", "sqrtss"):
            di = xmm_index(ins.reg_name(ops[0].reg))
            src = f"m.xl({xmm_index(ins.reg_name(ops[1].reg))})" if ops[1].type == X.X86_OP_REG else f"m.ld({ea(ins, ops[1])}, 4)"
            return f"m.{mn}({di}, {src});", False
        if mn in ("comiss", "ucomiss"):
            a_ = f"m.xl({xmm_index(ins.reg_name(ops[0].reg))})"
            b_ = f"m.xl({xmm_index(ins.reg_name(ops[1].reg))})" if ops[1].type == X.X86_OP_REG else f"m.ld({ea(ins, ops[1])}, 4)"
            return f"m.comiss({a_}, {b_});", False
        if mn == "cvtss2sd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            src = f"m.xl({xmm_index(ins.reg_name(ops[1].reg))})" if ops[1].type == X.X86_OP_REG else f"m.ld({ea(ins, ops[1])}, 4)"
            return f"m.set_xl({di}, (f32::from_bits({src} as u32) as f64).to_bits());", False
        if mn == "cvtsd2ss":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return f"{{ let v = (f64::from_bits({self.x64(ins, ops[1])}) as f32).to_bits() as u128; m.x[{di}] = (m.x[{di}] & !0xffff_ffffu128) | v; }}", False
        if mn == "cvtps2pd":
            di = xmm_index(ins.reg_name(ops[0].reg))
            src = self.x64(ins, ops[1])
            return (f"{{ let s = {src}; let lo = f32::from_bits(s as u32) as f64; let hi = f32::from_bits((s >> 32) as u32) as f64; "
                    f"m.x[{di}] = lo.to_bits() as u128 | (hi.to_bits() as u128) << 64; }}"), False
        if mn == "cvtpd2ps":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let s = {self.x128(ins, ops[1])}; let lo = (f64::from_bits(s as u64) as f32).to_bits() as u128; "
                    f"let hi = (f64::from_bits((s >> 64) as u64) as f32).to_bits() as u128; m.x[{di}] = lo | hi << 32; }}"), False
        if mn in ("cvtsi2ss",):
            sz = ops[1].size
            return f"m.cvtsi2ss({xmm_index(ins.reg_name(ops[0].reg))}, {val(ins, ops[1], sz)}, {sz});", False
        if mn in ("cvttss2si", "cvtss2si"):
            src = f"m.xl({xmm_index(ins.reg_name(ops[1].reg))})" if ops[1].type == X.X86_OP_REG else f"m.ld({ea(ins, ops[1])}, 4)"
            return put(ins, ops[0], f"m.cvttsd2si((f32::from_bits({src} as u32) as f64).to_bits(), {ops[0].size})"), False
        if mn in ("addpd", "subpd", "mulpd", "divpd"):
            di = xmm_index(ins.reg_name(ops[0].reg))
            op = {"addpd": "+", "subpd": "-", "mulpd": "*", "divpd": "/"}[mn]
            return (f"{{ let d = m.x[{di}]; let s = {self.x128(ins, ops[1])}; "
                    f"let lo = (f64::from_bits(d as u64) {op} f64::from_bits(s as u64)).to_bits() as u128; "
                    f"let hi = (f64::from_bits((d >> 64) as u64) {op} f64::from_bits((s >> 64) as u64)).to_bits() as u128; "
                    f"m.x[{di}] = lo | hi << 64; }}"), False
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
        if mn == "cvtdq2ps":
            di = xmm_index(ins.reg_name(ops[0].reg))
            return (f"{{ let s = {self.x128(ins, ops[1])}; let mut r = 0u128; "
                    f"for k in 0..4 {{ r |= ((((s >> (32 * k)) as u32) as i32 as f32).to_bits() as u128) << (32 * k); }} m.x[{di}] = r; }}"), False
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
                    return f"pc = {self.A(t):#x}; continue;", True
                if t in self.stubs:
                    return f"{self.stubs[t]} m.pop(); return;", True
                if t in self.known:
                    return f"f_{self.A(t):x}({self.args}); return;", True
                if t in self.failing:
                    return f"m.fail(\"an error path at {t:#x}\");", True
                raise Unsupported(f"jump out of the function to {t:#x} at {ins.address:x}")
            a, name = self.import_of(ins)
            if name is not None:
                if self.cross and name not in self.imports:
                    c = self.cross(name)
                    if c:
                        # a tail jump into another module: no new return address, the callee's
                        # `ret` goes straight back to our caller
                        return f"crate::{c[0]}::tail_{c[1]:x}({self.args}); return;", True
                code = self.import_stmt(name, True)
                if code is None:
                    return f"m.fail(\"the import {name}\");", True
                return f"{code} m.pop(); return;", True
            if ops[0].type == X.X86_OP_REG:
                try:
                    targets = self.jump_table(ins_list, k, rng)
                except Unsupported:
                    ptr = val(ins, ops[0], 8)
                    return f"{{ let t = {ptr}; call_ptr({self.args}, t); return; }}", True
                reg = GPR[ins.reg_name(ops[0].reg)][0]
                arms = " ".join(f"{t:#x} => {t:#x}," for t in targets)
                return f"pc = m.r[{reg}]; continue;", True
            if ops[0].type == X.X86_OP_MEM:
                ptr = val(ins, ops[0], 8)
                return f"{{ let t = {ptr}; call_ptr({self.args}, t); return; }}", True
            raise Unsupported("indirect jump at %x" % ins.address)
        if mn.startswith("j"):
            t = ops[0].imm
            if not inside(t):
                if t in self.failing or t in self.stubs or t in self.known:
                    # a conditional tail call: branch to a stub that does the call and returns
                    return f"if m.cc(\"{mn[1:]}\") {{ {self.call_stmt(t)} m.pop(); return; }}", False
                raise Unsupported(f"conditional jump out of the function at {ins.address:x}")
            return f"if m.cc(\"{mn[1:]}\") {{ pc = {self.A(t):#x}; continue; }}", False
        if mn == "call":
            if ops[0].type == X.X86_OP_IMM:
                return self.call_stmt(ops[0].imm), False
            a, name = self.import_of(ins)
            if name is not None:
                code = self.import_stmt(name)
                if code is None:
                    return f"m.fail(\"the import {name}\");", False
                return code, False
            ptr = val(ins, ops[0], 8)
            return f"{{ let t = {ptr}; m.push(0); call_ptr({self.args}, t); }}", False
        raise Unsupported(f"{mn} {ins.op_str} at {ins.address:x}")

    def call_stmt(self, t):
        if t in self.stubs:
            return self.stubs[t]
        if t in self.known:
            return f"m.push(0); f_{self.A(t):x}({self.args});"
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
        eh = self.eh_of(s)
        leaders = {s}
        if eh:
            leaders.update(r for _, _, r in eh["catches"] if inside(r))
        jt = {}
        for k, i in enumerate(ins_list):
            if i.mnemonic == "jmp" and i.operands[0].type == X.X86_OP_IMM and inside(i.operands[0].imm):
                leaders.add(i.operands[0].imm)
            elif i.mnemonic.startswith("j") and i.mnemonic != "jmp" and i.operands[0].type == X.X86_OP_IMM:
                if inside(i.operands[0].imm):
                    leaders.add(i.operands[0].imm)
            elif i.mnemonic == "jmp" and i.operands[0].type == X.X86_OP_REG:
                try:
                    jt[k] = self.jump_table(ins_list, k, rng)
                    leaders.update(jt[k])
                except Unsupported:
                    pass  # a jump through a pointer, not a table: lift_ins stops there
            if i.mnemonic in ("jmp", "ret", "repz ret") or (i.mnemonic.startswith("j") and i.mnemonic != "jmp"):
                leaders.add(i.address + i.size)
        sig = 'm: &mut M, s: &mut dyn Svc' if self.svc else 'm: &mut M'
        lines = []
        if eh:
            name = f"f_{self.A(s):x}"
            lines += [f"pub(super) fn {name}({sig}) {{",
                      "    let entry = m.r[4];", f"    let mut start: u64 = {self.A(s):#x};",
                      "    loop {",
                      f"        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {name}_run(m, s, start))) {{",
                      "            Ok(()) => return,",
                      "            Err(p) => match p.downcast::<CxxThrow>() {",
                      "                Ok(t) => {",
                      f"                    let est = entry.wrapping_sub({eh['alloc']});",
                      "                    m.r[2] = est; handling(t.obj, t.info);"]
            c = eh["catches"][0]
            lines += [f"                    m.st(est.wrapping_add({c[0]} as u64), 8, t.obj);",
                      "                    m.r[4] = est.wrapping_sub(0x200); m.push(0);",
                      f"                    f_{self.A(c[1]):x}(m, s);",
                      "                    start = m.r[0]; m.r[4] = est;",
                      "                }",
                      "                Err(p) => std::panic::resume_unwind(p),",
                      "            },",
                      "        }", "    }", "}", ""]
            lines += [f"fn {name}_run({sig}, start: u64) {{", f"    m.trace_enter({self.A(s):#x});",
                      "    let mut pc: u64 = start;", "    loop {", "        match pc {"]
        else:
            lines = [f"pub(super) fn f_{self.A(s):x}({sig}) {{", f"    m.trace_enter({self.A(s):#x});", f"    let mut pc: u64 = {self.A(s):#x};", "    loop {", "        match pc {"]
        cur, body, terminated = None, [], True

        def close(next_addr):
            nonlocal cur, body
            if cur is None:
                return
            lines.append(f"            {self.A(cur):#x} => {{")
            lines.append(f"                m.block({self.A(cur):#x});")
            lines.extend("                " + b for b in body if b)
            if not terminated:
                lines.append(f"                pc = {self.A(next_addr):#x}; continue;")
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
            except (Unsupported, KeyError) as why:
                # Reached only on paths a test would show: running it stops with this message.
                why = Unsupported(f"{why} at {i.address:x} {i.mnemonic} {i.op_str}") if isinstance(why, KeyError) else why
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

    def import_id(self, name):
        if name not in self.import_names:
            self.import_names.append(name)
        return self.import_names.index(name)

    def write(self, out_dir, name, header, trailer="", entries=(), module=None):
        self.discover()
        parts = [f"//! Generated by tools/{header} from the machine code of {name}. Do not edit.", "",
                 "#![allow(warnings, clippy::all)]",
                 "use crate::mach::*;", ""]
        if self.svc:
            parts.insert(-1, "use crate::Svc;")
        for s in sorted(self.known):
            try:
                parts.append(self.lift_function(s))
            except Unsupported as why:
                raise Unsupported(f"in the routine at {s:#x}: {why}")
        tls_slot_addr = self.tls_info()[2]
        spans = [(a, a + 16) for a in self.data_refs] + [(a, a + 8192) for a in self.table_bases]
        if tls_slot_addr:
            spans.append((tls_slot_addr, tls_slot_addr + 16))
        if getattr(self, "whole_data", False):
            # the module indexes its tables: keep all of its data
            for n, v, vs, rp, rs in self.im.secs:
                if n in (b".rdata", b".data"):
                    spans.append((self.im.base + v, self.im.base + v + max(vs, rs)))
        spans.sort()
        merged = []
        for a, b in spans:
            if merged and a <= merged[-1][1]:
                merged[-1][1] = max(merged[-1][1], b)
            else:
                merged.append([a, b])
        blob, ranges = bytearray(), []
        for a, b in merged:
            # a run of the image, cut where a window runs off the end of a section
            start, chunk = a, bytearray()
            for va in range(a, b, 16):
                got = self.read16(va)
                if got is None:
                    if chunk:
                        ranges.append((start, len(blob), len(chunk)))
                        blob += chunk
                    start, chunk = va + 16, bytearray()
                    continue
                chunk += got
            if chunk:
                ranges.append((start, len(blob), len(chunk)))
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
        # A function imported by pointer (loaded from its import slot and called later): the slot
        # holds a made-up address, and calling it does what a call through the import does.
        fnimp = {}
        fake_fn = 0x4000_0000_0000
        for slot, imp in sorted(self.iat.items()):
            cd = self.cross_data(imp) if self.cross_data and imp not in data_imports else None
            if os.environ.get('LIFT_DEBUG') and 'ComPorts' in imp:
                print('DEBUG', imp, hex(slot), cd, slot in self.data_refs, file=sys.stderr)
            if cd is not None and slot in self.data_refs:
                # a variable of another translated module: the slot holds its address
                at = next(i for i, (a, o, n) in enumerate(ranges) if a <= slot < a + n)
                a, o, n = ranges[at]
                struct.pack_into("<Q", blob, o + slot - a, cd)
                continue
            if imp not in data_imports and slot in self.data_refs:
                at = next(i for i, (a, o, n) in enumerate(ranges) if a <= slot < a + n)
                a, o, n = ranges[at]
                struct.pack_into("<Q", blob, o + slot - a, fake_fn)
                fnimp[fake_fn] = imp
                fake_fn += 16
        # Pointers stored in the tables (vtables, function pointers) follow the module's bias.
        if self.bias:
            size = struct.unpack_from("<I", self.im.b, self.im.dd - 112 + 56 - 112 + 112)[0] if False else None
            lo, hi = self.im.base, self.im.base + max(sec[1] + max(sec[2], sec[4]) for sec in self.im.secs)
            for ptr in self.dir64_relocs():
                for a, o, n in ranges:
                    if a <= ptr < a + n - 7 and a < 0x4000_0000_0000:
                        v, = struct.unpack_from("<Q", blob, o + ptr - a)
                        if lo <= v < hi:
                            struct.pack_into("<Q", blob, o + ptr - a, v + self.bias)
                        break
        stem = module or name.lower().replace(".dll", "").replace(".s2l", "")
        parts.append(f'static BLOB: &[u8] = include_bytes!("{stem}.dat");')
        parts.append("/// (address in the DLL, offset in the blob, length)")
        parts.append("const RANGES: &[(u64, u32, u32)] = &[")
        for a, o, n in ranges:
            parts.append(f"    ({self.A(a):#x}, {o}, {n}),")
        parts.append("];")
        tls_bytes, tls_zero, tls_index_addr = self.tls_info()
        if tls_index_addr:
            at = next((i for i, (a, o, n) in enumerate(ranges) if a <= tls_index_addr < a + n), None)
            if at is not None:
                a, o, n = ranges[at]
                struct.pack_into("<I", blob, o + tls_index_addr - a, self.bias >> 32)
        parts.append(f"static TLS: &[u8] = &{list(tls_bytes)!r};")
        parts.append(f"pub(super) static IMG: Img = Img {{ blob: BLOB, ranges: RANGES, tls: TLS, tls_zero: {tls_zero}, tls_slot: {self.bias >> 32} }};")
        parts.append("pub(super) static IMGS: &[&Img] = &[&IMG];")
        sig = "m: &mut M, s: &mut dyn Svc, t: u64" if self.svc else "m: &mut M, t: u64"
        arms = "\n".join(f"        {self.A(f):#x} => f_{self.A(f):x}({self.args})," for f in sorted(self.known))
        for fa, imp in sorted(fnimp.items()):
            code = self.import_stmt(imp) or f'm.fail("the import {imp}");'
            arms += f"\n        {fa:#x} => {{ {code} m.pop(); }}"
        arms_try = arms
        parts.append(f"/// Call a routine through a pointer (a function pointer or a virtual call).\n"
                     f"fn call_ptr({sig}) {{\n    match t {{\n{arms}\n"
                     f"        _ => {'crate::commands::call_other(m, s, t)' if self.svc else 'm.fail(&format!(\"a call through a pointer to {t:#x}\"))'},\n    }}\n}}\n"
                     f"/// Is `t` a routine of this module? If so, call it.\n"
                     f"pub(crate) fn try_call({sig}) -> bool {{\n    match t {{\n{arms_try}\n        _ => return false,\n    }}\n    true\n}}\n")

        for e in entries:
            if self.svc:
                parts.append(f"pub(crate) fn tail_{self.A(e):x}(m: &mut M, s: &mut dyn Svc) {{\n    f_{self.A(e):x}(m, s)\n}}")
            if self.svc:
                parts.append(f"/// Run the routine at {e:#x} with the machine's registers as the caller set them.\n"
                             f"pub(crate) fn enter_{self.A(e):x}(m: &mut M, s: &mut dyn Svc) {{\n"
                             f"    m.with_img(&IMG, |m| {{ m.push(0); f_{self.A(e):x}(m, s); }});\n}}")
            else:
                parts.append(f"pub(crate) fn enter_{self.A(e):x}(m: &mut M) {{\n"
                             f"    m.with_img(&IMG, |m| {{ m.push(0); f_{self.A(e):x}(m); }});\n}}")
        parts.append(trailer)
        write_if_changed(os.path.join(out_dir, f"{stem}_gen.rs"), ("\n".join(parts) + "\n").encode())
        write_if_changed(os.path.join(out_dir, f"{stem}.dat"), bytes(blob))
        for w in self.warnings:
            print("warning:", w)
        print(f"{len(self.known)} routines translated, {len(self.failing)} error paths cut, "
              f"{len(blob)} bytes of tables in {len(ranges)} ranges")
