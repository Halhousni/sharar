# The Spike2 user interface, as recovered from the executable

Sharar's menus are thin next to Spike2's. This note records the real inventory
so the gap can be closed deliberately rather than by guessing, and says how to
regenerate it.

## Where the inventory comes from

No screenshots and no decompilation: menus live in the executable as `RT_MENU`
resources, and Spike2's are intact in `SONVIEW.exe`. `tools/menu_dump.py`
extracts them with `wrestool` and walks the template.

```
tools/menu_dump.py SONVIEW.exe            # list the menu resources
tools/menu_dump.py SONVIEW.exe 129        # dump one
```

Two format details cost time and are worth writing down. `wrestool` refuses to
re-encode a menu, so the extract needs `--raw`. And the id word is present only
when an item is *not* a popup — a popup carries its option word and then goes
straight to the string, so reading two words unconditionally shifts every
submenu by one character.

Menu 129 is the data-window frame. 130 to 138 are the same nine menus for the
other document types (script window, result view, and so on); 128 is a stub.

## The nine menus

| menu | items |
|---|---|
| File | 28 |
| Edit | 8 |
| View | 31 |
| Analysis | 33 |
| Cursor | 45 |
| Sample | 24 |
| Script | 8 |
| Window | 11 |
| Help | — |

188 items in the data window alone. Sharar has nine menus with 18 items
between them, so the menu *bar* matches and the *contents* do not.

## What the data window's menus contain

The parts that matter for a viewer, in Spike2's order:

- **File** — New, Open, Import (with a Resource Files submenu), Close, Save,
  Export As, Revert to Saved, Send Mail, Load/Save Configuration, the page setup
  and three print items, Recent File, Exit.
- **Edit** — Undo, Copy, Copy as Text, Copy for Spreadsheet, Delete,
  Edit Preferences.
- **View** — Toolbar, Status Bar, Enlarge/Reduce View, X Axis Range…, Y Axis
  Range…, Standard Display, Channel Draw Mode…, Show/Hide Channel…, a
  Trigger/Overdraw submenu, Vertical Marker…, Channel Pen Width…, **Channel
  Information…**, Channel Image…, File Information…, Multimedia files, Spike
  Monitor, Font…, Use Black and White, Change Colours…, Sonogram Colours…,
  ReRun….
- **Analysis** — Process…, Process Settings…, New Result View, Measurements,
  Fit Data…, a Memory Buffer submenu (six items), Virtual Channels (two),
  Duplicate/Save/Delete Channel, Calibrate…, Channel process…, Marker Filter…,
  Set Marker codes…, New/Edit WaveMark, New N-Trode, then a **Digital Filters**
  group holding FIR Filters… and IIR Filters….
- **Cursor** — New Cursor, Delete/Fetch/Move to/Position submenus that fill with
  the open cursors, Display All, Label Mode (Position, Number, Both, None, Set
  Label…), Renumber, then the whole set again for horizontal cursors, then
  Active Cursor Modes…, Search Right/Left, Display Y Values, Cursor Regions.
- **Sample** — the acquisition configuration. 24 items; nothing here can be
  built before the 1401 backend (see `1401.md`).
- **Script** — Load and Run, Compile, the script window's own commands.
- **Window** — Duplicate, Hide, Show (a list), Window Title…, Cascade, Tile
  Horizontally, Tile Vertically, Arrange Icons, Close All.
- **Help** — Index, Using Help, Tip of the Day, View Web site, Getting Started,
  About.

Two things stand out from the structure rather than the individual items. The
Cursor menu is the largest in the program: cursors are a first-class object in
Spike2, with label modes, regions, horizontal variants and search, and Sharar
has two fixed cursors. And Analysis separates *process* (a named pipeline you
configure once and apply) from the individual filter dialogs, which is why
"Process…" and "Channel process…" are distinct entries.

## What Sharar has now

File (Open, Open by path, Close, Export CSV, Quit), Edit (three selection
helpers), View (fit, zoom in/out, Channel Information, X Axis Range, Y Axis
Range, Show/Hide Channel), Analysis
(digital filters, average), Cursor (Display All, Label Mode, Display Y Values,
Move to, Search Left/Right), Window (fit), Help (keyboard help). Sample and
Script are empty.

Two notes on where things sit. Cursor placement used to appear in both the View
and Cursor menus; it now belongs to Cursor only, which is where Spike2 keeps it.
And X Axis Range honours left, right, width and an explicit large-tick spacing,
with subdivisions as minor ticks — its logarithmic and auto-units controls are
drawn disabled rather than faked, because the axis has no log or unit model.

## The next steps, in order of what they buy

1. **View > Channel Information…** — done.
2. **The Cursor menu** — done: label modes, display all, move to, search
   left/right over the selected channel's events, and Y values on the tag.
3. **View > X Axis Range…, Y Axis Range…, Show/Hide Channel…** — done. The Y
   range needed a per-channel displayed range the drawing did not have; it now
   carries one, and an explicit range replaces the automatic fit and the `gain`
   together, which is what Spike2's dialog does. With Automatic ticked the
   dialog shows the range the channel is actually drawn over, so unticking it
   starts from what is on screen rather than from a default.
4. **Analysis > Process…** — the named pipeline. This is a design change, not a
   dialog: it means storing a list of processes per channel instead of the
   single `Option<FilterSpec>` the app carries today.
5. **Sample** and **Script** — blocked on the 1401 backend and on an `.s2s`
   interpreter respectively; neither is a menu problem.

Dialog templates are in `dialogs.md`: 175 of them, with the FIR dialog's 55
controls laid out and the parameterisation difference it settles. String tables
are the remaining inventory piece.
