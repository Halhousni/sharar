#!/usr/bin/env python3
"""Dump RT_DIALOG templates from a Windows executable as a control list.

The template is a header followed by item records, all DWORD aligned, with
variable-length strings encoded as either one UTF-16 NUL-terminated run or
0x0000 plus an ordinal. `wSignature == 0xffff` marks the extended (DLGITEMTEMPLATEEX)
form, whose header and items carry extra fields.

    ./dialog_dump.py SONVIEW.exe              # list dialogs with titles
    ./dialog_dump.py SONVIEW.exe 132          # dump one
"""
import struct
import subprocess
import sys

# Ordinals in the windowClass field (0xffff followed by the ordinal).
CLASSES = {
    0x80: "Button",
    0x81: "Edit",
    0x82: "Static",
    0x83: "ListBox",
    0x84: "ScrollBar",
    0x85: "ComboBox",
}

DS_SETFONT = 0x40
WS_BORDER = 0x00800000
WS_VISIBLE = 0x10000000
WS_TABSTOP = 0x00010000


def extract(exe, name):
    return subprocess.run(
        ["wrestool", "-x", "--raw", "--type=5", f"--name={name}", exe],
        capture_output=True,
    ).stdout


def align(i):
    return (i + 3) & ~3


def read_sz(buf, i):
    """A NUL-terminated UTF-16 string, or 0x0000 + ordinal."""
    if i + 1 >= len(buf):
        return "", i + 2
    (w,) = struct.unpack_from("<H", buf, i)
    if w == 0:
        return "", i + 2
    start = i
    while i + 1 < len(buf):
        if buf[i] == 0 and buf[i + 1] == 0:
            break
        i += 2
    return buf[start:i].decode("utf-16-le", "replace"), i + 2


def read_cls(buf, i):
    if i + 1 < len(buf):
        (w,) = struct.unpack_from("<H", buf, i)
        if w == 0xFFFF:
            (ordinal,) = struct.unpack_from("<H", buf, i + 2)
            return CLASSES.get(ordinal, f"class#{ordinal:#x}"), i + 4
    return read_sz(buf, i)


def parse(buf):
    dlg_ver, sig = struct.unpack_from("<HH", buf, 0)
    extended = sig == 0xFFFF
    if extended:
        # helpID, exStyle and style are DWORDs but cDlgItems is a WORD.
        (help_id, ex_style, style) = struct.unpack_from("<III", buf, 4)
        (n_items,) = struct.unpack_from("<H", buf, 16)
        x, y, cx, cy = struct.unpack_from("<hhhh", buf, 18)
        i = 26
    else:
        (style, ex_style, n_items) = struct.unpack_from("<IIH", buf, 0)
        x, y, cx, cy = struct.unpack_from("<hhhh", buf, 12)
        i = 20
    _, i = read_sz(buf, i)          # menu
    _, i = read_cls(buf, i)         # window class
    title, i = read_sz(buf, i)
    font = None
    if style & DS_SETFONT:
        if extended:
            point, weight, italic, charset = struct.unpack_from("<HHBB", buf, i)
            i += 6
        else:
            (point,) = struct.unpack_from("<H", buf, i)
            i += 2
        face, i = read_sz(buf, i)
        font = f"{face} {point}pt"
    i = align(i)

    items = []
    for _ in range(n_items):
        if extended:
            # helpID first, then exStyle and style.
            (_h, iex, istyle, ix, iy, icx, icy, iid) = struct.unpack_from("<IIIhhhhI", buf, i)
            i += 24
        else:
            (istyle, iex, ix, iy, icx, icy, iid) = struct.unpack_from("<IIhhhhH", buf, i)
            i += 18
        cls, i = read_cls(buf, i)
        text, i = read_sz(buf, i)
        (extra,) = struct.unpack_from("<H", buf, i)
        i += 2 + extra
        i = align(i)
        items.append((cls, iid, text, istyle, ix, iy, icx, icy))
    return dict(title=title, size=(cx, cy), font=font, items=items, extended=extended)


def flags(style, cls):
    f = []
    if style & WS_VISIBLE:
        f.append("visible")
    if style & WS_TABSTOP:
        f.append("tab")
    if cls == "Button":
        kind = {0: "push", 1: "default", 2: "checkbox", 3: "auto-check", 4: "radio",
                5: "auto-radio", 7: "group", 0x0B: "owner-draw"}.get(style & 0xF, f"btn{style & 0xF}")
        f.append(kind)
    if cls == "Static" and (style & 0x1F) in (4, 5, 6, 7, 8, 9, 0x0A):
        # frames and separators, not labels
        f.append("frame")
    if cls == "ComboBox" and style & 3 == 2:
        f.append("dropdown")
    return ",".join(f)


def dump(exe, name):
    raw = extract(exe, name)
    if not raw:
        print(f"# no dialog {name}")
        return 1
    d = parse(raw)
    print(f"# dialog {name}  \"{d['title']}\"  {d['size'][0]}x{d['size'][1]} units"
          f"  font {d['font']}  items {len(d['items'])}")
    for cls, iid, text, style, x, y, cx, cy in d["items"]:
        label = text.replace("\r", "\\r").replace("\n", "\\n")
        print(f"  {cls:<10} #{iid:<6} {x:>5},{y:<5} {cx:>5}x{cy:<5} "
              f"[{flags(style, cls)}] {label}")
    return 0


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else "SONVIEW.exe"
    if len(sys.argv) > 2:
        return dump(exe, sys.argv[2])
    listing = subprocess.run(["wrestool", "-l", exe], capture_output=True, text=True).stdout
    for line in listing.splitlines():
        if "--type=5 " in line:
            name = line.split("--name=")[1].split()[0].strip("'")
            d = parse(extract(exe, name))
            if d["items"]:
                print(f"{name:>5}  {len(d['items']):>3} items  \"{d['title']}\"")
    return 0


if __name__ == "__main__":
    sys.exit(main())
