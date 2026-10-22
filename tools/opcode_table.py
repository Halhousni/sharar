#!/usr/bin/env python3
"""Recover Spike2's script opcode table from compiler.dll.

`CScriptMgr::Disassemble` names each instruction through a lookup at RVA 0x48320,
which indexes a pointer array of narrow strings. Opcodes 0..0x14 are formatted
from `const=%d` instead, and anything above 0x79 is `NotPerm`, so the named range
is 0x15..0x79 with the array indexed by `opcode - 0x14`.

    tools/opcode_table.py compiler.dll

The image base and the three dimensions of the table are the only constants in
here; everything else is read from the file.
"""
import re
import struct
import subprocess
import sys

IMAGE_BASE = 0x180000000
NAMES_TABLE_RVA = 0x8A9E0
FIRST_NAMED = 0x14
LAST_NAMED = 0x79


def sections(path):
    out = subprocess.run(
        ["x86_64-w64-mingw32-objdump", "-h", path], capture_output=True, text=True
    ).stdout
    secs = []
    for line in out.splitlines():
        m = re.match(
            r"\s*\d+\s+(\S+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)",
            line,
        )
        if m:
            vma = int(m.group(3), 16)
            # objdump prints the full VA for a PE; the table is addressed by RVA.
            if vma >= IMAGE_BASE:
                vma -= IMAGE_BASE
            secs.append((m.group(1), int(m.group(2), 16), vma, int(m.group(5), 16)))
    return secs


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    path = sys.argv[1]
    blob = open(path, "rb").read()
    secs = sections(path)

    def rva_to_off(rva):
        for _name, size, vma, foff in secs:
            if vma <= rva < vma + size:
                return foff + (rva - vma)
        return None

    off = rva_to_off(NAMES_TABLE_RVA)
    if off is None:
        print("names table is not mapped", file=sys.stderr)
        return 1

    for opcode in range(FIRST_NAMED, LAST_NAMED + 1):
        ptr = struct.unpack_from("<Q", blob, off + (opcode - 0x14) * 8)[0]
        if not ptr:
            print(f"{opcode:3d} <null>")
            continue
        s = rva_to_off(ptr - IMAGE_BASE)
        if s is None:
            print(f"{opcode:3d} <unmapped>")
            continue
        end = s
        while blob[end] != 0:
            end += 1
        print(f"{opcode:3d} {blob[s:end].decode('latin-1')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
