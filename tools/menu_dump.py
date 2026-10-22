#!/usr/bin/env python3
"""Dump Spike2's menu resources as an indented tree.

RT_MENU (type 4) templates are a small tagged format: a 4-byte header, then
items of [option word][id word][UTF-16 string], with MF_POPUP items followed by
their children and MF_END closing a level. `wVersion == 1` marks the extended
(MENUEX) form, whose items carry four words instead of two.

    ./menu_dump.py SONVIEW.exe            # list the menu resources
    ./menu_dump.py SONVIEW.exe 129        # dump one
"""
import struct
import subprocess
import sys

MF_POPUP = 0x0010
MF_END = 0x0080
MFS_GRAYED = 0x0003
MF_CHECKED = 0x0008
MF_SEPARATOR = 0x0800


def extract(exe, name):
    # --raw: wrestool refuses to re-encode a menu it cannot write back.
    out = subprocess.run(
        ["wrestool", "-x", "--raw", "--type=4", f"--name={name}", exe],
        capture_output=True,
    )
    return out.stdout


def read_wstr(buf, i):
    start = i
    while i + 1 < len(buf):
        if buf[i] == 0 and buf[i + 1] == 0:
            break
        i += 2
    return buf[start:i].decode("utf-16-le", "replace"), i + 2


def parse(buf, extended):
    ver, hdr = struct.unpack_from("<HH", buf, 0)
    if ver == 1 and not extended:
        extended = True
    i = hdr if hdr else 4
    lines = []
    depth = 0
    while i < len(buf):
        if extended:
            opt, state, mid, res = struct.unpack_from("<HHHH", buf, i)
            i += 8
        else:
            # The id word is present only when the item is not a popup; a popup
            # carries its option word and then goes straight to the string.
            (opt,) = struct.unpack_from("<H", buf, i)
            i += 2
            if opt & MF_POPUP:
                mid = 0
            else:
                (mid,) = struct.unpack_from("<H", buf, i)
                i += 2
        text, i = read_wstr(buf, i)
        flags = []
        if opt & MF_SEPARATOR:
            flags.append("separator")
        if opt & MF_CHECKED:
            flags.append("checked")
        if opt & MFS_GRAYED == MFS_GRAYED:
            flags.append("greyed")
        accel = ""
        if "\t" in text:
            text, accel = text.split("\t", 1)
            accel = f"   [{accel}]"
        label = text or "<ownerdraw>"
        note = f"  ({', '.join(flags)})" if flags else ""
        idnote = "" if opt & MF_POPUP else f"  #{mid}"
        lines.append(f"{'  ' * depth}{label}{idnote}{accel}{note}")
        if opt & MF_POPUP:
            depth += 1
        if opt & MF_END:
            depth = max(0, depth - 1)
    return lines


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else "SONVIEW.exe"
    if len(sys.argv) > 2:
        name = sys.argv[2]
        raw = extract(exe, name)
        ver = struct.unpack_from("<H", raw, 0)[0] if len(raw) >= 2 else 0
        print(f"# menu {name}, version {ver}, {len(raw)} bytes")
        print("\n".join(parse(raw, ver == 1)))
        return 0
    listing = subprocess.run(["wrestool", "-l", exe], capture_output=True, text=True).stdout
    for line in listing.splitlines():
        if "--type=4 " in line:
            print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main())
