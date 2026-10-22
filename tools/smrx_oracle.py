#!/usr/bin/env python3
"""Check Sharar's .smrx reader against CED's own sonpy library.

    pip install sonpy numpy        (sonpy has wheels for recent CPython only)
    cargo build -p smr --examples
    python3 tools/smrx_oracle.py FILE.smrx [more.smrx ...]

For every channel it compares kind, title, units, scale, offset, sample interval, item
count and, for waveforms, every sample; for event channels, every event time.
"""
import subprocess, sys, os
import numpy as np
try:
    from sonpy import lib
except ImportError:  # newer wheels expose the API as sonpy.sonpy
    import sonpy.sonpy as lib

DUMP = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "examples", "dump")
KINDS = {"Adc": lib.DataType.Adc, "EventFall": lib.DataType.EventFall,
         "EventRise": lib.DataType.EventRise, "EventBoth": lib.DataType.EventBoth,
         "Marker": lib.DataType.Marker, "AdcMark": lib.DataType.AdcMark,
         "RealMark": lib.DataType.RealMark, "TextMark": lib.DataType.TextMark,
         "RealWave": lib.DataType.RealWave}


def dump(*args):
    """Output of the dump example, minus its one-line file summary."""
    out = subprocess.run([DUMP, *map(str, args)], check=True, capture_output=True,
                         text=True).stdout.splitlines()
    return out[1:]


def check(path):
    f = lib.SonFile(path, True)
    assert f.GetOpenError() == 0, f"sonpy could not open {path}"
    tb = f.GetTimeBase()
    rows = [r.split("\t") for r in dump(path)]
    bad = 0
    live = [i for i in range(f.MaxChannels()) if f.ChannelType(i) != lib.DataType.Off]
    if [int(r[1]) for r in rows] != live:
        print("  channel list differs", [int(r[1]) for r in rows], live)
        return 1
    for pos, r in enumerate(rows):
        c, kind, title, units = int(r[1]), r[2], r[3], r[4]
        items, ival, scale, off = int(r[5]), float(r[6]), float(r[7]), float(r[8])
        errs = []
        if f.ChannelType(c) != KINDS[kind]:
            errs.append(f"kind {kind} vs {f.ChannelType(c)}")
        if f.GetChannelTitle(c) != title:
            errs.append(f"title {title!r} vs {f.GetChannelTitle(c)!r}")
        if f.GetChannelUnits(c) != units:
            errs.append(f"units {units!r} vs {f.GetChannelUnits(c)!r}")
        if kind == "Adc":
            if abs(f.GetChannelScale(c) - scale) > 1e-6 * max(1, abs(scale)):
                errs.append("scale")
            if abs(f.GetChannelOffset(c) - off) > 1e-6:
                errs.append("offset")
            if abs(f.ChannelDivide(c) * tb - ival) > 1e-12:
                errs.append("interval")
            ref = f.ReadFloats(c, items + 10, 0)
            got = np.array([float(x) for x in dump(path, pos, 0, items)], dtype=np.float32)
            if len(ref) != items or len(got) != items:
                errs.append(f"count {items} / ref {len(ref)} / got {len(got)}")
            elif not np.allclose(ref, got, rtol=1e-6, atol=1e-7):
                errs.append(f"samples differ, max {np.abs(ref - got).max()}")
        elif kind.startswith("Event"):
            ref = f.ReadEvents(c, items + 10, 0)
            got = [float(l.split("\t")[0]) for l in dump(path, pos)]
            if len(ref) != len(got) or any(abs(a * tb - b) > 1e-9 for a, b in zip(ref, got)):
                errs.append(f"events {len(got)} vs {len(ref)}")
        print(f"  ch{c:<3} {kind:<9} {title:<12} {items:>9}  {'OK' if not errs else 'FAIL ' + '; '.join(errs)}")
        bad += bool(errs)
    return bad


total = 0
for p in sys.argv[1:]:
    print(p)
    total += check(p)
print("FAILED" if total else "all channels match sonpy")
sys.exit(1 if total else 0)
