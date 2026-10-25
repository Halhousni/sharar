#!/usr/bin/env python3
"""List the commands runtime.s2l registers with the script engine, in the order the engine numbers them.

    tools/command_table.py runtime.s2l > commands.json
    tools/command_table.py runtime.s2l --rust > smr/src/script/commands_table.rs

How the file says what it registers:

* Every command is an object whose initialiser ends in one call to
  CBaseCmd::IXCmd(this, name, min_args, max_args, descriptor), imported from
  compiler.dll. Finding each call and replaying the few instructions that set its
  arguments up gives the name and the descriptor. Two shapes need more: a name chosen
  by `cmp [this+d], v; cmovne rdx, rax` (XLow/XHigh, YLow/YHigh, the IIR filters), and
  an initialiser shared by two commands that read the name from a member the
  constructors fill in (CursorLabel, HCursorLabel).
* The engine numbers a command 0x7a plus its place in compiler.dll's command list, and
  that list is built in construction order: CBaseCmd's constructor appends. A creator
  function allocates each object (`new`, then the constructor, which stores the
  vtable) and calls the next creator, so walking the creators in address order and
  mapping each object's vtable -> its Initialize slot -> the initialiser found above
  gives the registration order.

Needs `capstone` (pip install capstone).
"""
import json, re, struct, sys
import capstone
from capstone import x86_const as X


class Image:
    def __init__(self, path):
        b = self.b = open(path, 'rb').read()
        pe = struct.unpack_from('<I', b, 0x3c)[0]
        ns, = struct.unpack_from('<H', b, pe + 6)
        osz, = struct.unpack_from('<H', b, pe + 20)
        self.base, = struct.unpack_from('<Q', b, pe + 48)
        self.secs, o = [], pe + 24 + osz
        for _ in range(ns):
            name = b[o:o + 8].rstrip(b'\0')
            vs, va, rs, rp = struct.unpack_from('<IIII', b, o + 8)
            self.secs.append((name, va, vs, rp, rs)); o += 40
        self.dd = pe + 24 + 112
        self.text = next(s for s in self.secs if s[0] == b'.text')
        self.tva = self.base + self.text[1]
        self.code = b[self.text[3]:self.text[3] + self.text[4]]
        pd = next(s for s in self.secs if s[0] == b'.pdata')
        self.starts = sorted(struct.unpack_from('<I', b, pd[3] + 12 * i)[0] + self.base
                             for i in range(pd[2] // 12))
        self.ends = {}
        for i in range(pd[2] // 12):
            s, e, _ = struct.unpack_from('<III', b, pd[3] + 12 * i)
            self.ends[s + self.base] = e + self.base

    def off(self, va):
        r = va - self.base
        for n, v, s, rp, rs in self.secs:
            if v <= r < v + max(s, rs): return rp + r - v

    def rva_off(self, r):
        for n, v, s, rp, rs in self.secs:
            if v <= r < v + max(s, rs): return rp + r - v

    def cstr(self, va):
        o = self.off(va); return self.b[o:self.b.index(b'\0', o)].decode('latin1')

    def u64(self, va):
        return struct.unpack_from('<Q', self.b, self.off(va))[0]

    def imports(self, ordinals=False):
        """IAT slot address -> import name. By-ordinal imports are skipped, or with `ordinals`
        named `dll!#ordinal`."""
        b = self.b
        rva, = struct.unpack_from('<I', b, self.dd + 8)
        o, out = self.rva_off(rva), {}
        while True:
            oft, _, _, name_rva, iat = struct.unpack_from('<IIIII', b, o)
            if name_rva == 0: break
            dll = b[self.rva_off(name_rva):].split(b'\0')[0].decode().lower()
            t, k = self.rva_off(oft or iat), 0
            while True:
                e, = struct.unpack_from('<Q', b, t + 8 * k)
                if e == 0: break
                if not e >> 63:
                    out[self.base + iat + 8 * k] = b[self.rva_off(e) + 2:].split(b'\0')[0].decode()
                elif ordinals:
                    out[self.base + iat + 8 * k] = f"{dll}!#{e & 0xffff}"
                k += 1
            o += 20
        return out

    def disasm(self, s, e=None):
        e = e or self.ends.get(s, s + 0x400)
        return list(MD.disasm(self.code[s - self.tva:e - self.tva], s))


MD = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
MD.detail = True
REG = {'ecx': 'rcx', 'edx': 'rdx', 'r8d': 'r8', 'r9d': 'r9', 'r8b': 'r8', 'r9b': 'r9', 'eax': 'rax'}


def canon(r):
    n = MD.reg_name(r)
    return REG.get(n, n)


def rip_target(ins, op):
    return ins.address + ins.size + op.mem.disp


def registrations(im, ixcmd):
    """Every IXCmd call: name, argument counts, descriptor address, and where it was made."""
    recs, shared = [], []
    for s in im.starts:
        if s < im.tva or s >= im.tva + len(im.code): continue
        val, stack, loaded, sel, cmpd = {}, {}, {}, {}, None
        for ins in im.disasm(s):
            m, ops = ins.mnemonic, ins.operands
            if m == 'call' and ops[0].type == X.X86_OP_MEM and ops[0].mem.base == X.X86_REG_RIP \
                    and rip_target(ins, ops[0]) == ixcmd:
                d = stack.get(0x20)
                if 'rdx' in sel:
                    disp, cv, ne, first, second = sel['rdx']
                    for nm, cond in ((first, 'eq' if ne else 'ne'), (second, 'ne' if ne else 'eq')):
                        recs.append(dict(site=ins.address, init=s, name=im.cstr(nm), min=val.get('r8'),
                                         max=val.get('r9'), desc=d, when=dict(field=disp, value=cv, cond=cond)))
                elif val.get('rdx') is None and 'rdx' in loaded and d is not None:
                    shared.append((loaded['rdx'], s, val.get('r8'), val.get('r9'), d))
                elif val.get('rdx') is None or d is None:
                    print('skipped: no constant name or descriptor at %#x' % ins.address, file=sys.stderr)
                else:
                    recs.append(dict(site=ins.address, init=s, name=im.cstr(val['rdx']),
                                     min=val.get('r8'), max=val.get('r9'), desc=d))
                continue
            if m == 'cmp' and len(ops) == 2 and ops[0].type == X.X86_OP_MEM:
                v = ops[1].imm if ops[1].type == X.X86_OP_IMM else \
                    val.get(canon(ops[1].reg)) if ops[1].type == X.X86_OP_REG else None
                cmpd = (ops[0].mem.disp, v)
            if m in ('cmovne', 'cmove') and len(ops) == 2 and cmpd and ops[0].type == X.X86_OP_REG:
                # `cmp [this+d], v; cmovne rdx, rax` takes the second name when the byte is not v.
                sel[canon(ops[0].reg)] = (cmpd[0], cmpd[1], m == 'cmovne',
                                          val.get(canon(ops[0].reg)), val.get(canon(ops[1].reg)))
                continue
            if m in ('lea', 'mov', 'xor') and len(ops) == 2:
                dst, src = ops
                v = None
                if m == 'mov' and src.type == X.X86_OP_MEM and dst.type == X.X86_OP_REG \
                        and src.mem.base != X.X86_REG_RIP:
                    loaded[canon(dst.reg)] = src.mem.disp
                if m == 'lea' and src.type == X.X86_OP_MEM and src.mem.base == X.X86_REG_RIP:
                    v = rip_target(ins, src)
                elif m == 'lea' and src.type == X.X86_OP_MEM and src.mem.index == 0 \
                        and val.get(canon(src.mem.base)) is not None:
                    v = val[canon(src.mem.base)] + src.mem.disp
                elif m == 'mov' and src.type == X.X86_OP_IMM: v = src.imm
                elif m == 'mov' and src.type == X.X86_OP_REG: v = val.get(canon(src.reg))
                elif m == 'xor' and dst.type == X.X86_OP_REG and src.type == X.X86_OP_REG \
                        and dst.reg == src.reg: v = 0
                if dst.type == X.X86_OP_REG: val[canon(dst.reg)] = v
                elif dst.type == X.X86_OP_MEM and dst.mem.base == X.X86_REG_RSP: stack[dst.mem.disp] = v
    for fld, init, mn, mx, d in shared:
        for s in im.starts:
            if s < im.tva or s >= im.tva + len(im.code): continue
            last = None
            for ins in im.disasm(s):
                if ins.mnemonic == 'lea' and ins.operands[1].type == X.X86_OP_MEM \
                        and ins.operands[1].mem.base == X.X86_REG_RIP:
                    last = (ins.operands[0].reg, rip_target(ins, ins.operands[1]))
                elif ins.mnemonic == 'mov' and ins.operands[0].type == X.X86_OP_MEM \
                        and ins.operands[0].mem.disp == fld and last and ins.operands[1].type == X.X86_OP_REG \
                        and ins.operands[1].reg == last[0]:
                    nm = im.cstr(last[1])
                    if re.fullmatch(r'[A-Za-z][A-Za-z0-9$]{3,}', nm):
                        recs.append(dict(site=ins.address, init=init, name=nm, min=mn, max=mx, desc=d))
    return recs


def descriptor(im, r):
    """Parse a descriptor the way compiler.dll 0x180005c80 does, and hold it to its rule."""
    words = list(struct.unpack_from('<64H', im.b, im.off(r['desc'])))
    ret, variants, cur, k = words[0], [], [], 1
    while True:
        w = words[k]; k += 1
        if w in (0xe, 0xf): cur.append(w); continue
        if w in (5, 7):
            variants.append(cur); cur = []
            if w == 7: break
            continue
        assert (w & 0x7f) <= 0x13, (r['name'], hex(w))
        cur.append(w)
    counts = []
    for v in variants:
        n, inside = 0, False
        for w in v:
            if w == 0xe: n += 1; inside = True       # a 0xe..0xf group is one argument
            elif w == 0xf: inside = False
            elif not inside: n += 1
        counts.append(n)
    assert min(counts) >= r['min'] and max(counts) == r['max'], (r['name'], counts, r['min'], r['max'])
    return ret, variants


def creation_order(im, recs, slots):
    """The registration order: commands in the sequence their objects are constructed."""
    NEW = next(a for a in slots_new(im, slots))
    byinit = {}
    for r in recs: byinit.setdefault(r['init'], []).append(r)
    cache = {}

    def vtable_in(fn_va, depth=3):
        if depth == 0 or fn_va not in im.ends: return None
        regs, vt = {}, None
        for q in im.disasm(fn_va):
            if q.mnemonic == 'lea' and q.operands[1].type == X.X86_OP_MEM \
                    and q.operands[1].mem.base == X.X86_REG_RIP:
                regs[q.operands[0].reg] = rip_target(q, q.operands[1])
            if q.mnemonic == 'mov' and q.operands[0].type == X.X86_OP_MEM and q.operands[0].mem.disp == 0 \
                    and q.operands[1].type == X.X86_OP_REG and q.operands[1].reg in regs:
                vt = regs[q.operands[1].reg]
        return vt

    def events(s):
        if s in cache: return cache[s]
        ins = im.disasm(s)
        news = [k for k, i in enumerate(ins) if i.mnemonic == 'call' and i.operands[0].type == X.X86_OP_MEM
                and i.operands[0].mem.base == X.X86_REG_RIP and rip_target(i, i.operands[0]) == NEW]
        nset, ev, fregs = set(news), [], {}
        for k, i in enumerate(ins):
            if i.mnemonic == 'lea' and i.operands[1].type == X.X86_OP_MEM and i.operands[1].mem.base == X.X86_REG_RIP:
                fregs[i.operands[0].reg] = rip_target(i, i.operands[1])
            if k in nset:
                nxt = min([j for j in news if j > k], default=len(ins))
                seg, vt, flags, names, arg = ins[k:nxt], None, {}, [], None
                for q in seg:
                    if q.mnemonic == 'lea' and q.operands[1].type == X.X86_OP_MEM \
                            and q.operands[1].mem.base == X.X86_REG_RIP:
                        fregs[q.operands[0].reg] = rip_target(q, q.operands[1])
                    if q.mnemonic == 'mov' and q.operands[0].type == X.X86_OP_MEM and q.operands[0].mem.disp == 0 \
                            and q.operands[0].mem.base != X.X86_REG_RSP and q.operands[1].type == X.X86_OP_REG \
                            and q.operands[1].reg in fregs:
                        vt = fregs[q.operands[1].reg]
                    if q.mnemonic == 'mov' and q.operands[0].type == X.X86_OP_MEM and q.operands[1].type == X.X86_OP_IMM \
                            and q.operands[0].size == 1:
                        flags[q.operands[0].mem.disp] = q.operands[1].imm
                    if q.mnemonic == 'mov' and q.operands[0].type == X.X86_OP_MEM and q.operands[1].type == X.X86_OP_REG \
                            and q.operands[0].mem.disp == 0x90:
                        names.append(q.address)
                    if arg is None and q.mnemonic == 'xor' and q.op_str == 'edx, edx': arg = 0
                    if arg is None and q.mnemonic == 'mov' and q.op_str in ('edx, 1', 'dl, 1'): arg = 1
                if vt is None:      # the constructor stores the vtable
                    for q in seg:
                        if q.mnemonic == 'call' and q.operands[0].type == X.X86_OP_IMM:
                            vt = vtable_in(q.operands[0].imm); break
                if arg is not None: flags.setdefault('arg', arg)
                ev.append(('new', i.address, vt, names, flags))
            elif i.mnemonic == 'call' and i.operands[0].type == X.X86_OP_IMM:
                ev.append(('call', i.operands[0].imm))
        cache[s] = ev
        return ev

    # creators are functions that allocate; roots are those no other creator calls
    allocs = {}
    for s in im.starts:
        if s < im.tva or s >= im.tva + len(im.code): continue
        n = sum(1 for i in im.disasm(s) if i.mnemonic == 'call' and i.operands[0].type == X.X86_OP_MEM
                and i.operands[0].mem.base == X.X86_REG_RIP and rip_target(i, i.operands[0]) == NEW)
        if n >= 2: allocs[s] = n
    called = {e[1] for s in allocs for e in events(s) if e[0] == 'call' and e[1] in allocs}
    best = []

    def walk(s, out):
        for e in events(s):
            if e[0] == 'new': out.append(e)
            elif e[1] in allocs: walk(e[1], out)

    for root in sorted(set(allocs) - called):
        out = []; walk(root, out)
        if len(out) > len(best): best = out
    order = []
    for _, addr, vt, names, flags in best:
        init = im.u64(vt + 8)
        cs = byinit[init]
        pick = []
        for c in cs:
            w = c.get('when')
            if w:
                fv = flags.get(w['field'], flags.get('arg'))
                assert fv is not None, (hex(addr), c['name'])
                if (fv == w['value']) != (w['cond'] == 'eq'): continue
            elif len(cs) > 1 and names and c['site'] not in names:
                continue
            pick.append(c)
        assert len(pick) == 1, (hex(addr), [c['name'] for c in pick])
        pick[0]['vtable'] = vt
        # Execute is the ninth slot of the vtable (after the destructor, Initialize and six
        # thunks into compiler.dll's base class).
        pick[0]['exec'] = im.u64(vt + 0x40)
        order.append(pick[0])
    return order


def slots_new(im, slots):
    """The IAT slot of operator new: the call after `mov ecx, size` in the first creator."""
    from collections import Counter
    cnt = Counter()
    for s in im.starts:
        if s < im.tva or s >= im.tva + len(im.code): continue
        prev = None
        for i in im.disasm(s):
            if prev is not None and prev.mnemonic == 'mov' and prev.op_str.startswith('ecx, 0x') \
                    and i.mnemonic == 'call' and i.operands[0].type == X.X86_OP_MEM \
                    and i.operands[0].mem.base == X.X86_REG_RIP:
                cnt[rip_target(i, i.operands[0])] += 1
            prev = i
    yield cnt.most_common(1)[0][0]


def extra_table(path):
    """The commands of one of the other libraries (Fitting.s2l, MatEng.s2l, FIRWin.s2l, spikecmd.dll), each with the
    vtable of its object and the Execute that answers it. Their order here is the order they are registered in; the
    engine's real order for them does not matter to what they do."""
    import bisect
    im = Image(path)
    slots = im.imports()
    ixcmd = next(a for a, n in slots.items() if n == '?IXCmd@CBaseCmd@@IEAAFPEBDFFPEBF@Z')
    recs = registrations(im, ixcmd)
    starts = sorted(im.starts)
    def func_of(a):
        return starts[bisect.bisect_right(starts, a) - 1]
    byinit = {}
    for r in recs:
        r['ret'], r['variants'] = descriptor(im, r)
        r['desc_va'] = r.pop('desc')
        byinit.setdefault(func_of(r['site']), []).append(r)
    # the object's class: a vtable whose second slot is the initialiser of the command
    out = []
    for n, v, vs, rp, rs in im.secs:
        if n != b'.rdata':
            continue
        for off in range(rp, rp + rs - 16, 8):
            q = struct.unpack_from('<Q', im.b, off + 8)[0]
            if q in byinit:
                vt = im.base + v + off - rp
                assert len(byinit[q]) == 1, (path, byinit[q])
                r = byinit[q][0]
                r['vtable'] = vt
                r['exec'] = struct.unpack_from('<Q', im.b, off + 0x40)[0]
                out.append(r)
    assert len(out) == len(recs), (path, len(out), len(recs))
    # in the order the script engine would list them: the order of the registrations in the file
    out.sort(key=lambda r: r['site'])
    return out


def main(path):
    if path == '--extra':
        json.dump(extra_table(sys.argv[2]), sys.stdout, indent=0)
        return
    im = Image(path)
    slots = im.imports()
    ixcmd = next(a for a, n in slots.items() if n == '?IXCmd@CBaseCmd@@IEAAFPEBDFFPEBF@Z')
    recs = registrations(im, ixcmd)
    for r in recs:
        r['ret'], r['variants'] = descriptor(im, r)
        r['desc_va'] = r.pop('desc')
    order = creation_order(im, recs, slots)
    assert len(order) == len(recs) == len({r['name'].lower() for r in recs}), (len(order), len(recs))
    if '--rust' not in sys.argv and '--all' not in sys.argv:
        json.dump(order, sys.stdout, indent=0); return
    # the other libraries' commands follow the runtime's
    import os
    here = os.path.dirname(os.path.abspath(path))
    for f in EXTRA_LIBS:
        order += extra_table(os.path.join(here, f))
    if '--rust' not in sys.argv:
        json.dump(order, sys.stdout, indent=0); return
    print('// Generated by tools/command_table.py from CED\'s runtime.s2l; do not edit.')
    print('//')
    print('// One entry per command, in the order the engine registers them: a command\'s symbol')
    print('// word is 0x7a plus its place in this list. `ret` is 0 for none, 1 integer, 2 real,')
    print('// 3 string. `variants` are the argument lists as the type words the vendor stores.')
    print()
    print('pub(crate) const TABLE: &[(&str, u8, u8, u8, &[&[u16]])] = &[')
    for r in order:
        vs = ', '.join('&[' + ', '.join(map(str, v)) + ']' for v in r['variants'])
        print('    (%s, %d, %d, %d, &[%s]),' % (json.dumps(r['name']), r['min'], r['max'], r['ret'], vs))
    print('];')


# The other libraries that register script commands, in the order their commands are listed after the runtime's.
EXTRA_LIBS = ['Fitting.s2l', 'MatEng.s2l', 'FIRWin.s2l', 'spikecmd.dll']

if __name__ == '__main__':
    main(sys.argv[1])
