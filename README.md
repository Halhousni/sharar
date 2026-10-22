# Sharar

An application to read CED Spike2 `.smr` files.

Sharar opens a recording from CED's Spike2 (the acquisition program Spike2 ships with the
1401 interface) and displays it: waveform channels, event channels and markers on a shared
time axis, with cursors, per-channel gain and scale bars. It is written in Rust with egui and
reads the files directly rather than through CED's Windows DLLs. It builds on Linux, macOS and
Windows, and has been developed and tested on Linux.

![the EMG example open in Sharar](docs/screenshot.png)

## Build and run

```sh
cargo run -p sharar -- path/to/recording.smr
```

Command-line flags: `--dc` starts with DC remove on, `--filters` opens the filter dialog,
`--lowpass=<hz>` applies a FIR low pass at startup, `--average` opens the average dialog with
a result computed, `--export` writes the visible window to CSV and reports the path, and
`--record` opens the live strip chart on its synthetic source.

Drag the window on a file, or use `File → Open…`. Keyboard: `F` fits the recording to the
window, `+`/`-` change the selected channel's gain, `0` resets it, `D` toggles DC remove on
the selected channel. Mouse: wheel zooms about the pointer, middle-drag pans, dragging near
a cursor moves it.

## What works

- Reads SON-format `.smr` files: file header, up to 32 channel headers, chained data blocks.
- Waveform channels (16-bit ADC and 32-bit float), drawn as a min/max envelope so a
  12,000-sample trace reads correctly in 500 pixels.
- Event and marker channels drawn as tick rows; marker codes shown as characters and text
  marks shown as their label.
- Waveform-mark channels (spike trains with a snippet per event): tick raster plus the mean
  snippet, e.g. 646 spikes of 100 points each.
- Two draggable cursors, with the value at each cursor shown per channel and `Δt` and its
  reciprocal in the status bar.
- Per-channel vertical gain, an automatic amplitude fit, a labelled scale bar, grid and zero
  line.
- Channel list with visibility toggles and selection.
- Measurements between the cursors: sample count, mean, RMS, minimum, maximum and
  peak-to-peak per waveform channel, computed on the processed signal.
- DC remove (a centred boxcar mean subtraction) per channel, off by default: the Analysis
  menu's `DC remove on selected channel` checkbox, or `D`, or `--dc` on the command line.
- Peri-stimulus average: `Analysis → Average around events…` picks an event channel and a
  window, then plots the mean waveform with the trial count.
- CSV export of the selected channel over the visible window: `File → Export channel…`,
  or `--export` on the command line.
- Live strip chart: `--record` opens a scrolling view of the last five seconds, one trace per
  channel, fed by a synthetic source until the 1401 path is wired.
- Digital filters: `Analysis → Digital filters…` applies a low-pass or high-pass to the
  selected channel, either as a Hamming-windowed sinc FIR (odd tap count, centred
  convolution so there is no time shift) or as a two-pole Butterworth IIR section.

  These are approximations, not ports. Spike2 designs FIR filters in `FIRWin.s2l` with
  the Parks-McClellan exchange — `CFIR::fircalc` takes a band table (`CBand`) and an
  attained deviation, and the class exposes `GetExtremal` and `InIteration` — so its
  filters are equiripple against a requested ripple, where these are windowed.

## What is not built yet

This is an early slice of a large program, not a finished replacement for Spike2.

| Area | State |
|---|---|
| Acquisition from a CED 1401 | `--record` opens a live strip chart fed by a synthetic source; the 1401 backend is a stub that reports no device attached |
| Sampling configuration | none |
| Channel processing | DC remove and low/high-pass digital filters; no rectification, integration or other processes |
| Averaging | peri-stimulus average works; no smoothing, confidence bands or baselines |
| Spike sorting | wavemarks load and plot, but there is no sorting or unit separation |
| `.s2s` scripting | none |
| Writing `.smr` | read-only; CSV export works |
| Filter design | low/high pass only; no band pass, stop bands, differentiators or scripts |

## Layout

```
smr/      .smr reader — no dependencies
app/      the egui/eframe application
tools/    inventory.py, the reference reader used to validate smr/
docs/     screenshots
```

```sh
cargo test     # parser tests
```

The tests check the reader against known values from CED's own example recordings: channel
inventories, sample rates, units, event counts and durations. Those recordings ship on the
Spike2 CD and are not redistributed here, so the tests skip when they are absent — extract
`data2.cab` from the CD to `testdata/` to run them fully.

## Checked against Spike2

The layout and menus were checked against Spike2 rather than the manual alone. Its data
window carries a numeric y-axis per channel and a status bar reading `cursor, time, value`.
Its menus are File, Edit, View, Analysis, Cursor, Sample, Script, Window, Help.

`tools/vm.sh` drives a Windows guest over libvirt and the QEMU guest agent to do that. It is
a development aid, not part of the build, and needs your own guest with Spike2 installed, its
libvirt domain in `DOM`, and `LIBVIRT_DEFAULT_URI=qemu:///session`.

## Notes on the format

Recovered by reading the files and cross-checking against `neo.rawio.Spike2RawIO`:

- Channel titles, comments and units are **Pascal strings** (length byte first), not
  NUL-terminated C strings.
- Files reserve 32 channel headers and mark unused ones `kind = 0`; they must be filtered or
  the channel list is mostly empty slots.
- Filters follow the arrangement the CD's manual describes: the FIR type is set by a pass
band edge and a transition gap, and the number of coefficients follows from the narrower of
the bands. This implementation exposes the tap count directly instead of deriving it.

ADC samples convert to units as `count * scale / 65536 + offset`. Note that `neo` applies
  an extra factor of ten (`gain = scale / 6553.6`), which is a neo convention rather than
  the file's.
- `neo` reports one marker where the EMG example's "Memory" channel holds twelve, evenly
  spaced every 6 s. The block payload is authoritative; `smr` reads all twelve.

## Legal

Sharar is an independent reader. It contains no CED code. Spike2 and the 1401 are CED
products; the example recordings used for testing remain CED's and are not included.
