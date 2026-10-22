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
time through a formatter at **0x48c70**, which takes an instruction index and
writes one line of text, returning the next index or `0xffffffff` at the end. That
formatter is where the opcode names live, and it is the next artifact: decompiling
it gives the full instruction set with its numbering, and after that the compiler's
emission sites give each instruction's operands.

It is a jump table rather than a `switch` in the decompilation, so the opcode
dispatch has to be read from the disassembly rather than from the C.

## What is not known yet

- the instruction set and its encoding (the formatter above);
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
