#!/usr/bin/env python3
"""The message maps of SONVIEW.exe: which window class handles which user message.

An MFC class lists its handlers in an array of AFX_MSGMAP_ENTRY (message, code, id, last id, signature,
function; 32 bytes each, ended by an all-zero entry); an AFX_MSGMAP {GetBaseMap, entries} ties the array
to a class; the class's `GetMessageMap` (a small routine returning the AFX_MSGMAP, one of the slots of
its vtable) and the base map routine chain it to the base classes. `class_maps(vtable)` gives the entries
of a class and of its bases, in the order MFC looks them up.

    tools/msgmaps.py              prints the classes with the most handlers
"""
import os, struct, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from command_table import Image, MD
from capstone import x86_const as X

MD.detail = True


class Maps:
    def __init__(self, path):
        im = self.im = Image(path)
        self.b = im.b
        self.text = (im.tva, im.tva + im.text[2])
        self.secs = [(im.base + v, rp, max(vs, rs), n) for n, v, vs, rp, rs in im.secs]
        # entries: runs of 32-byte records ending in a zero record
        self.arrays = {}
        self.find_arrays()
        self.structs = self.find_structs()

    def q(self, va):
        o = self.im.off(va)
        return struct.unpack_from("<Q", self.b, o)[0] if o is not None else None

    def find_arrays(self):
        im = self.im
        for n, v, vs, rp, rs in im.secs:
            if n not in (b".rdata", b".data"):
                continue
            base = im.base + v
            i = rp
            end = rp + rs - 32
            while i < end:
                # a terminator record: 4 ints zero, nSig 0 and pfn 0
                rec = struct.unpack_from("<IIIIQQ", self.b, i)
                i += 8
        # a simpler route: every record whose function is in the code and whose nSig is small
        for n, v, vs, rp, rs in im.secs:
            if n not in (b".rdata", b".data"):
                continue
            base = im.base + v
            off = rp
            while off + 32 <= rp + rs:
                msg, code, ident, last, sig, pfn = struct.unpack_from("<IIIIQQ", self.b, off)
                if self.text[0] <= pfn < self.text[1] and (sig < 0x80 or self.im.base <= sig < self.im.base + 0x800000) and msg < 0x10000:
                    # walk back to the start of the array
                    start = off
                    while start - 32 >= rp:
                        m2, c2, i2, l2, s2, p2 = struct.unpack_from("<IIIIQQ", self.b, start - 32)
                        if self.text[0] <= p2 < self.text[1] and (s2 < 0x80 or self.im.base <= s2 < self.im.base + 0x800000) and m2 < 0x10000:
                            start -= 32
                        else:
                            break
                    # forward to the first record that is not one
                    stop = start
                    while stop + 32 <= rp + rs:
                        m2, c2, i2, l2, s2, p2 = struct.unpack_from("<IIIIQQ", self.b, stop)
                        if self.text[0] <= p2 < self.text[1] and (s2 < 0x80 or self.im.base <= s2 < self.im.base + 0x800000) and m2 < 0x10000:
                            stop += 32
                        else:
                            break
                    ents = []
                    for o in range(start, stop, 32):
                        m2, c2, i2, l2, s2, p2 = struct.unpack_from("<IIIIQQ", self.b, o)
                        ents.append((m2, c2, i2, l2, p2))
                    self.arrays[base + start - rp] = ents
                    off = stop
                else:
                    off += 8

    def find_structs(self):
        """AFX_MSGMAP records: {pfnGetBaseMap, lpEntries} -> {entries address: struct address}."""
        im = self.im
        out = {}
        for n, v, vs, rp, rs in im.secs:
            if n not in (b".rdata", b".data"):
                continue
            base = im.base + v
            for off in range(rp, rp + rs - 16, 8):
                a, b_ = struct.unpack_from("<QQ", self.b, off)
                if b_ in self.arrays and (a == 0 or self.text[0] <= a < self.text[1] or True):
                    out[b_] = (base + off - rp, a)
        return out

    def returns_lea(self, fn):
        """If the routine at `fn` is `lea rax,[X]; ret`, X."""
        o = self.im.off(fn)
        if o is None:
            return None
        ins = list(MD.disasm(self.b[o:o + 16], fn))
        if len(ins) >= 2 and ins[0].mnemonic == "lea" and ins[1].mnemonic == "ret" \
                and ins[0].operands[1].mem.base == X.X86_REG_RIP:
            return ins[0].address + ins[0].size + ins[0].operands[1].mem.disp
        return None

    def struct_to_entries(self):
        return {s: e for e, (s, _) in self.structs.items()}

    def class_maps(self, vtable):
        """The entry arrays, own class first, for the class whose vtable is at `vtable`."""
        s2e = self.struct_to_entries()
        # the GetMessageMap slot: the first slot of the vtable whose routine returns an AFX_MSGMAP
        slot_fn = None
        for k in range(0, 40):
            fn = self.q(vtable + 8 * k)
            if fn is None or not (self.text[0] <= fn < self.text[1]):
                continue
            r = self.returns_lea(fn)
            if r in s2e or (r is not None and self.q(r + 8) in self.arrays):
                slot_fn = fn
                break
        out = []
        cur = self.returns_lea(slot_fn) if slot_fn else None
        seen = set()
        while cur and cur not in seen:
            seen.add(cur)
            ents = self.q(cur + 8)
            if ents in self.arrays:
                out.append(ents)
            base_fn = self.q(cur)
            cur = self.returns_lea(base_fn) if base_fn and self.text[0] <= base_fn < self.text[1] else None
        return out


if __name__ == "__main__":
    mp = Maps(sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "vmbin", "SONVIEW.exe"))
    print(len(mp.arrays), "arrays,", len(mp.structs), "map records")
    for name, vt in (("CTimeWindow", 0x1402c56c8), ("CMainWindow", 0x1402a56a8), ("CTextView", 0x1402c7fa8), ("CResultView", 0x1402b45b8)):
        arrs = mp.class_maps(vt)
        print(name, [(hex(a), len(mp.arrays[a])) for a in arrs])
