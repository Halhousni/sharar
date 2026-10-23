# Spike2's script engine

Sharar's Script menu is a placeholder. This is the first pass at what it would run: the
engine is not in `SONVIEW.exe` at all, it is two libraries beside it.

| file | size | what it is |
|---|---|---|
| `compiler.dll` | 632 KB | the language: lexer, parser, compiler, debugger, interpreter |
| `runtime.s2l` | 676 KB | the command objects the interpreter drives |

Both are PE32+ DLLs. `compiler.dll` exports its classes with full C++ names,
which is what makes this tractable — there is no guesswork about the boundaries.

## The classes, as exported

| class | what it does |
|---|---|
| `CScriptMgr` | the whole session: `CompileAndRun`, `Disassemble`, `EvalGlobal`, `EvalLocal`, `EvalStackArgs`, `CheckUserAbort` |
| `CompileParams` | what a compile needs from its host: source name, a line fetcher, the debugger, error strings, script options |
| `CInterpreter` | the execution state a command object is handed |
| `CRunInfo` | which script and which view a run belongs to |
| `CDebugger` | breakpoints, line lookup, the object map from a script line to a window |
| `CScriptControl` | the program as the script sees it |
| `CBaseCmd`, `TBasicObject`, `TCurrentView`, `TResultArray`, `CArrObj`, `TIndex`, `TArray` | the command and value objects — `TBasicObject::Execute(CInterpreter*)` and `TCurrentView::Execute(CInterpreter*)` are exportable virtuals |
| `CScriptTimer` | `CheckTime`, for the timer-driven side of the language |

Eval is split three ways — `EvalGlobal`, `EvalLocal` and `EvalStackArgs` — and the
local one takes an `InterpreterState`, so the debugger evaluates expressions in a
suspended frame rather than in a separate interpreter.

## The compile pipeline

From `CScriptMgr::CompileAndRun` (0x42c0):

1. `CompileParams::StartTiming`, then a status message (resource 0xdc6).
2. The source is wrapped as something with a **line fetcher** rather than a buffer:
   `CompileParams::FnGetLine` plus `SourceView`. A script is compiled out of the
   editor's own view, so an unsaved script compiles as it stands on screen.
3. A preparation step (0x4790) builds the compiler state from that.
4. `CompileParams::GetScriptOption(3)` picks a mode before the two compile calls
   (0x4bc0 then 0x75c0) run. Errors surface through
   `CompileParams::SetErrorString`.
5. `CompileParams::SetCodeID` after a clean compile.

## The language

The keyword table is in the binary as plain strings, and it is Pascal-shaped:

```
Call  case  const  else  endcase  endif  for  func  next
proc  repeat  return  step  until  var  while
```

That matches CED's published script language: `proc` and `func` for procedures and
functions, `if … endif`, `case … endcase`, `for … next` with `step`,
`repeat … until`, `var` and `const`. Keywords are matched case-insensitively —
`Call` and `Next` and `Return` appear capitalised as well as lowercase, which is
how the tokeniser spells them in prompts rather than a sign of two sets.

## The instruction set

`CScriptMgr::Disassemble` (0x404c0) walks the compiled program one instruction at a
time through a formatter at **0x48c70**, which takes an instruction index and writes
one line of text, returning the next index or `0xffffffff` at the end. That
formatter is where the opcode names come from: it dispatches through a jump table
rather than a `switch`, so the numbering had to be read from the disassembly.

Recovered. `Disassemble` asks a small lookup at **0x48320** for each instruction's
name, and that function gives the whole table away: opcodes 0 to 0x14 are formatted
from the string `const=%d` — inline constant references — opcodes 0x15 to 0x79 come
from a pointer array at **0x8a9e0**, indexed by `opcode - 0x14`, and anything above
0x79 is `NotPerm`. `tools/opcode_table.py` reads that array out of the file, so the
list below is regenerable rather than transcribed.

So the opcode is a **WORD**, 0 to 121, and the operands follow it as further words:
`Disassemble` loads `movzwl (program, ip, 2)` for the opcode and then walks a
per-opcode operand list.

The names, verbatim:

| opcode | mnemonic | opcode | mnemonic |
|---|---|---|---|
| 20 | `Const` | 21 | `Int:=` |
| 22 | `Flt:=` | 23 | `Int+` |
| 24 | `Int-` | 25 | `Int*` |
| 26 | `Int/` | 27 | `IntMod` |
| 28 | `Float+` | 29 | `Float-` |
| 30 | `Float*` | 31 | `Float/` |
| 32 | `FloatMod` | 33 | `InitInt` |
| 34 | `InitFloat` | 35 | `PopFor` |
| 36 | `CallAsProc` | 37 | `Call` |
| 38 | `IntProc` | 39 | `FloatProc` |
| 40 | `StringProc` | 41 | `Return` |
| 42 | `End` | 43 | `ReturnVar` |
| 44 | `GetArg` | 45 | `GetLocal` |
| 46 | `NullOp` | 47 | `Arg1` |
| 48 | `Arg2` | 49 | `Arg3` |
| 50 | `Arg4` | 51 | `Arg5` |
| 52 | `Local1` | 53 | `Local2` |
| 54 | `Local3` | 55 | `Local4` |
| 56 | `Local5` | 57 | `StringAssign` |
| 58 | `StringAdd` | 59 | `InitString` |
| 60 | `JumpRelImm` | 61 | `JumpRelImmF` |
| 62 | `Int+=` | 63 | `Int-=` |
| 64 | `Int*=` | 65 | `Int/=` |
| 66 | `Flt+=` | 67 | `Flt-=` |
| 68 | `Flt*=` | 69 | `Flt/=` |
| 70 | `String+=` | 71 | `IntEquals` |
| 72 | `IntNotEq` | 73 | `IntGreat` |
| 74 | `IntGtOrEq` | 75 | `IntLess` |
| 76 | `IntLessOrEq` | 77 | `FltEquals` |
| 78 | `FltNotEq` | 79 | `FltGreat` |
| 80 | `FltGtOrEq` | 81 | `FltLess` |
| 82 | `FltLessOrEq` | 83 | `Str=` |
| 84 | `Str<>` | 85 | `Str>` |
| 86 | `Str>=` | 87 | `Str<` |
| 88 | `Str<=` | 89 | `IntUMinus` |
| 90 | `IntUNot` | 91 | `FltUMinus` |
| 92 | `FltUNot` | 93 | `VoidProc` |
| 94 | `MakeIntFor` | 95 | `MakeFltFor` |
| 96 | `Next` | 97 | `LogAND` |
| 98 | `LogOR` | 99 | `LogXor` |
| 100 | `BitAND` | 101 | `BitOR` |
| 102 | `BitXor` | 103 | `FltLogAND` |
| 104 | `FltLogOR` | 105 | `FltLogXor` |
| 106 | `ResizeLocArr` | 107 | `ArrIndex` |
| 108 | `Arr2Index` | 109 | `Arr3Index` |
| 110 | `Arr4Index` | 111 | `Arr5Index` |
| 112 | `MakeNDArr` | 113 | `ResizeArray` |
| 114 | `CurView` | 115 | `GetSetView` |
| 116 | `SetView` | 117 | `GetView` |
| 118 | `SetViewSwap` | 119 | `MatTrans` |
| 120 | `MatDiag` | 121 | `Halt` |

Three things the names say about the language without reading the compiler:

- Arithmetic is typed. `Int+` and `Float+` are separate opcodes, as are `Int:=` and
  `Flt:=`, and so is comparison — eight of them for ints and floats, six more for
  strings. The compiler resolves types at compile time rather than tagging values
  at run time.
- `GetArg`/`Arg1`..`Arg5` and `GetLocal`/`Local1`..`Local5` are separate opcodes.
  The first five arguments and the first five locals get their own instructions
  rather than an index operand, which is a size decision a compiler makes when the
  common case is small.
- Arrays are their own opcode family: `ArrIndex` through `Arr5Index`,
  `MakeNDArr`, `ResizeArray`, `ResizeLocArr`, plus `MatTrans` and `MatDiag` for
  matrices. This is a language with real n-dimensional arrays, not a scripting
  veneer over lists.

`JumpRelImm` and `JumpRelImmF` are the two branches — one plain, one flagged, the
pair a `for`/`while` needs to know whether a loop is being entered or re-entered.
`PopFor`, `MakeIntFor`, `MakeFltFor` and `Next` are the loop setup and step, and
`CurView`, `GetView`, `SetView`, `SetViewSwap` and `GetSetView` are the script's
grip on the data windows — which is where a port would have to meet the rest of
Sharar rather than the interpreter.

## How large the built-in surface is

The number that decides whether a port is a project or a season:

| measure | count |
|---|---|
| exported symbols, `runtime.s2l` | 589 |
| exported symbols, `compiler.dll` | 756 |
| classes in `runtime.s2l` | 72 |
| methods on `CArrObj` alone | 61 |

`CArrObj` is the array type the `Arr*` opcodes operate on, and its exports are the
array function library: `Abs`, `ATan`, `ATan2`, `AmpPh`, `Conv`, `Diff`, `DimSize`,
`ArrHist` and fifty more. `CBaseViewCmd` adds 27 more for the data windows
(`GetAdcChannel`, `GetChannelList`, `CurrentMaxTime`, `GetCurrentSpkView`), and
`CBaseCmd` another 17 for the command plumbing.

There are also numerics that have nothing to do with scripting as such — `Beta`,
`BetaI`, `BinomialC`, `CBrent`, `CLinPred`, `CRand`, spline bases — because the
runtime doubles as a numerical library for the scripts that call it.

So the surface is of the order of several hundred functions, not a few dozen. A
port that runs the language but stubs the library would run almost nothing real: the
average Spike2 script is mostly calls.

## The front end that exists

`smr::script` parses the language. The grammar was not written from a manual but
read off the vendor's own scripts, and the test is that both of them parse:
`coef.s2s` (the one written during the filter work) and `basedemo.s2s`, the 43 KB
demonstration Spike2 ships, pulled out of the Windows VM. The demo yields 66
top-level items — 32 `proc`, 7 `func`, 88 `var`, 7 `const`, 48 `if`, 72 loops.

Each of these was found by the parser failing on it, which is a better teacher than
syntax anyone would guess:

| what | where it shows |
|---|---|
| member access, `View(App(3)).FileName$(1)` | a `.` token, and the `$` suffix carries through the member name |
| comments from `'` to end of line | the very first line of the demo is one |
| adjacent string literals concatenating across lines | a message split over four lines with no `+` between the pieces |
| hexadecimal literals, `0x2010` | a sound flag argument |
| compound assignment, `list$ += …` | building a string in a loop |
| dimensions repeated rather than comma-separated, `var m[250][300]` | an FFT results array |
| array parameters, `proc AddToWaterfall(xyh%, data[], incX%, incY%)` | the waterfall routine |
| a bare `[]` as an expression | `ArrConst(calcArr[i%][], [])` |
| `band`/`bor`/`bxor` | `if flags% band 1 then` |
| `while … wend`, `docase … endcase`, `halt` | confirmed against the keyword strings in `compiler.dll` |

Not present, though an outsider would expect them: `downto`, `div`, `array`,
`begin`, `dim`, `elseif`, `endwhile`, `exit`.

## The operand encoding

Read from the same formatter at 0x48c70, whose comparison ladder (0x48d6d to
0x49129) decides per opcode whether to read a second word. Every instruction is one
or two words. Eight opcodes take an operand word:

| opcode | operand | printed as |
|---|---|---|
| `GetArg` 44, `GetLocal` 45 | slot `n`, 1-based | `[n]:` and the name of slot `n - 1` |
| `JumpRelImm` 60, `JumpRelImmF` 61 | one word | signed 16-bit decimal |
| `MakeIntFor` 94, `MakeFltFor` 95 | one word | unsigned decimal |
| `MakeNDArr` 112, `ResizeArray` 113 | one word | unsigned octal, after `oct:` |

The rest take none. `Arg1`..`Arg5` and `Local1`..`Local5` carry slot 0 to 4 in the
opcode. `IntProc`, `FloatProc`, `StringProc` and `VoidProc` take no operand: the
listing looks up the instruction's own address in the procedure table and prints
the name it finds. Words 0 to 20 print as `const=N`. Words above 121 are symbol
references. The word minus 122 indexes one table at offset 0xb0 of the manager, and
past its end a second one at 0x80, whose entries can carry an int, a float or a
string that the listing prints beside the name.

The formatter also decides where a listing line ends. It ends after assignments,
calls, returns, jumps, the compound assignments, the loop opcodes, the array-index
and resize opcodes, `CurView`, `SetView` and `Halt`. Other instructions are joined
by a space, so one line is roughly one statement in postfix order.

`smr::script::bytecode` holds this as a decoder and a formatter, with a test that
compares its name table with the DLL.

## How the interpreter runs an opcode

There is no dispatch switch. A builder at 0x19c0 allocates one C++ object per
opcode, in opcode order, and each class's third virtual method executes it. The
classes carry RTTI names, so each opcode's class is named in the DLL:
`TJumpImmediateOperator`, `TJumpImmediateIfFalse`, `TMakeFor`, `TNext`, and for
assignment `TAssign<CInterpreter::AssignInteger>`. Comparing vtables shows that
slot 2 is the only one that differs between them.

The control-flow opcodes, read from those methods:

- `JumpRelImm` (0xaf10) adds its signed operand to the index of the word after
  the operand. A target below -2 raises error -7016. A target past the end of the
  code is ignored and execution carries on, which looks like a bug and is kept.
- `JumpRelImmF` (0xd900) pops a value and jumps only when it is exactly zero. A
  float NaN counts as true and does not jump.
- `MakeIntFor` and `MakeFltFor` are one class with an int flag (0xb100). They pop
  the step, the limit, the start and the loop variable, set the variable to the
  start, and raise -7030 for a zero step (and, for floats, a NaN step). If the
  start is already past the limit, they skip forward by the unsigned operand.
  Otherwise they push a loop record that remembers where the operand was.
- `Next` (0xc4f0) adds the step. While the variable has not passed the limit it
  jumps back to the word after `MakeFor`'s operand. When the loop ends it pops the
  record. For floats the comparison is the compiled one: a NaN keeps an upward loop
  running and ends a downward one.

The builder also names every other opcode's class. The arithmetic, comparison,
logical and assignment opcodes are template instances over a `CInterpreter` member,
so the RTTI gives the operation directly: `TBinaryOperator<IntegerAdd>`,
`TBinaryOperator<FloatLess>`, `TAssign<AssignFloatDiv>`, `TUnaryOperator<FloatNot>`.
Read from their methods:

- Ints are 64-bit. `+` and `-` wrap. `*` multiplies, divides back, and raises -7011
  if the quotient differs. `/` and `mod` raise -7012 for a zero divisor.
  `i64::MIN / -1` is not checked and faults in hardware.
- Float `/` and `mod` (C `fmod`) raise -7012 for a zero or NaN divisor.
- A float read as an int truncates, and raises -7011 outside ±2^63 or for NaN.
- Comparisons push the int 0 or 1. `FltEquals` uses `ucomisd` with `je`, so a NaN
  compares equal to anything and `FltNotEq` is false. The ordered float comparisons
  are false for NaN.
- `LogAND`, `LogOR` and `LogXor` give 0 or 1; `BitAND`, `BitOR` and `BitXor` work on
  the bits. The float logical forms and `FltUNot` treat NaN as zero.
- Assignment pops the value, then the variable. Compound assignment reads the
  variable, combines it with the value, and writes it back.

`smr::script::exec::Machine` runs these opcodes, the two jumps, the loop opcodes
and `Halt` on a word stream, with a test for each rule above and small programs
(a for loop summing 1 to 10, a while loop from the two jumps). Any other opcode
stops the run with its address rather than guessing. Variables are supplied by the
caller; `smr::script::compile` builds the word stream and the symbol tables that go
with it, as the next section records.

## Checked against Spike2

`tools/oracle/run.sh <name>` copies `tools/oracle/<name>.s2s` into the Windows
guest, restarts Spike2 with a one-line `startup.s2s` that runs it, and pulls back
the binary file the script writes with `BWrite`. Two scripts have been run in
Spike2 8.10:

- `t1.s2s`: assignment, integer arithmetic, a `for` loop summing 1 to 10, a
  `while` loop, float arithmetic and a downward float loop. Spike2 returned
  a% = 1, b% = 0, s% = 55, i% = 11, c% = 8, x = -0.25, y = 2.75, z = 2.5.
- `t2.s2s`: NaN against the six comparisons and `not`, `-2.7` stored in an int,
  `mod` and `/` with negative operands, float `mod`, a loop skipped on entry, a
  float loop with step 0.5, and `band`, `bxor`, `xor`.

Spike2 agreed with every rule read from the DLL: NaN = 1 is true and the ordered
comparisons are false, `not NaN` is 1, `-2.7` truncates to -2, `-7 mod 3` is -1,
`7 mod -3` is 1, `-7 / 2` is -3, `-7.5 mod 2.0` is -1.5, a skipped loop leaves the
variable at its start, and the float loop ends one step past its limit at 2.5.
`n and 1` with a NaN `n` stopped the script with "Integer Overflow", which is the
-7011 that converting NaN to an int raises.

The tests `t1_matches_spike2` and `t2_matches_spike2` run hand-assembled
equivalents through `Machine` and compare with those numbers, and with the files
themselves when `vmbin/oracle/` holds them. Changing the NaN rule for `=` makes
the second one fail.

What this does not check is the instruction layout. The programs are assembled
by hand because the compiler is not ported, so the order Spike2's compiler emits
operands in, and which opcodes it picks, are still only read from the DLL.
`DebugOpts(1, 1)` is the option whose label is "Write compiled code to
'default.cod'" (string `0x2a26 + option` in `runtime.s2l`), but setting it from a
startup script wrote no such file anywhere on the guest's disk.

## The compiler

`smr::script::compile` turns a parsed script into the words the interpreter runs. It
is a port of the vendor's code generator, class by class: every syntax-tree class in
`compiler.dll` has one generator as its first virtual method, and every generator
writes through a single emitter, so the layout was recoverable rather than guessed.

| the vendor's class | generator | what it emits |
|---|---|---|
| `CExpLitInt` / `CExpLitFloat` / `CExpLitString` | 0x8000, 0x1b690, 0x8410 | a constant word, or a pool entry above the symbols |
| `CExpBinaryOp`, `CExpUnaryOp` | 0x7af0, 0x2db60 | the typed opcode for the operator |
| `CAssignStatement` | 0x8150 | the target's reference, the value, `Int:=` or its float/string twin |
| `CIfStatement`, `CWhileStatement`, `CRepeatStatement` | 0x7410, 0x30e60, 0x30c70 | a test and `JumpRelImmF`/`JumpRelImm`, with the offset patched |
| `CForStatement` | 0x6c80 | `MakeIntFor`/`MakeFltFor`, the body, `Next`, and the skipped length |
| `CProcFuncStatement`, `CCallStatement` | 0x6a00, 0x7e60 | the entry word, or arguments, count, target and `Call` |
| `CExpArray`, `CResizeStatement` | 0x18580, 0x4f50 | the reference and `ArrNIndex`, or `ResizeArray` and a dimension mask |
| `CDoCaseStatement`, `CBreakContinueStatement`, `CHaltStatement` | 0x5780, 0x30a00, 0x304e0 | `docase` chains, `PopFor` and jumps, `Halt` |

Some rules are the vendor's rather than the language's. Each was read off the
generator and then confirmed against the vendor's compiler, and each is reproduced
deliberately:

- A symbol occupies a word from 126 up, variables and constants in declaration order
  and then the routines. Constants follow, pooled: strings, then integers, then
  floats, each sorted. Integers 0 to 20 and whole floats 0 to 20 are their own
  opcodes and take no pool entry.
- Constant expressions fold at compile time with 64-bit integers, and an overflow or
  a division by zero is a compile error. A unary operator folds only through a bare
  literal, so `not 1` is the constant 0 while `not (2-1)` is the constant 1 followed
  by `IntUNot`.
- `xor` of two constants folds as a logical operation, but the same operator on two
  variables compiles to `BitXor`.
- A comparison binds tighter than `band` and `and`, so `flags% band 1 = 1` is
  `flags% band (1 = 1)`.
- `and`, `or` and `xor` between floats use the integer opcodes.
- `if` with a constant condition keeps only the branch it takes. `while 0` disappears
  and `while 1` drops its test. A `docase` arm with a constant-false condition keeps
  the test and drops the body.
- Code after `break` or `continue` in the same block is dropped; code after `halt` is
  kept. `break` in a `for` loop emits `PopFor` first.
- A declaration's initialiser compiles as an assignment unless it folds, which is why
  `var a% := 25` emits nothing and `var a% := b%` emits the store.

How it was checked. `tools/compile_oracle.py` runs the vendor's own compiler over
every case in `tools/compile_cases` and writes the words it produced as a fixture;
`smr/tests/compile_reference.rs` compiles the same files with this port and compares
word for word. All 75 cases match, and the two cases the vendor refuses (a `break`
outside a loop and a call to a built-in) are refused here too. The harness that calls
the vendor compiler is `compile_oracle.c` beside CED's DLLs, outside this repository,
because the DLL is not redistributable; the fixture is gitignored for the same reason,
so a fresh clone skips that test and runs the unit tests in `compile.rs`, which pin
the same rules on a handful of scripts.

What is missing is the call. `runtime.s2l` registers the built-ins and the vendor
resolves a call against them at compile time; this port now has the table (below) but
does not yet compile a call from it, so a script that calls `Abs`, `Len`, `View(…)` or
any other command still stops with an error naming it rather than compiling to words the
interpreter could not run. Member calls (`View(0).MaxTime()`) fail the same way. That is
the honest boundary: a program made of the language itself compiles to the same words
Spike2 produces, and a program made mostly of calls — which is most real scripts — does
not compile yet.

## The command table

`runtime.s2l` has no list of its commands. Each command is a C++ object whose
initialiser ends in one call to `CBaseCmd::IXCmd(this, name, min, max, descriptor)`,
imported from `compiler.dll` (0x1800061d0). `tools/command_table.py` finds every such
call, replays the few instructions that set its arguments up, and reads the name and the
descriptor out of the file. It finds 497 commands, 495 with a fixed name in their own
initialiser and two (`CursorLabel`, `HCursorLabel`) that share one initialiser and take
the name from a member their constructors fill in. Names are unique without regard to
case, which is how the vendor's table compares them: it lower-cases a name before it
looks it up (0x180014350).

The descriptor is a run of 16-bit words that the vendor parses at registration
(`compiler.dll` 0x180005c80) and refuses if it is malformed:

- the first word is the return type: 0 none, 1 integer, 2 real, 3 string. Of the 497
  commands, 297 return an integer, 107 a real, 41 a string and 52 nothing;
- then argument type words. The low seven bits are the type, at most 0x13; bits 8 to 12
  are the number of array dimensions, which only types 1 to 6 may have; 0x80 is a flag
  whose meaning is not recovered;
- `5` ends one argument list and `7` ends the descriptor. A command has one list per
  form it accepts: `Len` has five, whose words are 3, 390, 387, and the pairs 18,1 and 19,1;
- `0xe` … `0xf` brackets a group of arguments that are written back, and the group
  counts as one argument.

The registration also carries the fewest and most arguments the command takes, and the
vendor accepts it only if the lists agree: no list shorter than the minimum, and the
longest exactly the maximum. All 497 satisfy that check here (it is a test in
`smr/src/script/commands.rs`), which is the evidence the decoding is right. The types
that appear are 1 to 4, 6, 8 to 12 and 16 to 19 (14 and 15 are the group brackets); the meaning of 1, 2 and 3 as integer,
real and string follows from the return types, and the rest are unnamed so far.

`smr::script::commands` exposes the table as data: `all()` and `lookup(name)`.
It describes the interface. It does not run anything, and it does not yet say how the
engine numbers the commands. Registration (0x180006230) starts the count at 0x7a and
gives each command the word 0x7a plus its place in the list it builds, so the words of a
script's variables and the words of its calls both depend on that order, and it is not
reproduced yet.

## What is not known yet

- what the `MakeNDArr` operand counts. `ResizeArray`'s operand is settled in the
  sense that it can be reproduced — one bit per dimension, three apart, giving 1, 9
  and 73 for one, two and three dimensions — but not in the sense that its meaning is
  understood;
- what sets the bound at +0x30 of the code frame (probably the program length);
- the order the engine numbers the commands in, which decides every symbol word once
  a script calls one;
- the argument type words 4, 6 and 8 to 19, and the 0x80 flag;
- the type system: `TIndex`, `TArray`, `TResultArray` suggest arrays are
  first-class values with dimensions and strides, which matters for a port. Enough of
  it is now visible to compile: an array is a slot, an element is `ArrNIndex` on a
  reference, and a parameter is passed as the slot itself.

The symbol words are settled: they index the global table and then the external
command table, in that order, which is why the first variable is 126 — `0x7a` plus
the four command slots a bare compile registers.

## What a port would still need

The lexer, the parser and the compiler now exist and agree with the vendor's on
everything they cover. Two things stand between them and a Script menu that runs a
real script. The commands are the large one: the table now names 497 built-ins and their
argument lists, but each still has its own behaviour to be read out of `runtime.s2l`,
and the compiler does not yet emit a call. The other is the interpreter's remaining
opcodes — the array, matrix and view families — which `Machine` currently stops on
by name. A partial engine that runs most scripts but differs in a corner is worse
than no Script menu, because a script that runs is trusted, which is why each piece
is checked against Spike2 rather than against a reading of the manual.
