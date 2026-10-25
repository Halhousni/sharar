#!/usr/bin/env python3
"""Find the vtable of a class from its RTTI in an x64 PE: tools/rtti.py SONVIEW.exe CLogView CTextView ..."""
import os, struct, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from command_table import Image


def vtables(im, name):
    pat = (".?AV" + name + "@@").encode() + b"\0"
    out = []
    i = -1
    while True:
        i = im.b.find(pat, i + 1)
        if i < 0:
            break
        td_rva = None
        # the type descriptor starts 16 bytes before the name
        for n, v, vs, rp, rs in im.secs:
            if rp <= i - 16 < rp + rs:
                td_rva = v + (i - 16 - rp)
        if td_rva is None:
            continue
        # a Complete Object Locator holds {sig, offset, cdOffset, typeDescRva, classDescRva, selfRva}
        for n, v, vs, rp, rs in im.secs:
            if n not in (b".rdata", b".data"):
                continue
            for off in range(rp, rp + rs - 24, 4):
                sig, o1, o2, td, cd, me = struct.unpack_from("<IIIIII", im.b, off)
                if sig == 1 and td == td_rva and me == v + (off - rp):
                    col_va = im.base + me
                    # the vtable is the 8 bytes after a qword equal to the locator's address
                    for n2, v2, vs2, rp2, rs2 in im.secs:
                        if n2 not in (b".rdata", b".data"):
                            continue
                        for o in range(rp2, rp2 + rs2 - 8, 8):
                            if struct.unpack_from("<Q", im.b, o)[0] == col_va:
                                out.append((im.base + v2 + (o - rp2) + 8, o1))
    return out


if __name__ == "__main__":
    im = Image(sys.argv[1])
    for name in sys.argv[2:]:
        for va, off in vtables(im, name):
            print(name, hex(va), "offset", off)
