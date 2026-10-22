# Spike2's script engine

Sharar has no Script menu. This is the first pass at what one would run: the
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

## What is not known yet

- the operand encoding: which words follow which opcode;
- the built-in function table — Spike2 ships hundreds of functions and they are
  registered rather than compiled in, so the table is the place to look;
- how `runtime.s2l`'s command objects are bound to script names;
- the type system: `TIndex`, `TArray`, `TResultArray` suggest arrays are
  first-class values with dimensions and strides, which matters for a port.

## What a port would need, honestly

A language is not a dialog. Sharar would need a lexer, a parser, a compiler and a
runtime, each of which has to agree with this one — and a partial engine that runs
most scripts but differs in a corner is worse than no Script menu, because a script
that runs is trusted. That is why this file exists before any code: the estimate
depends on how large the built-in table is, and that is measurable.
