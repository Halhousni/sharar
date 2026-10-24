#!/usr/bin/env python3
"""List a DLL's exports: `tools/pe_exports.py file.dll [substring]` prints `address name`."""
import struct, sys

b = open(sys.argv[1], "rb").read()
pe = struct.unpack_from("<I", b, 0x3c)[0]
ns, = struct.unpack_from("<H", b, pe + 6)
osz, = struct.unpack_from("<H", b, pe + 20)
base, = struct.unpack_from("<Q", b, pe + 48)
secs, o = [], pe + 24 + osz
for _ in range(ns):
    vs, va, rs, rp = struct.unpack_from("<IIII", b, o + 8)
    secs.append((va, max(vs, rs), rp)); o += 40

def off(rva):
    for va, sz, rp in secs:
        if va <= rva < va + sz:
            return rp + rva - va

exp_rva, = struct.unpack_from("<I", b, pe + 24 + 112)
e = off(exp_rva)
nfun, nnames, afun, anames, aords = struct.unpack_from("<IIIII", b, e + 20)
funs = struct.unpack_from("<%dI" % nfun, b, off(afun))
names = struct.unpack_from("<%dI" % nnames, b, off(anames))
ords = struct.unpack_from("<%dH" % nnames, b, off(aords))
want = sys.argv[2] if len(sys.argv) > 2 else ""
for n, ordv in zip(names, ords):
    o2 = off(n)
    name = b[o2:b.index(b"\0", o2)].decode()
    if want in name:
        print("%x %s" % (base + funs[ordv], name))
