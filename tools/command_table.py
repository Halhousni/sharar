#!/usr/bin/env python3
"""List the commands runtime.s2l registers with the script engine.

Every command object has an initialiser that ends in one call to
CBaseCmd::IXCmd(this, name, min_args, max_args, descriptor), imported from
compiler.dll. This finds each such call, replays the few instructions that set up
its arguments, and reads the name and the descriptor out of the file.

    tools/command_table.py runtime.s2l > commands.json
    tools/command_table.py runtime.s2l --rust > smr/src/commands_table.rs

Needs `capstone` (pip install capstone). Output is JSON: one record per command, in
the order the initialisers sit in the file.
"""
import json, re, struct, sys
import capstone
from capstone import x86_const as X

def load(path):
    b = open(path, 'rb').read()
    pe = struct.unpack_from('<I', b, 0x3c)[0]
    ns, = struct.unpack_from('<H', b, pe + 6)
    osz, = struct.unpack_from('<H', b, pe + 20)
    base, = struct.unpack_from('<Q', b, pe + 48)
    secs, o = [], pe + 24 + osz
    for _ in range(ns):
        name = b[o:o + 8].rstrip(b'\0')
        vs, va, rs, rp = struct.unpack_from('<IIII', b, o + 8)
        secs.append((name, va, vs, rp, rs)); o += 40
    dd = pe + 24 + 112
    return b, base, secs, dd

def main(path):
    b, base, secs, dd = load(path)
    def off(va):
        r = va - base
        for n, v, s, rp, rs in secs:
            if v <= r < v + max(s, rs): return rp + r - v
    def cstr(va):
        o = off(va); return b[o:b.index(b'\0', o)].decode('latin1')
    # the IAT slot of CBaseCmd::IXCmd, by import name
    imp_rva, = struct.unpack_from('<I', b, dd + 8)
    def rva_off(r):
        for n, v, s, rp, rs in secs:
            if v <= r < v + max(s, rs): return rp + r - v
    slot = None
    o = rva_off(imp_rva)
    while True:
        oft, _, _, name_rva, iat = struct.unpack_from('<IIIII', b, o)
        if name_rva == 0: break
        t = rva_off(oft or iat); k = 0
        while True:
            e, = struct.unpack_from('<Q', b, t + 8 * k)
            if e == 0: break
            if not e >> 63:
                nm = b[rva_off(e) + 2:].split(b'\0')[0]
                if nm == b'?IXCmd@CBaseCmd@@IEAAFPEBDFFPEBF@Z': slot = base + iat + 8 * k
            k += 1
        o += 20
    assert slot, 'IXCmd is not imported'
    text = next(s for s in secs if s[0] == b'.text')
    tva = base + text[1]
    code = b[text[3]:text[3] + text[4]]
    # function starts from .pdata, so each initialiser is decoded on its own
    pd = next(s for s in secs if s[0] == b'.pdata')
    starts = sorted(struct.unpack_from('<I', b, pd[3] + 12 * i)[0] + base for i in range(pd[2] // 12))
    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64); md.detail = True
    regs = {'rcx': X.X86_REG_RCX, 'rdx': X.X86_REG_RDX, 'r8': X.X86_REG_R8, 'r9': X.X86_REG_R9}
    recs, shared = [], []
    for i, s in enumerate(starts):
        e = starts[i + 1] if i + 1 < len(starts) else s + 0x400
        if s < tva or e > tva + len(code): continue
        val, stack, loaded = {}, {}, {}
        def canon(r):
            n = md.reg_name(r)
            return {'ecx': 'rcx', 'edx': 'rdx', 'r8d': 'r8', 'r9d': 'r9', 'eax': 'rax'}.get(n, n)
        for ins in md.disasm(code[s - tva:e - tva], s):
            m, ops = ins.mnemonic, ins.operands
            if m == 'call' and ops[0].type == X.X86_OP_MEM and ops[0].mem.base == X.X86_REG_RIP \
                    and ins.address + ins.size + ops[0].mem.disp == slot:
                d = stack.get(0x20)
                if val.get('rdx') is None and 'rdx' in loaded and d is not None:
                    # the name is a member the constructor fills in: CursorLabel and HCursorLabel
                    # share one initialiser. Their constructors are paired up below.
                    shared.append((loaded['rdx'], s, val.get('r8'), val.get('r9'), d))
                    continue
                if val.get('rdx') is None or d is None:
                    print('skipped: no constant name or descriptor at %#x' % ins.address, file=sys.stderr)
                    continue
                recs.append(dict(site=ins.address, init=s, name=cstr(val['rdx']),
                                 min=val.get('r8'), max=val.get('r9'), desc=d))
                continue
            if m in ('lea', 'mov', 'xor') and len(ops) == 2:
                dst, src = ops
                v = None
                if m == 'mov' and src.type == X.X86_OP_MEM and dst.type == X.X86_OP_REG \
                        and src.mem.base != X.X86_REG_RIP:
                    loaded[canon(dst.reg)] = src.mem.disp
                if m == 'lea' and src.type == X.X86_OP_MEM and src.mem.base == X.X86_REG_RIP:
                    v = ins.address + ins.size + src.mem.disp
                elif m == 'lea' and src.type == X.X86_OP_MEM and src.mem.index == 0 \
                        and val.get(canon(src.mem.base)) is not None:
                    v = val[canon(src.mem.base)] + src.mem.disp
                elif m == 'mov' and src.type == X.X86_OP_IMM: v = src.imm
                elif m == 'mov' and src.type == X.X86_OP_REG: v = val.get(canon(src.reg))
                elif m == 'xor' and dst.type == X.X86_OP_REG and src.type == X.X86_OP_REG and dst.reg == src.reg: v = 0
                if dst.type == X.X86_OP_REG: val[canon(dst.reg)] = v
                elif dst.type == X.X86_OP_MEM and dst.mem.base == X.X86_REG_RSP: stack[dst.mem.disp] = v
    for fld, init, mn, mx, d in shared:
        for i, s in enumerate(starts):
            e = starts[i + 1] if i + 1 < len(starts) else s + 0x400
            if s < tva or e > tva + len(code): continue
            last = None
            for ins in md.disasm(code[s - tva:e - tva], s):
                if ins.mnemonic == 'lea' and ins.operands[1].type == X.X86_OP_MEM \
                        and ins.operands[1].mem.base == X.X86_REG_RIP:
                    last = (ins.operands[0].reg, ins.address + ins.size + ins.operands[1].mem.disp)
                elif ins.mnemonic == 'mov' and ins.operands[0].type == X.X86_OP_MEM \
                        and ins.operands[0].mem.disp == fld and last and ins.operands[1].type == X.X86_OP_REG \
                        and ins.operands[1].reg == last[0]:
                    nm = cstr(last[1])
                    if re.fullmatch(r'[A-Za-z][A-Za-z0-9$]{3,}', nm):
                        recs.append(dict(site=ins.address, init=init, name=nm, min=mn, max=mx, desc=d))
    for r in recs:
        d = r['desc']; o = off(d)
        words = list(struct.unpack_from('<64H', b, o))
        ret, variants, cur, ref, k = words[0], [], [], False, 1
        # The vendor's parser (compiler.dll 0x180005c80): a word is a type; 5 closes a
        # variant, 7 closes the list; 0xe..0xf brackets arguments that come back out
        # (the group counts as one); a type is the low 7 bits (at most 0x13), 0x1f00 its
        # array dimensions and 0x80 a flag.
        while True:
            w = words[k]; k += 1
            if w == 0xe: ref = True; cur.append(w); continue
            if w == 0xf: ref = False; cur.append(w); continue
            if w in (5, 7):
                variants.append(cur); cur = []
                if w == 7: break
                continue
            assert (w & 0x7f) <= 0x13, (r['name'], hex(w))
            cur.append(w)
        # a 0xe..0xf group stands for one argument, the 0xe that opens it
        counts = []
        for v in variants:
            n, inside = 0, False
            for w in v:
                if w == 0xe: n += 1; inside = True
                elif w == 0xf: inside = False
                elif not inside: n += 1
            counts.append(n)
        assert min(counts) >= r['min'] and max(counts) == r['max'], (r['name'], counts, r['min'], r['max'])
        r['ret'] = ret; r['variants'] = variants
        del r['desc']; r['desc_va'] = d
    if '--rust' not in sys.argv:
        json.dump(recs, sys.stdout, indent=0); return
    print('// Generated by tools/command_table.py from CED\'s runtime.s2l; do not edit.')
    print('//')
    print('// One entry per command the runtime registers with the script engine, in the order')
    print('// the initialisers sit in the file (not the order the engine numbers them in).')
    print('// `ret` is 0 for none, 1 integer, 2 real, 3 string. `variants` are the argument')
    print('// lists, as the type words the vendor stores: see `Command` in commands.rs.')
    print()
    print('pub(crate) const TABLE: &[(&str, u8, u8, u8, &[&[u16]])] = &[')
    for r in recs:
        vs = ', '.join('&[' + ', '.join(map(str, v)) + ']' for v in r['variants'])
        print('    (%s, %d, %d, %d, &[%s]),' % (json.dumps(r['name']), r['min'], r['max'], r['ret'], vs))
    print('];')

main(sys.argv[1])
