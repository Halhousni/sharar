import sys
import numpy as np
from neo.rawio import Spike2RawIO

path = sys.argv[1]
r = Spike2RawIO(filename=path)
r.parse_header()
h = r.header

print("FILE", path.split("/")[-1])
print("sig_channels", len(h["signal_channels"]))
for c in h["signal_channels"]:
    print("  ADC", repr(str(c["name"])), "rate", round(float(c["sampling_rate"]), 6),
          "units", repr(str(c["units"])), "gain", repr(float(c["gain"])),
          "offset", repr(float(c["offset"])))

print("event_channels", len(h["event_channels"]))
for i, c in enumerate(h["event_channels"]):
    t, d, lab = r.get_event_timestamps(0, 0, i)
    ts = t / 1e5
    print("  EV", repr(str(c["name"])), "n", len(t),
          "first", round(float(ts[0]), 6) if len(ts) else None,
          "last", round(float(ts[-1]), 6) if len(ts) else None,
          "sample_labels", [str(x) for x in lab[:4]])

print("spike_channels", len(h["spike_channels"]))
print("t_start", r.header["t_start"], "duration_ticks", r._seg_t_stops[0])
for i, c in enumerate(h["signal_channels"]):
    n = r.get_signal_size(0, 0, i)
    first = r.get_analogsignal_chunk(0, 0, 0, min(5, n), stream_index=0, channel_indexes=[i]).ravel()
    print("  ADC", repr(str(c["name"])), "n_samples", n, "first5", [round(float(x), 6) for x in first])
