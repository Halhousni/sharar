#!/usr/bin/env python3
"""List the constants a decompiled msvcr110 function refers to, read from the DLL image.

    tools/dump_consts.py vmbin/msvcr110.dll decompiled.c [...]

Prints `name address low-8-bytes (as a double) high-8-bytes` for every DAT_ symbol named."""
import re, struct, sys, os
sys.path.insert(0, os.path.dirname(__file__))
from command_table import Image

im = Image(sys.argv[1])
names = {}
for f in sys.argv[2:]:
    for m in re.finditer(r"\b(_?)DAT_([0-9a-f]{9})\b", open(f).read()):
        names[int(m.group(2), 16)] = 1
for a in sorted(names):
    b = im.b[im.off(a):im.off(a) + 16]
    lo, hi = struct.unpack("<QQ", b)
    print(f"{a:x} {lo:016x} {struct.unpack('<d', b[:8])[0]!r} {hi:016x}")
