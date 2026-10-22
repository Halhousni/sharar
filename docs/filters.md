# The filter designer, as Spike2 implements it

Sharar's FIR filters used to be a Hamming-windowed sinc: they behaved like a
low pass, but they were not Spike2's filters. They are now a port of the code
Spike2 actually runs. This note records what was recovered, where from, and how
it was checked, so the next reader does not have to repeat the analysis.

## Where the design lives

Spike2's script command `FIRQuick` is implemented in `FIRWin.s2l`, a DLL that
`script.tip` documents as "creates a set of filter coefficients in the same way
that FIRMake() does". `FIRWin.s2l` does no filtering maths itself: `CFIRQuickCmd`
assembles a band table and calls into `Filters.dll`, where `CFIR` runs the
exchange.

The two halves, with the addresses the decompiler reported:

| What | Where |
|---|---|
| `CFIR::fircalc` — validate the bands, try the grid sizes | `Filters.dll` 0x3960 |
| `CFIR::docalc` — build the grid, then run the exchange | `Filters.dll` 0x3520 |
| `CFIR::Equivalent` — warp the response for odd/even symmetry | `Filters.dll` 0x3390 |
| `CFIR::Remez` — the exchange itself | `Filters.dll` 0x2930 |
| `CFIR::invfft` — response samples back to an impulse | `Filters.dll` 0x1e50 |
| `CFIR::Impulse` — fold that into the symmetric half | `Filters.dll` 0x2640 |
| `CFIR::GetCoeff` — mirror the half to the full length | `Filters.dll` 0x3a90 |
| `CFIR::geeNoErr`, `eff`, `wate`, `D` | `Filters.dll` 0x1d30, 0x1c90, 0x1cf0, 0x1c20 |
| `CFilterObject::GetIdealCoeffNum` — the tap count | `Filters.dll` 0xdc70 |
| `FIRQuick` band table and defaults | `FIRWin.s2l` 0x180003a40, 0x180003d30, 0x180004060 |
| `ArrFir` — apply the coefficients | `compiler.dll` 0x18001f470 |

Everything here was read out of the binaries with REA driving Ghidra, with the
disassembly used wherever the decompiled C was ambiguous. Screenshots of the
running program establish layout and behaviour, not code, and none of the
numbers below come from one.

## The design, in order

1. **Bands.** `FIRQuick` builds a `CBand` table: each band has a lower and upper
   edge as a fraction of the sampling rate, a desired amplitude and a weight.
   A low pass is two bands, `[0, f-w/2]` at amplitude 1 and `[f+w/2, 0.5]` at 0,
   with the stop band weighted 10 times the pass band. A high pass is the
   mirror image. A band pass and a band stop are three bands with a further
   0.02 gap either side of the transition, where the exchange is unconstrained.
   A differentiator is `[0, f]` at amplitude 2π and `[f+w, 0.5]` at 0.
2. **Tap count.** Derived, not asked for. For a low or high pass,

   ```
   n = (atten - 23.9*width - 5.585) / (14.41*width + 0.0723)
   ```

   truncated, plus one, clamped to 2047, then rounded up to odd. A band filter
   uses a width-independent line instead, `1 + 2.7739*(atten - 6.063)`. Above
   0.48 a differentiator takes a fixed 20 coefficients. Constants 23.9, 14.41,
   5.585, 0.0723, 0.478 and 2.773925104022191 are the ones in the binary.
3. **Grid.** `docalc` builds a uniform grid of `delf = 0.5/(r * lgrid)` over the
   bands, where `r` is half the coefficient count (`(n+1)/2` when `n` is odd and
   the filter is symmetric). Each band's last point is snapped to the band edge.
   If the grid would exceed 0x4020 points, `lgrid` is decremented and the count
   recomputed. For an even-length symmetric filter the point at 0.5 is dropped,
   which is what keeps `cos(pi*f)` — the warp in step 4 — away from zero.
4. **Warp.** For an even-length symmetric filter the desired response is divided
   by `cos(pi*f)` and the weight multiplied by it; for an odd differentiator the
   same with `sin(2*pi*f)`, and for an even one `sin(pi*f)`. An odd-length
   symmetric filter is left alone.
5. **Initial extrema.** `r+1` points, `floor(k * (ngrid-1)/r)`, with the last
   fixed at `ngrid-1`.
6. **Exchange.** Barycentric interpolation over `cos(2*pi*f)` at the extremal
   frequencies gives the weighted error; the deviation is the ratio of the
   alternating sums. Each extremum then moves to the peak of the error between
   its neighbours — upward first, downward only while nothing has moved yet, and
   with a last-resort scan deeper into the interval when neither finds anything.
   The iteration stops after 25 passes, when nothing moves, or when the
   deviation *decreases*, which sets error bit 0 and asks `fircalc` to retry
   with `lgrid/2+1` and then `2*lgrid-1`.
7. **Ends.** After the sweep the two ends are re-checked against the running
   error plus a 1.00001 margin, and the extremal set is shifted when a peak was
   missed outside its range. This is the part that decides whether a design
   converges to Spike2's answer, and it is why an approximation of the loop that
   looks right on paper is not enough.
8. **Impulse.** The response is sampled at `r` frequencies, matched to the
   bracketing extremal, and taken back through a cosine series; `Impulse` then
   folds it into the symmetric half, and `GetCoeff` mirrors it. A differentiator
   differs in sign on the mirrored half and carries one extra zero.

The filter is applied by `ArrFir` in `compiler.dll`, which clamps the window at
both ends rather than padding with zeros: a constant in gives a constant out,
scaled by the DC gain.

## How it was checked

`tools/fir_oracle.py` runs the same designs through this port and through CED's
own `Filters.dll`, the latter under Wine through a small Windows harness
(`cfir_oracle.c`) that calls `CFIR::fircalc` and `CFIR::GetCoeff` directly.
Nineteen designs — odd and even lengths from 3 to 255 taps, low pass, high pass,
band pass, band stop and differentiator — agree to a worst relative difference
of **1.8e-14**, which is the last two or three bits of the mantissa. The
deviations agree to every printed digit, and the extremal sets are identical.

Five further designs use band tables that stop short of Nyquist, so they take
the warped path. Three of them also agree to machine precision (5.7e-15,
7.7e-13, 2.2e-12); the other two land at 1.2e-9 and 8.6e-8 relative to their
peak coefficient. That gap is not a difference in the algorithm: the corner
recursion multiplies by `2*wb` at every step and the warp's `acos` has an
infinite derivative at the band edges, so a last-bit difference in `cos`
between MSVC's maths and glibc's becomes visible. These designs also produce
large coefficients (peak 66 in one case), which scales the absolute difference
up with them.

The DLL is not redistributable, so the harness and the fixture live outside the
repository. `smr/tests/fir_reference.rs` reads `testdata/fir_reference.txt` when
it is present and prints a skip when it is not, so a fresh clone still passes.

## What this does not cover

- `FIRMake`, which takes explicit band parameters and a grid size, shares
  `docalc` and is available through `smr::design`, but has no script-level
  wrapper here and no oracle case beyond the band tables above.
- `CFIR` types 5, 6 and 7 (Hilbert and the other antisymmetric designs) are not
  ported. `SetFilt` collapses any type below 4 to 0, so the four band layouts
  above are the ones `FIRQuick` can produce.
- A band table that does not span 0 to Nyquist is supported, through the
  warped path in `CFIR::invfft` and the corner recursion that goes with it
  (0x180002212, ported as `corner_fix`). Two details there are easy to get
  wrong and both were confirmed against the oracle: the recursion halves its
  warp at the last step, and it stores the previous half of the impulse
  response *into the amplitude array* rather than a fresh buffer, so later
  terms read the overwritten values. No `FIRQuick` layout reaches this path —
  every one covers the whole spectrum — so it matters only for `FIRMake`-style
  band tables.
- A differentiator above 0.48, or with a band edge that leaves no room for the
  stop band, degenerates in both implementations: CED reports deviation 1.0 and
  NaN coefficients, and this port does the same.
