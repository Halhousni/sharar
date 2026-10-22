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

Sharar now draws the response — `CFIR::FreqR` is ported (0x18c0) as
`Design::freq_r`, checked against a plain DTFT of the full coefficient array in
`freq_r_matches_a_direct_dtft`, and the filter dialog plots it from 0 to Nyquist
over a +10 to -80 dB window with a 0 dB reference line. What is still missing is
the other half: the band display and the five editable edges with their -3 dB
points, which is what turns Spike2's dialog from a plot with parameters under it
into a band editor.

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
