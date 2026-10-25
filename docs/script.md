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

`compile` is the bare compiler, as the vendor's is before `runtime.s2l` has registered
anything, and refuses a call to a command by name. `compile_with_commands` registers the
whole table, as Spike2 does, and compiles calls to commands (next section). Member calls
(`View(0).MaxTime()`) are not compiled yet.

## The command table

`runtime.s2l` has no list of its commands. Each command is a C++ object whose
initialiser ends in one call to `CBaseCmd::IXCmd(this, name, min, max, descriptor)`,
imported from `compiler.dll` (0x1800061d0). `tools/command_table.py` finds every such
call, replays the few instructions that set its arguments up, and reads the name and the
descriptor out of the file. It finds 503 commands: most have a fixed name in their own initialiser;
two (`CursorLabel`, `HCursorLabel`) share one and take the name from a member their
constructors fill in; and six pairs (`XLow`/`XHigh`, `YLow`/`YHigh`, `IIRBp`/`IIRBs`,
`IIRLp`/`IIRHp`, `IIRReson`/`IIRNotch`, `IIRName$`/`IIRComment$`) pick a name by a byte
the constructor sets, with `cmp [this+d], v; cmovne`. Names are unique without regard to
case, which is how the vendor's table compares them: it lower-cases a name before it
looks it up (0x180014350).

The descriptor is a run of 16-bit words that the vendor parses at registration
(`compiler.dll` 0x180005c80) and refuses if it is malformed:

- the first word is the return type: 0 none, 1 integer, 2 real, 3 string. 
- then argument type words. The low seven bits are the type, at most 0x13; bits 8 to 12
  are the number of array dimensions, which only types 1 to 6 may have; 0x80 is a flag
  whose meaning is not recovered;
- `5` ends one argument list and `7` ends the descriptor. A command has one list per
  form it accepts: `Len` has five, whose words are 3, 390, 387, and the pairs 18,1 and 19,1;
- `0xe` … `0xf` brackets a group of arguments that are written back, and the group
  counts as one argument.

The registration also carries the fewest and most arguments the command takes, and the
vendor accepts it only if the lists agree: no list shorter than the minimum, and the
longest exactly the maximum. All 503 satisfy that check here (it is a test in
`smr/src/script/commands.rs`), which is the evidence the decoding is right. The types
that appear are 1 to 4, 6, 8 to 12 and 16 to 19 (14 and 15 are the group brackets); the meaning of 1, 2 and 3 as integer,
real and string follows from the return types, and the rest are unnamed so far.

`smr::script::commands` exposes the table as data: `all()`, `find(name)` and `word(index)`.

### The order, and the numbering

A command's word is `0x7a` plus its place in the list the compiler builds at registration
(`RegisterExternalCommands`, 0x180006230), and the first word a script's own symbols can
use follows the last command: 126 + 503 = 629. The list is `CHeadCmd::m_plstCmd`, and
`CBaseCmd`'s constructor appends to it (every call in `runtime.s2l` passes `false`, so
nothing is pushed to the front). So the order is the order the objects are constructed
in. Creator functions allocate each command (`new`, then a constructor that stores the
vtable) and call the next creator; walking them from `0x18004f200` in address order, and
mapping each object's vtable to its `Initialize` slot (vtable + 8) and so to the
registration found above, gives the order. `tools/command_table.py` does that and
writes the table in that order.

Registering the table in that order with the vendor compiler (see
`tools/compile_oracle.c`: it constructs a `CBaseCmd` per command and calls the real
`IXCmd` with the file's own descriptor) puts the first variable at 629 and `Abs`, `Len`
and `Mid$` at 551, 447 and 516, which is the formula. That checks the numbering rule.
It cannot check the order itself against Spike2, because `runtime.s2l` will not load
outside Spike2 (it imports from `SONVIEW.exe`); the order is derived from the code that
builds it.

### What a call compiles to

```
args…, count, [form], command word, Call
```

`count` is the number of arguments, `form` is which of the command's argument lists the
call fits, counted from one, and is present only when the command has more than one.
Both are integer constants, so a pooled constant beyond 20 like any other. A command
used as a statement that returns a value ends in `CallAsProc` rather than `Call`; one
that returns nothing ends in `Call` either way and cannot be assigned. An argument is
compiled as it would be anywhere else; a whole array is its symbol alone.

The compiler matches the arguments against the command's argument lists in order and
takes the first that fits (`compiler.dll` 0x180016a40, 0x180016d50, 0x180016dc0). A list
may be longer than the arguments, never shorter. An argument is described by three
things: how many dimensions it names (`a[]` is 1, `a[][]` is 2, a scalar 0), the type
of its value, and whether it is a variable or an element of one. A parameter word fits
as follows:

- dimensions (0x180017030): a scalar fits a word with no dimension bits and not types
  0x12 or 0x13. An array of rank *n* fits if the word allows it: bit `0x100 << (n-1)` of
  the word's dimension field, where type 8 allows only rank 1, types 4, 0xc and
  0x11 to 0x13 allow any, and type 0x10 allows ranks 1 and 2;
- elements (0x18002d610): types 1 and 9 want integers, 2 and 10 reals, 3, 0xb and 0x13
  strings, 6, 8, 0x10 and 0x12 either number, and 5, 7, 0xe and 0xf nothing;
- scalar types: 1, 2, 6, 8, 9, 0x10 and 0x11 take an integer or a real, 3 and 0xb a
  string, 10 a real, and everything else, including 4 and every word with the 0x80 flag
  in a scalar position, takes any;
- types 9 to 13 without the 0x80 flag are references and want a variable or an element;
  and an array that is a variable needs the 0x80 flag.

How a reference parameter is marked afterwards (`vtable + 0x60` on the argument sets a
byte on its symbol) emits no code.

A group `0xe … 0xf` is a procedure passed as an argument (`ToolbarSet`, `DlgButton`,
`ZeroFind`, `DlgMouse`). The word after `0xe` is the return type the procedure must have
(0 for a `proc`, 1, 2 or 3 for a `func` of that type) and the words up to `0xf` its
parameters, which must be the same in number and each an integer for 1 and a real for 2,
by value or by reference, never an array. Only four such groups exist in the table. The
argument is the procedure's name alone, and compiles to its symbol word.

### How it was checked

`tools/call_oracle.py` writes, for every command and every form it accepts, a script
that calls it with right arguments, with fewer, with one too many, and with each of its
first three arguments replaced by each of 22 shapes (literals, variables, a constant,
elements, arrays of three ranks, expressions, a parenthesised variable, a call). It runs
them through the vendor compiler with the table registered and writes the answers;
`smr/tests/call_reference.rs` replays them through `compile_with_commands`. Of 32,065
scripts (including a matching procedure and several near misses for each group),
14,861 compile to the same words and 17,204 are refused by both; none differ. The fixture is gitignored as the
compiler fixture is, so a fresh clone skips that test and runs the unit tests in
`compile.rs`, which pin the same rules on a few scripts.

## The interpreter

`smr::script::vm` runs a compiled program. Storage is an arena of cells, globals first, with
a call allocating cells for its by-value parameters, locals and result above them. A
reference is a cell or one element of an array cell, so a by-reference parameter and a
whole array passed to a procedure are the caller's own cells. The scalar arithmetic, the
comparisons and the loop opcodes use the helpers transcribed from the vendor's `Execute`
methods in `exec.rs`.

Commands the interpreter does not run itself go to a `Host` (`host.rs`), which also owns the
current view: `GetView`, `SetView`, `GetSetView` and `SetViewSwap` call it. `Session`
(`session.rs`) is a host built on the `.smr` reader: it opens and closes files and answers
the queries on a view and its channels. Everything else (the pure commands) is in
`vm/builtins.rs`, with the C formatter that `Str$`, `Print` and `Print$` use in
`vm/printf.rs`.

### How it is checked

The compiler is held to the vendor's words. The interpreter is held to Spike2's behaviour by
running the same programs in Spike2. `tools/run_oracle.py` takes every program in
`tools/run_cases/`, each of which declares a global array `res` and fills it, appends code that
writes `res` to a file with `BWrite`, and chains them: `ScriptRun` at the end of one program
starts the next, so one launch of Spike2 in the Windows guest runs a batch (a program that
raises a script error leaves a dialog open and stops the chain; the program that wrote no file
is the one that failed, and the batch restarts after it). `smr/tests/run_reference.rs` runs the
same programs through the port and compares every element bit for bit. Programs named `g_*`
are written by `tools/gen_run_cases.py` and grid a command over its inputs; `d_*` open the
example recordings.

What that found, beyond what could be read from the code:

- Symbols are numbered in the order the source declares them, routines included, and a `var`
  inside a block at the top level is a global. The compiler had numbered variables first and
  routines after, which is only the same when the routines come last.
- `Str$`, `Print` and `Print$` format with the old Microsoft C runtime: three-digit exponents
  (`1e+006`), a number cut to 17 significant digits and then rounded half up (`2.25` with
  `%.1f` is `2.3`), only the conversions `d x X c s f e E g G`, `*` for a missing argument or a
  number given to `%s`, and any argument the format does not use appended after it with a
  newline.
- `Max` and `Min` of an array return the index of the first extreme element.
- `Len` of an array with more than one dimension is an error.
- An integer array met by a real scalar: `ArrAdd`, `ArrSub` and `ArrSubR` cut the scalar to an
  integer first; `ArrMul`, `ArrDiv` and `ArrDivR` work in real and cut the result. Against a
  real array, all work in real and cut the result.
- `ArrSort` sorts descending when its mode is odd and permutes any further arrays the same
  way; `ArrSum(a[], &mean, &sd)` returns the sum and sets the mean and the sample standard
  deviation.
- `Count` on an event channel counts the events whose tick is in `[round(t0/tick),
  round(t1/tick))`, and on a waveform channel is the mean of the samples whose ticks are.
- A query on a channel that does not exist returns a code rather than raising an error:
  `ChanKind` 0, `ChanTitle$` an empty string, `ChanComment$` `No comment`, `NextTime` -1505.

### The maths library

`Sin`, `Cos`, `Tan`, `ATan`, `Sinh`, `Cosh`, `Tanh`, `Exp`, `Ln`, `Log` and `Pow` (and the
distributions in `Rand*`, which take logarithms) call `msvcr110.dll`'s routines, and a host libm
rounds many arguments differently in the last bit. They are therefore the DLL's own:
`tools/lift_libm.py` reads the DLL and writes `s2lift/src/msvcr110_gen.rs`, one Rust function
per routine in which each x86 instruction is one call on the small machine model in `s2lift/src/mach.rs`
(registers, flags, SSE2 scalar arithmetic, a stack), plus `msvcrt.dat`, the constant tables the
routines read. The DLL's error reporting (errno, `_matherr`) is not translated; each of its entry
points answers with the value it was given. Translating the instructions rather than transcribing
a decompiler's output matters: a decompiler prints `a * b * c * d` for a product the code groups
as `a * (b * c * d)`, and the grouping decides the last bit.

`tools/libm_fuzz.py` runs the real DLL under Wine (`WINEDLLOVERRIDES=msvcr110=n`; without it Wine
substitutes its own implementation, which is the host's libm) and compares every function on
a million arguments each: random bit patterns, log-uniform magnitudes, values next to multiples of
pi/2, denormals, NaNs and infinities. Zero differences. `smr/tests/libm_reference.rs` keeps a
few hundred of each, and the `m_libm_*` programs compare against Spike2 itself.

### Special functions

`LnGamma`, `GammaP`, `GammaQ`, `BetaI` and `BinomialC` are Boost.Math code inside `SpikeUtl.dll`.
`tools/lift_spikeutl.py` translates the routines the same way (`tools/lift.py` is the shared
translator: it follows calls, takes functions split into pieces by their unwind data, resolves
jump tables and calls through the import table, and cuts the C++ error-reporting paths).
`tools/util_fuzz.py` compares them with the real `SpikeUtl.dll` under Wine on tens of thousands of
arguments each, with no difference; `m_special` compares with Spike2 itself. Out-of-range
arguments raise the codes `GetI32Err` (-1000) and `GetFloatErr` (-1001) raise.

### Spike2's own command code (`s2lift`)

Beyond the hand-written commands, the crate `s2lift` holds each command's `Execute` and the routines it
calls, translated from the DLLs' machine code by `tools/lift_runtime.py` (the shared translator is
`tools/lift.py`): `runtime.s2l`, `compiler.dll` (arrays, `CArrObj`), `SpikeUtl.dll` (maths, splines, FFT),
`spikecmd.dll` (the view-command base classes) and the data file engine itself (`sonlayer.dll`,
`son64.dll`). Each instruction becomes one call on the machine model in `s2lift/src/mach.rs`, which is
checked against the processor by `s2lift/tests/machine.rs`. The host answers what the code imports:
`smr/src/script/vm/lifted/` (`mod.rs`: the script's arguments and result, `CRunInfo`, view messages;
`cstring.rs`: MFC strings; `win.rs`: files, time, memory and the C library).

Order of dispatch: a hand-written command first, then Spike2's own code. `S2LIFT_FORCE=Name,Name` (or `all`)
runs Spike2's code instead, and `S2LIFT_SKIP=` takes names out of that; `S2LIFT_SERVICES=1`,
`S2LIFT_CALLS=1` and `S2LIFT_BLOCKS=1` trace what the code asks for. Commands run this way and checked
against Spike2: `ArrConv`, `ArrFFT`, `ArrFilt`, `ArrDiff`, `ArrIntgl`, `ArrStats`, `ArrHist`, `ArrSpline`,
`Date$`, `Time$`. Forcing everything through it (`all`, with `FileOpen` and the view commands skipped)
agrees with Spike2 on 91 of the 120 reference programs; the rest wait for the window/view services of
SONVIEW (`CWnd::FromHandlePermanent`, the channel-list messages, the dialogs).

## What is not known yet

- what the `MakeNDArr` operand counts. `ResizeArray`'s operand is settled in the
  sense that it can be reproduced — one bit per dimension, three apart, giving 1, 9
  and 73 for one, two and three dimensions — but not in the sense that its meaning is
  understood;
- what sets the bound at +0x30 of the code frame (probably the program length);
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
real script. The commands are the large one: the table now names 503 built-ins and their
argument lists and the compiler emits calls to them, but each still has its own
behaviour to be read out of `runtime.s2l`. The other is the interpreter's remaining
opcodes — the array, matrix and view families — which `Machine` currently stops on
by name. A partial engine that runs most scripts but differs in a corner is worse
than no Script menu, because a script that runs is trusted, which is why each piece
is checked against Spike2 rather than against a reading of the manual.

### Window messages and exceptions in the translated code

Many commands do their work by sending a window message (`SendMessageW`) to the main window or a
view. `tools/lift_runtime.py --msg=0x244e,…` finds the handler of each listed message in SONVIEW's
message maps and translates it; the host runs it with a zeroed object standing for the window
(`World::wnds`). So `Error$` is SONVIEW's own `0x244e` handler, reading its text from the string
tables dumped by `tools/dump_strings.py`, and the 1401 error texts from `USE1432_vm.dll`. Handlers
that need the real window tree (`ViewFind`'s view list, the channel list of a view) are written
against the session instead and compared with Spike2.

C++ `catch` is supported for routines with one try block: `_CxxThrowException` unwinds as a Rust
panic carrying `CxxThrow`, and the routine's wrapper runs the catch funclet on the frame it would
have had and resumes where the funclet says (`Lifter.eh_of` in `tools/lift.py`). `InStrRE` relies on it
for bad patterns.

### Channel measurements and searches

`ChanMeasure` and `ChanSearch` send `0x15d1` / `0x15d3` to the current view. The host answers by building a
stand-in for the channel view (`mock_view` in `lifted/mod.rs`): an object whose vtable is the real vtable of
SONVIEW's class for the channel's kind (`CAdcView`, `CRealView`, `CEventView`, `CMarkerView`, the wavemark
and textmark views), whose channel number is at +0x170 and kind at +0x174, and whose owner at +0x140
answers the few virtual calls the routines make (the data file, seconds per tick, seconds to ticks). The
class's own slot 0x4d0 (measure) or 0x398 (search) then runs, translated. `lift_runtime.py --vslot=` lifts
chosen slots of a vtable; `tools/` loops that add missing slots are in the session notes. Every measure
type (area, mean, slope, sum, … sem) and every search mode agrees with Spike2 bit for bit.

### The main window and the sampling settings

The `Sample*` commands that only read or change the sampling settings work on a `CConfig` object that the
application keeps at +0xc870 of its main window (a thin wrapper over a `CSamModeInfo` inside the window,
built by `CMainWindow`'s constructor). The host builds the same thing (`Run::main_window`): a zeroed window
object, `CSamModeInfo`'s constructor, SONVIEW's own start-up routine `0x14008bc80` (which sets the
defaults, reading "Preferences" through a stand-in application object whose `GetProfileInt` answers with
its default), and the `CConfig`. The commands find it through message `0x2059`. Commands that depend on
the connected 1401 (`SampleTimePerAdc`'s limits, starting, stopping) are not covered.

### The filter bank

`Filters.dll` is translated as a ninth module (`filters`). The main-window stand-in holds a `CIIRFilterBank`
(+0xcdf0) and the `IIRDefine` the bank commands edit (+0xcdd0); messages `0x540`/`0x541` (set / get a
filter) are SONVIEW's own handlers. Commands that share a class (`IIRName$` / `IIRComment$`) are told
apart by the order their objects are built in. Note for tests: the application keeps this state for the whole
Spike2 session, so a test case must set what it reads.

### Start-up of the translated application

The translated SONVIEW and Filters modules run their C++ static initialisers (the compiler's `.CRT$XCU`
table, found by `crt_run` in `lift_runtime.py`) when the first command runs, and the stand-in main window is
built by SONVIEW's own `CMainWindow` constructor, with services nothing provides answering zero (the MFC
window classes it derives from and the toolbars it creates). That leaves the window's real state
(colour tables, the sampling configuration, the filter bank) for the application-level message handlers.
What it does not have is the document/view tree, so commands that list or address views (`App`, `ViewList`,
`WindowGetPos`) are still the session's own.

### The data view's window, document and channel views

For a data view the host builds SONVIEW's own objects (`time_window` in `lifted/mod.rs`): a `CTimeWindow`
made by its constructor, a `CTimeDoc` holding the data file object, and one channel view per channel made by
SONVIEW's factory (`0x140130230`) — `CAdcView`, `CEventView`, `CAdcMarkView` and so on, with their real
vtables, kept in the window's vector by channel index. Services of the window classes (`CWnd::Create`,
painting, device contexts) answer "done", and constructors that nothing provides become do-nothing
objects. `ChanMeasure`, `ChanSearch`, `ChanValue` and the rest of the channel commands then run
against the same objects the application uses.

`tools/command_census.py` calls every built-in once with made-up arguments of the right types and lists the
commands that stop because something is missing (`unported`); the count is the measure of what is left.

### Where the cursor commands stand

The window's cursor manager (`CCursorHelper`, made by SONVIEW's routine `0x1400ccbb0`) and the handlers of the
cursor messages are lifted, and every `Cursor*`/`HCursor*` command now reaches them. What stops them is the
creation of the cursor objects themselves, which are child windows (`CWnd::Create`, `GetParent`, the parent
frame): the first step that dereferences a parent that does not exist. `tools/pending_cases/d_hcursor.s2s` is
the test for them (Spike2's answers are recorded by `run_oracle.py --only`); it moves to `tools/run_cases/`
when they agree. The reference file `testdata/run_reference.txt` holds the date `t_date` ran on, so it must
be regenerated (`run_oracle.py --only t_date`) when the day changes.


### Status of the 503 commands

`tools/command_census.py` calls each command once. At the last run, 360 ran, 94 refused the made-up
arguments (so are implemented), 9 could not be compiled with them and 40 stopped on something the port lacks:
the `Toolbar*`, `Dlg*` and `Interact` dialogs and toolbars; `Process*`, `ViewLink`, `TabSettings`, `DupChan`,
`ChanSave`, `MemSave` and `MemDeleteTime`, which need more of the window's own state (its axes, the result
view); the text-view editing and export commands; `Eval` and `Debug*`, which need the compiler at run
time; and the multimedia and playback commands. The 1401 commands (`U1401*`, `Sample*` beyond the
settings) need the hardware library.

Two things differ from the Windows machine the reference was recorded on, because they are the machine's
state and not Spike2's: the serial ports (the port has none, where that machine has COM1) and the stored
settings (`Profile` starts empty). A test case must write what it reads.


### Regenerating the translated code

`tools/regen/regen.sh` runs `lift_runtime.py` with every root, message list and virtual-table slot the
port needs (`tools/regen/vslots.txt` and `addrs.txt` hold the long lists) and then builds. It needs a Python
with `capstone` (`PY=`) and the compiler oracle directory (`ORACLE_DIR=`). A new routine the port has to call
is added to `addrs.txt` as `--addr=module:0xADDRESS`.
