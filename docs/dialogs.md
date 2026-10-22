# Spike2's dialog templates

The menus are in `gui.md`; this is the other half of the interface inventory.
Dialogs are `RT_DIALOG` resources and `SONVIEW.exe` carries **175** of them,
each with its exact control layout in dialog units. `tools/dialog_dump.py`
extracts them.

```
tools/dialog_dump.py SONVIEW.exe           # the inventory with titles
tools/dialog_dump.py SONVIEW.exe 129       # one dialog, control by control
```

Two format details, both of which cost a pass:

- the extended template marks itself with a `0xffff` signature, and its item
  records lead with *helpID* — not the style word — so a parser that keeps
  reading `style, exStyle, id` from the classic form reports nonsense but does
  not fail until the buffer runs out;
- `cDlgItems` is a WORD while the three fields before it are DWORDs, which
  misplaces everything after it if it is read as a DWORD.

## The inventory

The dialogs that matter for a viewer, by resource id:

| id | title | items |
|---|---|---|
| 129 | FIR Digital filtering | 55 |
| 133 | IIR Digital filtering | 41 |
| 132 | Save filter into bank | 5 |
| 1800 | X Axis Range | 23 |
| 1100 | Settings for channel from | 49 |
| 400 | Settings for Average from | 18 |
| 800, 900 | Process %s from %s | 24, 33 |
| 200 | Settings for PSTH from | 26 |
| 300 | Settings for Phase from | 26 |
| 500 | Settings for Power from | 11 |
| 600, 700 | Settings for Correlation from | 14, 26 |
| 1000 | Settings for XY plot from | 60 |
| 100 | Settings for INTH from | 13 |
| 1500 | About Spike2 | 19 |
| 1536 | Choose a Directory | 14 |

The analysis dialogs are the interesting ones: the `%s` in the Process titles is
the channel and the source, so one template serves every process.

## The FIR dialog, control by control

Dialog 129 is 443 by 295 units and 55 controls, and it settles a question the
script path left ambiguous:

```
Channel [combo]   Filter [combo]   Comment [edit]
[response plot, 380x116]          [band display, 259x136]
LEDs [ ] Auto   Length [slider][spin]
[ ] Gain in dB   [ ] Log frequency
[ ] Continuous update during drag
[ ] Free parameters (move next band)
Type [combo]
              Edge    Frequency   -3dB point
 Band 2 high  [edit][spin]  [value]
 Band 2 high  [edit][spin]  [value]      (five rows)
 Band 2 high  [edit][spin]  [value]
 Band 2 high  [edit][spin]  [value]
 Transition gap [edit][spin] [Set]
[ ] Show axes   [ ] Same scale
[Apply] [Close] [Help] [DETAILS_X] [Load...] [Save...]
```

So the dialog is a **graphical band editor** over the response plot: up to five
band edges, each with a frequency and its own -3 dB point, a transition gap, and
a length that can be automatic. That is not the parameterisation `FIRQuick`
takes. The script command takes *attenuation in dB plus a transition width* and
derives the tap count; the dialog hands `CFilterObject` a band table with
explicit corner points and lets the plot drive it. Both end up in the same
`CFIR::fircalc`. Sharar's filter dialog follows the script path, which is why it
asks for attenuation and width and shows the coefficient count.

Sharar draws the response — `CFIR::FreqR` is ported (0x18c0) as
`Design::freq_r`, checked against a plain DTFT of the full coefficient array in
`freq_r_matches_a_direct_dtft`, and the filter dialog plots it from 0 to Nyquist
over a +10 to -80 dB window with a 0 dB reference line.

The dialog also edits a **band table**: rows of low, high, desired amplitude and
weight, which are the four fields `CFIR` reads and the same table `CFilterObject`
builds internally. `smr::design` takes that table straight to the exchange —
the same entry point the Wine harness calls — so a hand-built table designs with
the original's numbers. Verified on a three-band notch:

```
103 0 3 16 1  0.0 0.20 1.0 1.0  0.22 0.30 0.0 10.0  0.32 0.5 1.0 1.0
```

CED and the port both return deviation 0.023553798689392, coefficients agreeing
to 1e-14, and the design is in `tools/fir_oracle.py`'s sweep. A five-band table
with weight 20 does **not** converge — in CED it returns deviation 1 with NaN
coefficients and in the port the same, which is the exchange refusing an
infeasible problem rather than a porting fault. The dialog reports that over the
plot rather than drawing an empty box.

### The dialog's edges are gains, and the weight is derived

`Filters.dll` exports the whole dialog, not just the designer: `CDlgFilterObject`,
`CChangeFilter`, `CChangeGap`, `CMoveGap`, `CFilterBank`, `CResFile`. Two of its
functions settle the question the band table left open.

`CDlgFilterObject::SetBand` (0x9ba0) copies the dialog's band record into a
`CFiltBand` and, when the record's weight is zero, writes `10 - 9·gain` into the
weight, floored at 1. `CFilterObject::CreateBand` (0xbf20) implements the same
rule at the same field when it is handed a zero weight. So a band edge carries a
**wanted gain**, and the weight is a function of it: gain 1 gives weight 1, a
stop band (gain 0) gives 10, and a -3 dB corner gives 3.6285.

`CDlgFilterObject::CalcShowdB` (0x9650) decides whether the dialog displays that
gain in dB, which it does for every type except 11 and 12 — the two
differentiators, where a level makes no sense. `CondConvertFromdB` (0x96d0) is
`10^(dB/20)`.

Sharar ports the rule as `smr::edge_weight` and builds bands with
`Band::from_gain`. The dialog now edits a gain per edge and shows both the gain
in dB and the derived weight, so nothing is hidden. The notch table verified
against CED earlier was written with gains 1 and 0 and weights 1 and 10, which is
exactly what the rule produces — the hand-written table and the dialog's own
route agree.

### Applying a band table

Apply now carries a band table to the channel: the finished coefficients go into
a per-channel slot and are applied with `smr::arr_fir`, the port of CED's own
convolution, at both places a channel is filtered (the envelope the plot draws and
the statistics the Measurements panel shows). A `FilterSpec` and a coefficient set
are mutually exclusive, so applying one clears the other — otherwise the data
would be filtered twice, once by a spec resolved at draw time and once by a design.

Checked by applying a stop band over 0 to 0.2 to the demo file, whose first
channel is a 12.5 Hz sine wave: the trace collapses to a flat line and the panel
goes from RMS 1.982 V to 0.0167 V, with the status bar reporting the tap count.
The min and max that remain (-0.669 and 0.269) are the truncated first and last
pixels of the window, which is the same edge effect `dc_remove` and the IIR path
already have.

### The band display

The plot now carries the band boundaries as draggable handles, which is the part
of Spike2's dialog that turns a parameter form into an editor. Dragging an
internal edge moves the boundary between two bands, clamped so the neighbours stay
ordered and non-empty — the same constraint `CChangeFilter` enforces when a corner
is picked up, minus its drag state machine, which has modes for moving a gap,
moving a block and holding one side.

Frequency across the plot is linear unless "Log frequency" is ticked, which is
dialog 129's own switch. The log mapping runs from 1 mHz to Nyquist, using
`CChangeFilter::FreqToPixel`'s 1e-3 floor in normalised frequency. Both mappings
are now `freq_to_frac` and `frac_to_freq` in the app, with tests: they invert each
other across the band, Nyquist lands on the right edge in both, the log one is
monotonic, and it clamps at its floor instead of running to negative infinity.

Both gestures are in now: an edge handle moves a boundary, and a square handle on
each band's plateau moves that band's level. Spike2 keeps them apart the same way
— `CChangeGap` moves a gap, the basic moves change a level — and drawing a plateau
per band is what makes the plot read as an editable envelope rather than as a
curve with markers on it.

## X Axis Range, as an example of the scale of a dialog

Twenty-three controls: units, left, right as editable combos (values *or*
times), width, large tick spacing and subdivisions with their own enable boxes,
logarithmic, auto units, show powers, plus Show All, Draw, Cancel, Close, Help.
Sharar's view has a zoom and a fit. If the axis dialogs are worth matching, this
is the shape to match; if not, the keyboard and wheel behaviour already covers
the common case.

## What is not extracted yet

String tables (`RT_STRING`) hold the combo contents — the filter type names, the
units, the process names — and would let the dialogs be rebuilt with the right
entries rather than placeholder lists. They are the next artifact if the dialogs
are to be built properly.
