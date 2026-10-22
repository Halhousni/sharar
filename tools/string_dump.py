#!/usr/bin/env python3
"""Extract the RT_STRING tables from a Windows PE.

A string table resource holds 16 strings; string ID `n` lives in block
`n // 16 + 1` at index `n % 16`. Each entry is a WORD length followed by that
many UTF-16 code units, and a zero length is a hole, not a terminator, so the
blocks have to be walked rather than split.

    tools/string_dump.py SONVIEW.exe            # every string, with its id
    tools/string_dump.py SONVIEW.exe pass       # only those matching a regex
"""
import re
import struct
import subprocess
import sys


def blocks(exe):
    listing = subprocess.run(
        ["wrestool", "-l", exe], capture_output=True, text=True
    ).stdout
    out = []
    for line in listing.splitlines():
        if "--type=6 " not in line:
            continue
        name = line.split("--name=")[1].split()[0].strip("'")
        raw = subprocess.run(
            ["wrestool", "-x", "--raw", "--type=6", f"--name={name}", exe],
            capture_output=True,
        ).stdout
        out.append((int(name, 0), raw))
    return sorted(out)


def strings(block_id, raw):
    i = 0
    for k in range(16):
        if i + 2 > len(raw):
            break
        (n,) = struct.unpack_from("<H", raw, i)
        i += 2
        if n == 0:
            continue
        text = raw[i : i + n * 2].decode("utf-16-le", "replace")
        i += n * 2
        yield (block_id - 1) * 16 + k, text


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    exe = sys.argv[1]
    pattern = re.compile(sys.argv[2], re.I) if len(sys.argv) > 2 else None
    n = 0
    for block_id, raw in blocks(exe):
        for sid, text in strings(block_id, raw):
            if pattern and not pattern.search(text):
                continue
            print(f"{sid:5d}  {text}")
            n += 1
    print(f"-- {n} strings", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
