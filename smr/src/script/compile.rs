//! Spike2's compiler: source text to the 16-bit word code that [`super::exec`] runs.
//!
//! This is a port of the code generator in `compiler.dll`, not a design of its own.
//! Every rule here was read from the vendor's generator and then checked word for
//! word against the vendor's own compiler run over the same script. The generators
//! are the first virtual method of the `cedscr` syntax-tree classes — `CForStatement`
//! at 0x6c80, `CExpBinaryOp` at 0x7af0, `CExpLitInt` at 0x8000 and the rest — and
//! every one of them appends through a single emitter at 0x10d80, which is what
//! made the layout tractable: one function for how a word is written, plus the
//! fixed per-opcode operand shape already recorded in [`super::bytecode`].
//!
//! What a script compiles to:
//!
//! - The main program, then `Halt`, then each `proc`/`func` body in declaration
//!   order, each ending in `End`, then a final `Halt`.
//! - Symbols occupy words from 126 upwards: variables and constants in declaration
//!   order, then the routines. The base is 0x7a plus the four command slots the
//!   vendor's table always holds. A compile with `runtime.s2l` loaded would shift
//!   every number by the size of its command table, so 126 is the bare-compile base
//!   rather than a property of the language.
//! - Constant words follow the symbols: strings first, then integers, then floats,
//!   each pool sorted ascending. Integers from 0 to 20 and whole floats from 0 to 20
//!   need no pool entry — they are their own opcodes.
//!
//! Four behaviours belong to the vendor rather than to the language it looks like,
//! and are reproduced deliberately. `xor` of two values compiles to a bitwise xor
//! while `xor` of two constants folds as a logical one. A comparison binds tighter
//! than `band`, so `flags% band 1 = 1` means `flags% band (1 = 1)`. `and`, `or` and
//! `xor` between floats use the integer opcodes. Code after `break` in the same
//! block is dropped, while code after `halt` is kept.
//!
//! What is not here: the command table. `View(0).MaxTime()`, `Abs`, `Len` and the
//! rest of the built-ins are looked up in `runtime.s2l` by the vendor at compile
//! time; a script that calls one stops with a [`CompileError`] naming it rather than
//! compiling to words this crate could not run.

use super::parser::{Expr, IndexArg, Stmt};

/// The first word a symbol can occupy when no commands are registered: `0x7a` plus four
/// slots. With the command table registered it follows the last command; see
/// [`compile_with_commands`].
pub const SYMBOL_BASE: u16 = 0x7a + 4;

// Opcodes, named from the vendor's own table (see `super::bytecode::NAMES`).
const O_INT_ASSIGN: u16 = 0x15;
const O_FLT_ASSIGN: u16 = 0x16;
const O_INT_ADD: u16 = 0x17;
const O_INT_SUB: u16 = 0x18;
const O_INT_MUL: u16 = 0x19;
const O_INT_DIV: u16 = 0x1a;
const O_INT_MOD: u16 = 0x1b;
const O_FLT_ADD: u16 = 0x1c;
const O_FLT_SUB: u16 = 0x1d;
const O_FLT_MUL: u16 = 0x1e;
const O_FLT_DIV: u16 = 0x1f;
const O_FLT_MOD: u16 = 0x20;
const O_INIT_INT: u16 = 0x21;
const O_INIT_FLT: u16 = 0x22;
const O_POP_FOR: u16 = 0x23;
const O_CALL_AS_PROC: u16 = 0x24;
const O_CALL: u16 = 0x25;
const O_INT_PROC: u16 = 0x26;
const O_FLT_PROC: u16 = 0x27;
const O_STR_PROC: u16 = 0x28;
const O_RETURN: u16 = 0x29;
const O_END: u16 = 0x2a;
const O_RETURN_VAR: u16 = 0x2b;
const O_GET_ARG: u16 = 0x2c;
const O_GET_LOCAL: u16 = 0x2d;
const O_ARG1: u16 = 0x2f;
const O_LOCAL1: u16 = 0x34;
const O_STR_ASSIGN: u16 = 0x39;
const O_STR_ADD: u16 = 0x3a;
const O_INIT_STR: u16 = 0x3b;
const O_JUMP: u16 = 0x3c;
const O_JUMP_FALSE: u16 = 0x3d;
const O_INT_ADD_ASSIGN: u16 = 0x3e;
const O_FLT_ADD_ASSIGN: u16 = 0x42;
const O_STR_ADD_ASSIGN: u16 = 0x46;
const O_INT_EQ: u16 = 0x47;
const O_FLT_EQ: u16 = 0x4d;
const O_STR_EQ: u16 = 0x53;
const O_INT_NEG: u16 = 0x59;
const O_INT_NOT: u16 = 0x5a;
const O_FLT_NEG: u16 = 0x5b;
const O_FLT_NOT: u16 = 0x5c;
const O_VOID_PROC: u16 = 0x5d;
const O_MAKE_INT_FOR: u16 = 0x5e;
const O_MAKE_FLT_FOR: u16 = 0x5f;
const O_NEXT: u16 = 0x60;
const O_LOG_AND: u16 = 0x61;
const O_LOG_OR: u16 = 0x62;
const O_BIT_AND: u16 = 0x64;
const O_BIT_OR: u16 = 0x65;
const O_BIT_XOR: u16 = 0x66;
const O_ARR_INDEX: u16 = 0x6b;
const O_RESIZE_ARRAY: u16 = 0x71;
const O_HALT: u16 = 0x79;

/// The type a name's suffix gives it: `%` integer, `$` string, nothing float.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Str,
}

impl Ty {
    fn of(name: &str) -> Ty {
        match name.as_bytes().last() {
            Some(b'%') => Ty::Int,
            Some(b'$') => Ty::Str,
            _ => Ty::Float,
        }
    }

    fn assign_op(self) -> u16 {
        match self {
            Ty::Int => O_INT_ASSIGN,
            Ty::Float => O_FLT_ASSIGN,
            Ty::Str => O_STR_ASSIGN,
        }
    }

    /// The compound-assignment opcode for this type and operator. The three
    /// families are laid out four apart: int, then float, then string with only
    /// `+=`.
    fn compound_op(self, op: &str) -> Option<u16> {
        let step = match op {
            "+" => 0,
            "-" => 1,
            "*" => 2,
            "/" => 3,
            _ => return None,
        };
        Some(match self {
            Ty::Int => O_INT_ADD_ASSIGN + step,
            Ty::Float => O_FLT_ADD_ASSIGN + step,
            Ty::Str if step == 0 => O_STR_ADD_ASSIGN,
            Ty::Str => return None,
        })
    }

    fn init_op(self) -> u16 {
        match self {
            Ty::Int => O_INIT_INT,
            Ty::Float => O_INIT_FLT,
            Ty::Str => O_INIT_STR,
        }
    }

    fn neg_op(self) -> u16 {
        match self {
            Ty::Int => O_INT_NEG,
            _ => O_FLT_NEG,
        }
    }

    fn not_op(self) -> u16 {
        match self {
            Ty::Int => O_INT_NOT,
            _ => O_FLT_NOT,
        }
    }
}

/// A compile failure, with the vendor's own wording where it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError(pub String);

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A value the generator can fold, with the type it folds to.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(String),
}

impl Const {
    fn ty(&self) -> Ty {
        match self {
            Const::Int(_) => Ty::Int,
            Const::Float(_) => Ty::Float,
            Const::Str(_) => Ty::Str,
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Const::Int(v) => *v != 0,
            Const::Float(v) => *v != 0.0,
            Const::Str(s) => !s.is_empty(),
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            Const::Int(v) => Some(*v as f64),
            Const::Float(v) => Some(*v),
            Const::Str(_) => None,
        }
    }
}

/// A global variable or constant, as the engine has to create it.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalInfo {
    pub name: String,
    pub ty: Ty,
    /// The size of each dimension, empty for a scalar. `None` where it did not fold.
    pub sizes: Vec<Option<i64>>,
    /// A constant's value, or a variable's initial value when it folded.
    pub init: Option<Const>,
    pub is_const: bool,
    /// A routine rather than a variable.
    pub is_routine: bool,
}

/// A parameter of a routine.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub name: String,
    pub ty: Ty,
    pub by_ref: bool,
    pub array: bool,
}

/// A `proc` or `func`: where it starts and what its frame holds.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineInfo {
    pub name: String,
    /// The word its callers use.
    pub word: u16,
    /// The index of its first word in the code.
    pub entry: usize,
    /// What it returns, if it is a `func`.
    pub ret: Option<Ty>,
    pub params: Vec<ParamInfo>,
    /// Its local variables, in declaration order.
    pub locals: Vec<(String, Ty)>,
}

/// What was compiled: the words, and the tables those words index.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// One per symbol, in word order.
    pub globals: Vec<GlobalInfo>,
    pub routines: Vec<RoutineInfo>,
    pub code: Vec<u16>,
    /// The word the first symbol occupies.
    pub first_word: u16,
    /// Symbol names in word order, from `first_word` up.
    pub symbols: Vec<String>,
    /// The string pool, in word order, after the symbols.
    pub strings: Vec<String>,
    /// The integer pool, after the strings.
    pub ints: Vec<i64>,
    /// The float pool, after the integers.
    pub floats: Vec<f64>,
}

impl Compiled {
    /// The word a symbol occupies, for a test or a disassembly.
    pub fn word_of(&self, name: &str) -> Option<u16> {
        self.symbols
            .iter()
            .position(|n| n == name)
            .map(|i| self.first_word + i as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Var,
    Proc,
    /// A `func`, with the type its name's suffix gives it.
    Func(Ty),
}

#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    ty: Ty,
    kind: Kind,
    /// The value of a `const`, folded at its declaration.
    value: Option<Const>,
    /// How many dimensions it was declared with.
    dims: usize,
    /// The size of each dimension, when it folds to a constant.
    sizes: Vec<Option<i64>>,
    /// The initial value of a variable whose initialiser folds, which is why such a
    /// declaration emits no code.
    init: Option<Const>,
}

impl Symbol {
    fn is_array(&self) -> bool {
        self.dims > 0
    }
}

#[derive(Debug, Clone)]
struct Param {
    name: String,
    ty: Ty,
    by_ref: bool,
    array: bool,
}

#[derive(Debug, Clone)]
struct Routine {
    name: String,
    kind: Kind,
    args: Vec<Param>,
}

#[derive(Debug, Clone, Default)]
struct Frame {
    args: Vec<Param>,
    locals: Vec<(String, Ty)>,
}

/// One enclosing loop, for `break` and `continue`.
#[derive(Default)]
struct Loop {
    /// Operand positions of jumps that `break` wants landing after the loop.
    breaks: Vec<usize>,
    /// Operand positions of jumps that `continue` wants landing on the test or `Next`.
    continues: Vec<usize>,
    /// A `for` loop, whose `break` needs `PopFor` first.
    is_for: bool,
}

struct Gen {
    code: Vec<u16>,
    /// Constants whose word is not settled until every pool is sorted.
    pending: Vec<(usize, Const)>,
    symbols: Vec<Symbol>,
    routines: Vec<Routine>,
    frame: Option<Frame>,
    routine: Option<usize>,
    loops: Vec<Loop>,
    /// Whether the command table is registered, as it is when Spike2 compiles.
    commands: bool,
    done_routines: Vec<RoutineInfo>,
}

/// Compile with no commands registered: a bare compiler, as the vendor's is when nothing
/// has loaded `runtime.s2l`. Symbols start at [`SYMBOL_BASE`].
pub fn compile(src: &str) -> Result<Compiled, CompileError> {
    compile_impl(src, false)
}

/// Compile as Spike2 does, with every command in [`super::commands`] registered. A
/// command takes the word `0x7a` plus its place in the table, so the script's own
/// symbols start after the last of them.
pub fn compile_with_commands(src: &str) -> Result<Compiled, CompileError> {
    compile_impl(src, true)
}

fn compile_impl(src: &str, commands: bool) -> Result<Compiled, CompileError> {
    let stmts = super::parser::parse(src).map_err(|e| CompileError(e.to_string()))?;
    let mut g = Gen {
        code: Vec::new(),
        pending: Vec::new(),
        symbols: Vec::new(),
        routines: Vec::new(),
        frame: None,
        routine: None,
        loops: Vec::new(),
        commands,
        done_routines: Vec::new(),
    };
    collect(&mut g, &stmts)?;

    gen_block(&mut g, &stmts, true)?;
    g.emit(O_HALT);

    // With no routines the program ends at that `Halt`; with routines, one more
    // follows the last `End`.
    let n = g.routines.len();
    for i in 0..n {
        let r = g.routines[i].clone();
        g.frame = Some(Frame { args: r.args.clone(), locals: Vec::new() });
        g.routine = Some(i);
        let entry = g.code.len();
        g.emit(match r.kind {
            Kind::Func(Ty::Int) => O_INT_PROC,
            Kind::Func(Ty::Str) => O_STR_PROC,
            Kind::Func(Ty::Float) => O_FLT_PROC,
            _ => O_VOID_PROC,
        });
        if let Some(body) = routine_body(&stmts, &r.name) {
            gen_block(&mut g, body, false)?;
        }
        g.emit(O_END);
        let locals = g.frame.take().map(|f| f.locals).unwrap_or_default();
        let word = g.base() + g.index_of(&r.name).unwrap() as u16;
        g.done_routines.push(RoutineInfo {
            name: r.name.clone(),
            word,
            entry,
            ret: match r.kind {
                Kind::Func(t) => Some(t),
                _ => None,
            },
            params: r
                .args
                .iter()
                .map(|a| ParamInfo { name: a.name.clone(), ty: a.ty, by_ref: a.by_ref, array: a.array })
                .collect(),
            locals,
        });
        g.routine = None;
    }
    if n > 0 {
        g.emit(O_HALT);
    }

    Ok(finish(g))
}

/// Settle the pools and patch every constant word.
fn finish(mut g: Gen) -> Compiled {
    let mut strings: Vec<String> = Vec::new();
    let mut ints: Vec<i64> = Vec::new();
    let mut floats: Vec<f64> = Vec::new();
    for (_, c) in &g.pending {
        match c {
            Const::Str(s) if !strings.contains(s) => strings.push(s.clone()),
            Const::Int(v) if !ints.contains(v) => ints.push(*v),
            Const::Float(v) if !floats.iter().any(|f| f == v) => floats.push(*v),
            _ => {}
        }
    }
    strings.sort();
    ints.sort();
    floats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let first_word = g.base();
    let s_base = first_word + g.symbols.len() as u16;
    let i_base = s_base + strings.len() as u16;
    let f_base = i_base + ints.len() as u16;
    for (at, c) in &g.pending {
        g.code[*at] = match c {
            Const::Str(s) => s_base + strings.iter().position(|x| x == s).unwrap() as u16,
            Const::Int(v) => i_base + ints.iter().position(|x| x == v).unwrap() as u16,
            Const::Float(v) => {
                f_base + floats.iter().position(|x| x == v).unwrap() as u16
            }
        };
    }
    let globals = g
        .symbols
        .iter()
        .map(|s| GlobalInfo {
            name: s.name.clone(),
            ty: s.ty,
            sizes: s.sizes.clone(),
            init: if s.value.is_some() { s.value.clone() } else { s.init.clone() },
            is_const: s.value.is_some(),
            is_routine: matches!(s.kind, Kind::Proc | Kind::Func(_)),
        })
        .collect();
    Compiled {
        globals,
        routines: g.done_routines,
        code: g.code,
        first_word,
        symbols: g.symbols.iter().map(|s| s.name.clone()).collect(),
        strings,
        ints,
        floats,
    }
}

/// Number the symbols: variables and constants in source order, then the routines.
fn collect(g: &mut Gen, stmts: &[Stmt]) -> Result<(), CompileError> {
    for s in stmts {
        let is_const = matches!(s, Stmt::Const(_));
        if let Stmt::Var(ds) | Stmt::Const(ds) = s {
            for d in ds {
                if g.symbols.iter().any(|x| x.name == d.name) {
                    return Err(CompileError(format!("duplicate name {}", d.name)));
                }
                let value = if is_const {
                    let lookup = const_lookup(&g.symbols);
                    fold(&d.init.clone().unwrap_or(Expr::Int(0)), &lookup)?
                } else {
                    None
                };
                let (sizes, init) = {
                    let lookup = const_lookup(&g.symbols);
                    let sizes = d
                        .dims
                        .iter()
                        .map(|e| fold(e, &lookup).map(|c| c.and_then(|c| c.num()).map(|n| n as i64)))
                        .collect::<Result<Vec<_>, _>>()?;
                    let init = match (&d.init, is_const) {
                        (Some(e), false) => fold(e, &lookup)?,
                        _ => None,
                    };
                    (sizes, init)
                };
                g.symbols.push(Symbol {
                    name: d.name.clone(),
                    ty: Ty::of(&d.name),
                    kind: Kind::Var,
                    value,
                    dims: d.dims.len(),
                    sizes,
                    init,
                });
            }
        }
    }
    for s in stmts {
        let (name, params, kind) = match s {
            Stmt::Proc(n, p, _) => (n, p, Kind::Proc),
            Stmt::Func(n, p, _) => (n, p, Kind::Func(Ty::of(n))),
            _ => continue,
        };
        if g.symbols.iter().any(|x| x.name == *name) {
            return Err(CompileError(format!("duplicate name {name}")));
        }
        if g.commands && super::commands::find(name).is_some() {
            return Err(CompileError(format!("reuse of built-in script function name {name}")));
        }
        let args = params
            .iter()
            .map(|p| {
                let by_ref = p.starts_with('&');
                let p = p.trim_start_matches('&');
                let array = p.ends_with("[]");
                let bare = p.trim_end_matches("[]");
                Param { name: bare.to_string(), ty: Ty::of(bare), by_ref, array }
            })
            .collect();
        let ty = match kind {
            Kind::Func(t) => t,
            _ => Ty::Float,
        };
        g.symbols.push(Symbol { name: name.clone(), ty, kind, value: None, dims: 0, sizes: Vec::new(), init: None });
        g.routines.push(Routine { name: name.clone(), kind, args });
    }
    Ok(())
}

fn routine_body<'a>(stmts: &'a [Stmt], name: &str) -> Option<&'a [Stmt]> {
    stmts.iter().find_map(|s| match s {
        Stmt::Proc(n, _, b) | Stmt::Func(n, _, b) if n == name => Some(&b[..]),
        _ => None,
    })
}

fn const_lookup(syms: &[Symbol]) -> impl Fn(&str) -> Option<Const> + '_ {
    move |name: &str| syms.iter().find(|s| s.name == name).and_then(|s| s.value.clone())
}

/// Apply a binary operator to two folded values, or report the vendor's error.
fn apply_binary(op: &str, l: &Const, r: &Const) -> Result<Option<Const>, CompileError> {
    use Const::*;
    let v = match (op, l, r) {
        ("+", Str(a), Str(b)) => Some(Str(format!("{a}{b}"))),
        ("band", Int(a), Int(b)) => Some(Int(a & b)),
        ("bor", Int(a), Int(b)) => Some(Int(a | b)),
        ("bxor", Int(a), Int(b)) => Some(Int(a ^ b)),
        // A folded logical operator is logical, whatever the run-time opcode is.
        ("and", a, b) => Some(Int((a.truthy() && b.truthy()) as i64)),
        ("or", a, b) => Some(Int((a.truthy() || b.truthy()) as i64)),
        ("xor", a, b) => Some(Int((a.truthy() != b.truthy()) as i64)),
        // Integer folding is checked: the vendor reports an overflow at compile
        // time rather than wrapping, and the same for a division by zero.
        (op, Int(a), Int(b)) => {
            let v = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" => a.checked_div(*b),
                "mod" => a.checked_rem(*b),
                _ => return Ok(None),
            };
            match v {
                Some(v) => Some(Int(v)),
                None => return Err(CompileError("Integer Overflow".into())),
            }
        }
        (op, a, b) => match (a.num(), b.num()) {
            (Some(a), Some(b)) => {
                if matches!(op, "/" | "mod") && b == 0.0 {
                    return Err(CompileError("Integer Overflow".into()));
                }
                Some(Float(match op {
                    "+" => a + b,
                    "-" => a - b,
                    "*" => a * b,
                    "/" => a / b,
                    "mod" => a % b,
                    _ => return Ok(None),
                }))
            }
            _ => return Ok(None),
        },
    };
    Ok(v)
}

fn apply_compare(op: &str, l: &Const, r: &Const) -> Option<Const> {
    use Const::*;
    let b = match (l, r) {
        (Str(a), Str(b)) => match op {
            "=" => a == b,
            "<>" => a != b,
            ">" => a > b,
            ">=" => a >= b,
            "<" => a < b,
            "<=" => a <= b,
            _ => return None,
        },
        _ => {
            let (a, b) = (l.num()?, r.num()?);
            match op {
                "=" => a == b,
                "<>" => a != b,
                ">" => a > b,
                ">=" => a >= b,
                "<" => a < b,
                "<=" => a <= b,
                _ => return None,
            }
        }
    };
    Some(Int(b as i64))
}

/// Fold an expression as the generator would, or `None` where it would emit code.
///
/// A binary node folds when both sides fold, and a comparison folds through
/// [`apply_compare`]. A unary node folds only when its operand is a bare literal,
/// which is why `not 1` is the constant 0 while `not (2-1)` is a constant word
/// followed by the operator. Parentheses stop a unary operator looking through them
/// but do not stop a binary one: `(2+3)*4` is 20.
fn fold(e: &Expr, lookup: &impl Fn(&str) -> Option<Const>) -> Result<Option<Const>, CompileError> {
    let out = match e {
        Expr::Int(v) => Some(Const::Int(*v)),
        Expr::Float(v) => Some(Const::Float(*v)),
        Expr::Str(s) => Some(Const::Str(s.clone())),
        Expr::Paren(inner) => fold(inner, lookup)?,
        Expr::Name(n) => lookup(n),
        Expr::Unary(op, inner) => {
            let bare = match &**inner {
                Expr::Int(_) | Expr::Float(_) | Expr::Str(_) => true,
                Expr::Name(n) => lookup(n).is_some(),
                _ => false,
            };
            if !bare {
                return Ok(None);
            }
            match (op, fold(inner, lookup)?) {
                (&"-", Some(Const::Int(v))) => Some(Const::Int(-v)),
                (&"-", Some(Const::Float(v))) => Some(Const::Float(-v)),
                (&"not", Some(c)) => Some(Const::Int(!c.truthy() as i64)),
                _ => None,
            }
        }
        Expr::Binary(op, l, r) => {
            let (Some(l), Some(r)) = (fold(l, lookup)?, fold(r, lookup)?) else {
                return Ok(None);
            };
            if matches!(*op, "=" | "<>" | "<" | ">" | "<=" | ">=") {
                apply_compare(op, &l, &r)
            } else {
                match apply_binary(op, &l, &r)? {
                    Some(v) => Some(v),
                    None => return Ok(None),
                }
            }
        }
        _ => None,
    };
    Ok(out)
}

impl Gen {
    /// The word the first symbol occupies.
    fn base(&self) -> u16 {
        SYMBOL_BASE + if self.commands { super::commands::count() as u16 } else { 0 }
    }

    fn emit(&mut self, w: u16) -> usize {
        self.code.push(w);
        self.code.len() - 1
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn jump_op(&mut self, op: u16) -> usize {
        self.emit(op);
        self.emit(0)
    }

    /// Point the jump whose operand sits at `at` at `target`.
    fn patch_to(&mut self, at: usize, target: usize) -> Result<(), CompileError> {
        // The vendor's jump counts from the word after its own operand.
        let off = target as i64 - (at as i64 + 1);
        if !(-32768..=32767).contains(&off) {
            return Err(CompileError("code block too big".into()));
        }
        self.code[at] = off as i16 as u16;
        Ok(())
    }

    fn patch_here(&mut self, at: usize) -> Result<(), CompileError> {
        let target = self.here();
        self.patch_to(at, target)
    }

    /// Write one constant word: its own opcode when it is small enough to be one,
    /// else a pool entry whose number is patched once the pools are sorted.
    fn emit_const(&mut self, c: &Const) -> Ty {
        match c {
            Const::Int(v) if (0..=20).contains(v) => {
                self.emit(*v as u16);
            }
            Const::Float(v) if *v == v.trunc() && (0.0..=20.0).contains(v) => {
                self.emit(*v as u16);
            }
            _ => {
                let at = self.emit(0);
                self.pending.push((at, c.clone()));
            }
        }
        c.ty()
    }

    fn fold(&self, e: &Expr) -> Result<Option<Const>, CompileError> {
        fold(e, &const_lookup(&self.symbols))
    }

    /// The element type of an array name: a declared array, or an array parameter
    /// of the routine being generated.
    fn array_ty(&self, name: &str) -> Result<Ty, CompileError> {
        if let Some(f) = &self.frame {
            if let Some(p) = f.args.iter().find(|a| a.name == name) {
                if p.array {
                    return Ok(p.ty);
                }
                return Err(CompileError(format!("{name} is not an array")));
            }
        }
        let s = self.symbol(name)?;
        if !s.is_array() {
            return Err(CompileError(format!("{name} is not an array")));
        }
        Ok(s.ty)
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.symbols.iter().position(|s| s.name == name)
    }

    fn symbol(&self, name: &str) -> Result<&Symbol, CompileError> {
        self.index_of(name)
            .map(|i| &self.symbols[i])
            .ok_or_else(|| CompileError(format!("unknown name {name}")))
    }

    /// A reference to a name: a frame slot inside a routine, else its symbol word.
    fn emit_ref(&mut self, name: &str) -> Result<u16, CompileError> {
        if let Some(f) = self.frame.clone() {
            if let Some(i) = f.args.iter().position(|a| a.name == name) {
                if i < 5 {
                    let w = O_ARG1 + i as u16;
                    self.emit(w);
                    return Ok(w);
                }
                self.emit(O_GET_ARG);
                self.emit((i + 1) as u16);
                return Ok(O_GET_ARG);
            }
            if let Some(j) = f.locals.iter().position(|(n, _)| n == name) {
                if j < 5 {
                    let w = O_LOCAL1 + j as u16;
                    self.emit(w);
                    return Ok(w);
                }
                self.emit(O_GET_LOCAL);
                self.emit((j + 1) as u16);
                return Ok(O_GET_LOCAL);
            }
        }
        let i = self
            .index_of(name)
            .ok_or_else(|| CompileError(format!("unknown name {name}")))?;
        let w = self.base() + i as u16;
        self.emit(w);
        Ok(w)
    }
}

/// Statements, reporting whether one of them ended the block with `break` or
/// `continue`. Code after `halt` is kept, because the vendor keeps it.
fn gen_block(g: &mut Gen, stmts: &[Stmt], top: bool) -> Result<bool, CompileError> {
    for s in stmts {
        if gen_stmt(g, s, top)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn gen_stmt(g: &mut Gen, s: &Stmt, top: bool) -> Result<bool, CompileError> {
    match s {
        Stmt::Var(ds) => {
            for d in ds {
                if !top {
                    if let Some(f) = &mut g.frame {
                        f.locals.push((d.name.clone(), Ty::of(&d.name)));
                    }
                }
                // An initialiser is an assignment unless it folds to a constant,
                // which is why `var a% := 25` emits nothing and `var a% := b%`
                // emits the store.
                if let Some(init) = &d.init {
                    if g.fold(init)?.is_none() {
                        let lhs = Expr::Name(d.name.clone());
                        let ty = gen_ref(g, &lhs)?;
                        gen_expr(g, init)?;
                        g.emit(ty.assign_op());
                    }
                }
            }
            Ok(false)
        }
        Stmt::Const(_) => Ok(false),
        Stmt::Proc(..) | Stmt::Func(..) => Ok(false),
        Stmt::Halt => {
            g.emit(O_HALT);
            Ok(false)
        }
        Stmt::Keyword("break") => {
            let Some(l) = g.loops.last() else {
                return Err(CompileError("break outside a loop".into()));
            };
            if l.is_for {
                g.emit(O_POP_FOR);
            }
            let at = g.jump_op(O_JUMP);
            g.loops.last_mut().unwrap().breaks.push(at);
            Ok(true)
        }
        Stmt::Keyword("continue") => {
            if g.loops.is_empty() {
                return Err(CompileError("continue outside a loop".into()));
            }
            let at = g.jump_op(O_JUMP);
            g.loops.last_mut().unwrap().continues.push(at);
            Ok(true)
        }
        Stmt::Keyword(k) => Err(CompileError(format!("unsupported statement {k}"))),
        Stmt::Return(v) => {
            if let Some(e) = v {
                let ty = match g.routine.and_then(|i| g.routines.get(i)).map(|r| r.kind) {
                    Some(Kind::Func(t)) => t,
                    _ => return Err(CompileError("a proc has no value".into())),
                };
                g.emit(O_RETURN_VAR);
                gen_expr(g, e)?;
                g.emit(ty.assign_op());
            }
            g.emit(O_RETURN);
            Ok(false)
        }
        Stmt::Assign(lhs, rhs) => {
            let ty = gen_ref(g, lhs)?;
            gen_expr(g, rhs)?;
            g.emit(ty.assign_op());
            Ok(false)
        }
        Stmt::CompoundAssign(lhs, op, rhs) => {
            let ty = gen_ref(g, lhs)?;
            gen_expr(g, rhs)?;
            let code = ty
                .compound_op(op)
                .ok_or_else(|| CompileError(format!("bad compound assignment {op}=")))?;
            g.emit(code);
            Ok(false)
        }
        Stmt::Resize(target, dims) => {
            let Expr::Name(name) = target else {
                return Err(CompileError("resize needs an array name".into()));
            };
            g.emit_ref(name)?;
            for d in dims {
                gen_expr(g, d)?;
            }
            g.emit(O_RESIZE_ARRAY);
            // One mask bit per dimension, three apart: 1, 9, 73.
            let mut mask = 0u16;
            for i in 0..dims.len() {
                mask |= 1 << (3 * i);
            }
            g.emit(mask);
            Ok(false)
        }
        Stmt::Expr(e) => {
            if let Expr::Call(callee, args) = e {
                if let Expr::Name(name) = &**callee {
                    if g.commands && g.index_of(name).is_none() {
                        if let Some((i, cmd)) = super::commands::find(name) {
                            gen_command(g, i, &cmd, args, true)?;
                            return Ok(false);
                        }
                    }
                    let kind = g.symbol(name)?.kind;
                    if matches!(kind, Kind::Proc | Kind::Func(_)) && !g.routines.iter().any(|r| &r.name == name) {
                        return Err(CompileError(format!("unknown command {name}")));
                    }
                    if matches!(kind, Kind::Proc | Kind::Func(_)) {
                        gen_args(g, name, args)?;
                        g.emit(args.len() as u16);
                        let w = g.base() + g.index_of(name).unwrap() as u16;
                        g.emit(w);
                        g.emit(if matches!(kind, Kind::Func(_)) { O_CALL_AS_PROC } else { O_CALL });
                        return Ok(false);
                    }
                }
            }
            gen_expr(g, e)?;
            Ok(false)
        }
        Stmt::If(cond, then, otherwise) => {
            if let Some(c) = g.fold(cond)? {
                // A constant condition leaves only the branch it takes.
                return gen_block(g, if c.truthy() { then } else { otherwise }, top);
            }
            gen_expr(g, cond)?;
            let jf = g.jump_op(O_JUMP_FALSE);
            gen_block(g, then, top)?;
            if otherwise.is_empty() {
                g.patch_here(jf)?;
            } else {
                let jend = g.jump_op(O_JUMP);
                g.patch_here(jf)?;
                gen_block(g, otherwise, top)?;
                g.patch_here(jend)?;
            }
            Ok(false)
        }
        Stmt::While(cond, body) => {
            let start = g.here();
            let constant = g.fold(cond)?;
            if matches!(&constant, Some(c) if !c.truthy()) {
                return Ok(false);
            }
            let jf = if constant.is_none() {
                gen_expr(g, cond)?;
                Some(g.jump_op(O_JUMP_FALSE))
            } else {
                None
            };
            g.loops.push(Loop::default());
            gen_block(g, body, top)?;
            let back = g.jump_op(O_JUMP);
            g.patch_to(back, start)?;
            if let Some(jf) = jf {
                g.patch_here(jf)?;
            }
            let l = g.loops.pop().unwrap();
            for at in l.continues {
                g.patch_to(at, start)?;
            }
            let end = g.here();
            for at in l.breaks {
                g.patch_to(at, end)?;
            }
            Ok(false)
        }
        Stmt::Repeat(body, cond) => {
            let start = g.here();
            g.loops.push(Loop::default());
            gen_block(g, body, top)?;
            // `continue` in a `repeat` lands on the test.
            let test = g.here();
            let constant = g.fold(cond)?;
            let jf = match &constant {
                Some(c) if c.truthy() => None,
                _ => {
                    gen_expr(g, cond)?;
                    Some(g.jump_op(O_JUMP_FALSE))
                }
            };
            let l = g.loops.pop().unwrap();
            for at in l.continues {
                g.patch_to(at, test)?;
            }
            if let Some(jf) = jf {
                g.patch_to(jf, start)?;
            }
            let end = g.here();
            for at in l.breaks {
                g.patch_to(at, end)?;
            }
            Ok(false)
        }
        Stmt::For { var, from, to, step, body } => {
            let ty = gen_ref(g, var)?;
            gen_expr(g, from)?;
            gen_expr(g, to)?;
            match step {
                Some(s) => {
                    gen_expr(g, s)?;
                }
                None => {
                    let one = if ty == Ty::Int { Const::Int(1) } else { Const::Float(1.0) };
                    g.emit_const(&one);
                }
            }
            g.emit(if ty == Ty::Int { O_MAKE_INT_FOR } else { O_MAKE_FLT_FOR });
            let skip = g.emit(0);
            g.loops.push(Loop { is_for: true, ..Loop::default() });
            gen_block(g, body, top)?;
            let next = g.here();
            g.emit(O_NEXT);
            // The operand skips the body and its `Next` when the loop starts past
            // its limit, and counts from the word after itself.
            let after = g.here();
            g.patch_to(skip, after)?;
            let l = g.loops.pop().unwrap();
            for at in l.continues {
                g.patch_to(at, next)?;
            }
            let end = g.here();
            for at in l.breaks {
                g.patch_to(at, end)?;
            }
            Ok(false)
        }
        Stmt::Case(arms, otherwise) => {
            // Every arm but the last is followed by a jump past the whole `docase`.
            // The last arm, when there is no `else`, falls through to the end on a
            // false test and needs no jump of its own.
            let mut ends = Vec::new();
            let last = arms.len().saturating_sub(1);
            let mut tail = None;
            for (i, (cond, body)) in arms.iter().enumerate() {
                gen_expr(g, cond)?;
                let jf = g.jump_op(O_JUMP_FALSE);
                if g.fold(cond)?.map(|c| c.truthy()).unwrap_or(true) {
                    gen_block(g, body, top)?;
                }
                if i == last && otherwise.is_empty() {
                    tail = Some(jf);
                } else {
                    ends.push(g.jump_op(O_JUMP));
                    g.patch_here(jf)?;
                }
            }
            gen_block(g, otherwise, top)?;
            for at in ends {
                g.patch_here(at)?;
            }
            if let Some(at) = tail {
                g.patch_here(at)?;
            }
            Ok(false)
        }
    }
}

/// Emit a reference to an assignment target and return its type.
fn gen_ref(g: &mut Gen, e: &Expr) -> Result<Ty, CompileError> {
    match e {
        Expr::Name(name) => {
            if let Some(f) = g.frame.clone() {
                if let Some(a) = f.args.iter().find(|a| &a.name == name) {
                    let ty = a.ty;
                    g.emit_ref(name)?;
                    return Ok(ty);
                }
                if let Some((_, ty)) = f.locals.iter().find(|(n, _)| n == name) {
                    let ty = *ty;
                    g.emit_ref(name)?;
                    return Ok(ty);
                }
            }
            let s = g.symbol(name)?.clone();
            if s.is_array() {
                return Err(CompileError(format!("{name} is an array")));
            }
            if s.value.is_some() {
                return Err(CompileError(format!("{name} is a constant")));
            }
            g.emit_ref(name)?;
            Ok(s.ty)
        }
        Expr::Index(..) => Ok(gen_indices(g, e)?.0),
        other => Err(CompileError(format!("bad assignment target {other:?}"))),
    }
}

/// Emit an array reference — name, indices, `ArrNIndex` — and return its type.
fn gen_indices(g: &mut Gen, e: &Expr) -> Result<(Ty, usize), CompileError> {
    let mut idxs: Vec<&Expr> = Vec::new();
    let mut cur = e;
    loop {
        match cur {
            Expr::Index(base, arg) => {
                match arg {
                    IndexArg::One(i) => idxs.push(i),
                    _ => return Err(CompileError("a slice cannot be used here".into())),
                }
                cur = base;
            }
            Expr::Name(name) => {
                let ty = g.array_ty(name)?;
                g.emit_ref(name)?;
                for i in idxs.iter().rev() {
                    gen_expr(g, i)?;
                }
                g.emit(O_ARR_INDEX + idxs.len() as u16 - 1);
                return Ok((ty, idxs.len()));
            }
            other => return Err(CompileError(format!("bad index base {other:?}"))),
        }
    }
}

fn gen_args(g: &mut Gen, name: &str, args: &[Expr]) -> Result<(), CompileError> {
    let params = g
        .routines
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.args.clone())
        .ok_or_else(|| CompileError(format!("unknown command {name}")))?;
    if params.len() != args.len() {
        return Err(CompileError(format!(
            "{name} takes {} arguments, given {}",
            params.len(),
            args.len()
        )));
    }
    for (p, a) in params.iter().zip(args) {
        if p.by_ref {
            gen_ref(g, a)?;
            continue;
        }
        // A global passed by value is copied into the frame first; a literal, a
        // computed value and a frame slot are already values.
        if let Expr::Name(n) = a {
            let in_frame = g
                .frame
                .as_ref()
                .map(|f| {
                    f.args.iter().any(|x| &x.name == n) || f.locals.iter().any(|(l, _)| l == n)
                })
                .unwrap_or(false);
            let s = g.symbol(n)?.clone();
            if !in_frame && !s.is_array() && s.value.is_none() {
                g.emit_ref(n)?;
                g.emit(s.ty.init_op());
                continue;
            }
        }
        gen_expr(g, a)?;
    }
    Ok(())
}

/// Emit code leaving one value on the stack, and return its type.
fn gen_expr(g: &mut Gen, e: &Expr) -> Result<Ty, CompileError> {
    // A whole array passed as an argument is its symbol alone.
    if let Some((name, _)) = whole_array(e) {
        let ty = g.array_ty(name)?;
        g.emit_ref(name)?;
        return Ok(ty);
    }

    if let Expr::Call(callee, args) = e {
        if let Expr::Name(name) = &**callee {
            if g.commands && g.index_of(name).is_none() {
                if let Some((i, cmd)) = super::commands::find(name) {
                    return match gen_command(g, i, &cmd, args, false)? {
                        Some(t) => Ok(t),
                        None => Err(CompileError(format!("{name} has no value"))),
                    };
                }
            }
            if g.routines.iter().any(|r| &r.name == name) {
                gen_args(g, name, args)?;
                g.emit(args.len() as u16);
                let w = g.base() + g.index_of(name).unwrap() as u16;
                g.emit(w);
                g.emit(O_CALL);
                return match g.symbol(name)?.kind {
                    Kind::Func(t) => Ok(t),
                    _ => Err(CompileError(format!("{name} has no value"))),
                };
            }
        }
    }

    if matches!(e, Expr::Index(..)) {
        return Ok(gen_indices(g, e)?.0);
    }

    if let Some(c) = g.fold(e)? {
        return Ok(g.emit_const(&c));
    }

    match e {
        Expr::Paren(inner) => gen_expr(g, inner),
        Expr::Name(name) => {
            // An argument or a local is a value of its own type; only a global
            // needs the table.
            if let Some(f) = g.frame.clone() {
                if let Some(a) = f.args.iter().find(|a| &a.name == name) {
                    let ty = a.ty;
                    g.emit_ref(name)?;
                    return Ok(ty);
                }
                if let Some((_, ty)) = f.locals.iter().find(|(n, _)| n == name) {
                    let ty = *ty;
                    g.emit_ref(name)?;
                    return Ok(ty);
                }
            }
            let s = g.symbol(name)?.clone();
            if !matches!(s.kind, Kind::Var) || s.is_array() {
                return Err(CompileError(format!("{name} is not a value")));
            }
            g.emit_ref(name)?;
            Ok(s.ty)
        }
        Expr::Unary(op, inner) => {
            let ty = gen_expr(g, inner)?;
            g.emit(match *op {
                "-" => ty.neg_op(),
                _ => ty.not_op(),
            });
            Ok(ty)
        }
        Expr::Binary(op, l, r) => {
            let lt = gen_expr(g, l)?;
            let rt = gen_expr(g, r)?;
            g.emit(binary_op(op, lt, rt)?);
            Ok(match *op {
                "=" | "<>" | "<" | ">" | "<=" | ">=" => Ty::Int,
                "+" if lt == Ty::Str && rt == Ty::Str => Ty::Str,
                _ if lt == Ty::Int && rt == Ty::Int => Ty::Int,
                _ => Ty::Float,
            })
        }
        Expr::Ternary(c, a, b) => {
            if let Some(c) = g.fold(c)? {
                return gen_expr(g, if c.truthy() { a } else { b });
            }
            gen_expr(g, c)?;
            let jf = g.jump_op(O_JUMP_FALSE);
            let ta = gen_expr(g, a)?;
            let jend = g.jump_op(O_JUMP);
            g.patch_here(jf)?;
            let tb = gen_expr(g, b)?;
            g.patch_here(jend)?;
            Ok(if ta == Ty::Int && tb == Ty::Int { Ty::Int } else { Ty::Float })
        }
        other => Err(CompileError(format!("cannot compile {other:?}"))),
    }
}

/// What the vendor's matcher asks of an argument: how many array dimensions it names,
/// the type of its value (1 integer, 2 real, 3 string), and whether it is a variable or
/// an element of one, which is what a reference parameter needs.
#[derive(Debug, Clone, Copy)]
struct ArgInfo {
    rank: usize,
    ty: u8,
    lvalue: bool,
}

/// A procedure or function named as an argument: what it returns (0 nothing, 1 integer,
/// 2 real, 3 string) and the type of each parameter, or 0 for an array.
#[derive(Debug, Clone)]
struct ProcInfo {
    ret: u8,
    params: Vec<u8>,
}

fn ty_code(t: Ty) -> u8 {
    match t {
        Ty::Int => 1,
        Ty::Float => 2,
        Ty::Str => 3,
    }
}

/// A whole array written `a[]`, `a[][]`, …: its name and how many pairs of brackets.
fn whole_array(e: &Expr) -> Option<(&str, usize)> {
    match e {
        Expr::Index(base, IndexArg::Whole) => match &**base {
            Expr::Name(n) => Some((n, 1)),
            inner => whole_array(inner).map(|(n, k)| (n, k + 1)),
        },
        _ => None,
    }
}

/// Is this a variable, or an element of an array, that the script may write to?
fn is_lvalue(g: &Gen, e: &Expr) -> bool {
    match e {
        Expr::Paren(inner) => is_lvalue(g, inner),
        Expr::Name(n) => {
            if let Some(f) = &g.frame {
                if f.args.iter().any(|a| &a.name == n && !a.array)
                    || f.locals.iter().any(|(l, _)| l == n)
                {
                    return true;
                }
            }
            g.symbol(n).map(|s| matches!(s.kind, Kind::Var) && !s.is_array() && s.value.is_none()).unwrap_or(false)
        }
        Expr::Index(..) => {
            let mut cur = e;
            while let Expr::Index(base, arg) = cur {
                if !matches!(arg, IndexArg::One(_)) {
                    return false;
                }
                cur = base;
            }
            matches!(cur, Expr::Name(_))
        }
        _ => false,
    }
}

/// The routine an argument names, if it names one.
fn proc_arg(g: &Gen, e: &Expr) -> Option<ProcInfo> {
    let Expr::Name(n) = e else { return None };
    let r = g.routines.iter().find(|r| &r.name == n)?;
    Some(ProcInfo {
        ret: match r.kind {
            Kind::Func(t) => ty_code(t),
            _ => 0,
        },
        params: r.args.iter().map(|p| if p.array { 0 } else { ty_code(p.ty) }).collect(),
    })
}

/// Compile the argument to learn what it is, then take the code back out.
fn arg_info(g: &mut Gen, e: &Expr) -> Result<ArgInfo, CompileError> {
    if let Some((name, rank)) = whole_array(e) {
        return Ok(ArgInfo { rank, ty: ty_code(g.array_ty(name)?), lvalue: false });
    }
    let (code, pending) = (g.code.len(), g.pending.len());
    let ty = gen_expr(g, e);
    g.code.truncate(code);
    g.pending.truncate(pending);
    Ok(ArgInfo { rank: 0, ty: ty_code(ty?), lvalue: is_lvalue(g, e) })
}

/// Do the dimensions an argument has suit this parameter word? (compiler.dll 0x180017030)
fn dims_fit(word: u16, rank: usize) -> bool {
    let t = word & 0x7f;
    let mut m = word & 0x1f00;
    if rank == 0 {
        return m == 0 && t != 0x12 && t != 0x13;
    }
    if t == 8 {
        m = 0x100;
    } else if t == 4 || t == 0xc || (0x11..0x14).contains(&t) {
        m = 0x1f00;
    } else if t == 0x10 {
        m = 0x300;
    }
    m & (0x100 << (rank - 1)) != 0
}

/// Do the elements of an array suit this parameter word? (compiler.dll 0x18002d610)
fn elem_fits(word: u16, ty: u8) -> bool {
    match word & 0x7f {
        9 | 1 => ty == 1,
        2 | 10 => ty == 2,
        3 | 0xb | 0x13 => ty == 3,
        5 | 7 | 0xe | 0xf => false,
        6 | 8 | 0x10 | 0x12 => ty == 1 || ty == 2,
        _ => true,
    }
}

/// Does one argument suit one parameter word? (compiler.dll 0x180016dc0)
fn arg_fits(word: u16, a: ArgInfo) -> bool {
    if !dims_fit(word, a.rank) {
        return false;
    }
    if a.rank != 0 {
        return elem_fits(word, a.ty) && (!a.lvalue || word & 0x80 != 0);
    }
    // Types 9 to 13 without the 0x80 flag are references, which want a variable.
    if !a.lvalue && word & 0x80 == 0 && (9..14).contains(&word) {
        return false;
    }
    let numeric = a.ty == 1 || a.ty == 2;
    match word {
        10 => a.ty == 2,
        0 => true,
        1 | 2 | 6 | 8 | 9 | 0x10 | 0x11 => numeric,
        3 | 0xb => a.ty == 3,
        w if w > 0x11 => w != 0x13,
        _ => true,
    }
}

/// Does the group of words starting at `v[at]` (which is `0xe`) describe this routine?
/// The word after `0xe` is the return type, the words up to `0xf` the parameters; each
/// must be the routine's own, an integer for 1 and a real for 2, by value or by reference
/// (compiler.dll 0x180016dc0). Returns where the group ends.
fn group_fits(v: &[u16], at: usize, p: &ProcInfo) -> Option<usize> {
    let end = at + v[at..].iter().position(|&w| w == super::commands::GROUP_CLOSE)?;
    let (ret, params) = (v[at + 1], &v[at + 2..end]);
    (ret as u8 == p.ret && params.len() == p.params.len()
        && params.iter().zip(&p.params).all(|(&w, &t)| w as u8 == t))
    .then_some(end)
}

/// The first variant of `cmd` whose argument list the arguments fit, counted from one.
/// An argument list may be longer than the arguments, never shorter.
fn match_variant(
    cmd: &super::commands::Command,
    args: &[ArgInfo],
    procs: &[Option<ProcInfo>],
) -> Result<usize, CompileError> {
    if args.len() < cmd.min_args as usize || args.len() > cmd.max_args as usize {
        return Err(CompileError(format!("{}: wrong number of arguments", cmd.name)));
    }
    'variants: for (i, v) in cmd.variants.iter().enumerate() {
        let mut at = 0;
        for (a, p) in args.iter().zip(procs) {
            let Some(&w) = v.get(at) else { continue 'variants };
            if w == super::commands::GROUP_OPEN {
                match p.as_ref().and_then(|p| group_fits(v, at, p)) {
                    Some(end) => at = end + 1,
                    None => continue 'variants,
                }
            } else if p.is_some() || !arg_fits(w, *a) {
                continue 'variants;
            } else {
                at += 1;
            }
        }
        return Ok(i + 1);
    }
    Err(CompileError(format!("{}: wrong number of arguments or argument type mismatch", cmd.name)))
}

/// A call to a built-in: the arguments, how many there are, which variant of the command
/// they fit when it has more than one, the command's word, and `Call`. A command used as
/// a statement that returns a value ends in `CallAsProc` instead. Returns the type of
/// the value, if there is one.
fn gen_command(
    g: &mut Gen,
    index: usize,
    cmd: &super::commands::Command,
    args: &[Expr],
    as_statement: bool,
) -> Result<Option<Ty>, CompileError> {
    let mut infos = Vec::with_capacity(args.len());
    let mut procs = Vec::with_capacity(args.len());
    for a in args {
        let p = proc_arg(g, a);
        infos.push(if p.is_some() {
            ArgInfo { rank: 0, ty: 0, lvalue: false }
        } else {
            arg_info(g, a)?
        });
        procs.push(p);
    }
    let variant = match_variant(cmd, &infos, &procs)?;
    for (a, p) in args.iter().zip(&procs) {
        match (a, p) {
            (Expr::Name(n), Some(_)) => {
                let w = g.base() + g.index_of(n).unwrap() as u16;
                g.emit(w);
            }
            _ => {
                gen_expr(g, a)?;
            }
        }
    }
    g.emit_const(&Const::Int(args.len() as i64));
    if cmd.variants.len() > 1 {
        g.emit_const(&Const::Int(variant as i64));
    }
    g.emit(super::commands::word(index));
    let ret = match cmd.ret {
        super::commands::Ret::Void => None,
        super::commands::Ret::Int => Some(Ty::Int),
        super::commands::Ret::Real => Some(Ty::Float),
        super::commands::Ret::Str => Some(Ty::Str),
    };
    g.emit(if as_statement && ret.is_some() { O_CALL_AS_PROC } else { O_CALL });
    Ok(ret)
}

fn binary_op(op: &str, l: Ty, r: Ty) -> Result<u16, CompileError> {
    let both_int = l == Ty::Int && r == Ty::Int;
    let both_str = l == Ty::Str && r == Ty::Str;
    Ok(match op {
        "+" if both_str => O_STR_ADD,
        "+" if both_int => O_INT_ADD,
        "-" if both_int => O_INT_SUB,
        "*" if both_int => O_INT_MUL,
        "/" if both_int => O_INT_DIV,
        "mod" if both_int => O_INT_MOD,
        "+" => O_FLT_ADD,
        "-" => O_FLT_SUB,
        "*" => O_FLT_MUL,
        "/" => O_FLT_DIV,
        "mod" => O_FLT_MOD,
        "band" => O_BIT_AND,
        "bor" => O_BIT_OR,
        "bxor" => O_BIT_XOR,
        "and" => O_LOG_AND,
        "or" => O_LOG_OR,
        // The run-time `xor` is bitwise even where the fold was logical.
        "xor" => O_BIT_XOR,
        "=" | "<>" | ">" | ">=" | "<" | "<=" => {
            let idx = match op {
                "=" => 0,
                "<>" => 1,
                ">" => 2,
                ">=" => 3,
                "<" => 4,
                _ => 5,
            };
            if both_str {
                O_STR_EQ + idx
            } else if both_int {
                O_INT_EQ + idx
            } else {
                O_FLT_EQ + idx
            }
        }
        other => return Err(CompileError(format!("unsupported operator {other}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words a source compiles to, or the error it raises.
    fn words(src: &str) -> Vec<u16> {
        match compile(src) {
            Ok(c) => c.code,
            Err(e) => panic!("{src:?} did not compile: {e}"),
        }
    }

    // Each of these was checked against the vendor's own compiler; see
    // `tests/compile_reference.rs` for the harness that checks every case in
    // `tools/compile_cases`. They are repeated here so the rules stay pinned on a
    // machine without CED's DLL.

    #[test]
    fn an_assignment_is_symbol_value_store() {
        assert_eq!(words("var a%;\na% := 5;"), [126, 5, 21, 121]);
    }

    #[test]
    fn a_constant_expression_folds_at_compile_time() {
        assert_eq!(words("var a%;\na% := 7 - 3*2;"), [126, 1, 21, 121]);
    }

    #[test]
    fn the_pools_are_sorted_whatever_the_source_order() {
        let c = compile("var a%;\na% := 400; a% := 300;").unwrap();
        assert_eq!(c.code, [126, 128, 21, 126, 127, 21, 121]);
        assert_eq!(c.ints, [300, 400]);
    }

    #[test]
    fn a_comparison_binds_tighter_than_band() {
        // `b% band c% = d%` is `b% band (c% = d%)`, which is the vendor's grammar.
        assert_eq!(
            words("var a%,b%,c%,d%;\na% := b% band c% = d%;"),
            [126, 127, 128, 129, 71, 100, 21, 121]
        );
    }

    #[test]
    fn a_unary_operator_folds_through_a_literal_but_not_a_parenthesis() {
        // `not 1` is the constant 0; `not (2-1)` is the constant 1 and the operator.
        assert_eq!(
            words("var a%;\na% := not 1; a% := not (2-1);"),
            [126, 0, 21, 126, 1, 90, 21, 121]
        );
    }

    #[test]
    fn a_for_loop_counts_its_body_and_its_next() {
        assert_eq!(
            words("var i%,s%;\nfor i% := 1 to 10 do s% += i% next;"),
            [126, 1, 10, 1, 94, 4, 127, 126, 62, 96, 121]
        );
    }

    #[test]
    fn a_break_in_a_for_loop_pops_the_loop_record_first() {
        assert_eq!(
            words("var a%,i%;\nfor i% := 1 to 3 do if i% = 2 then break endif next;"),
            [127, 1, 3, 1, 94, 9, 127, 2, 71, 61, 3, 35, 60, 1, 96, 121]
        );
    }

    #[test]
    fn a_folded_logical_operator_stays_logical_where_the_opcode_is_bitwise() {
        let c = compile("var a%,b%,c%;\na% := 6 xor 3; a% := b% xor c%;").unwrap();
        // The first is folded and logical (0); the second is the bitwise opcode 102.
        assert_eq!(c.code, [126, 0, 21, 126, 127, 128, 102, 21, 121]);
    }

    #[test]
    fn a_constant_condition_leaves_only_the_branch_it_takes() {
        assert_eq!(
            words("var a%;\nif 0 then a% := 25 endif; a% := 30;"),
            [126, 127, 21, 121]
        );
    }

    #[test]
    fn a_fold_that_overflows_is_a_compile_error() {
        assert_eq!(
            compile("var a%;\na% := 9223372036854775807 + 1;").unwrap_err().0,
            "Integer Overflow"
        );
    }

    #[test]
    fn a_call_the_command_table_owns_is_refused_by_name() {
        let e = compile("var a%;\na% := Abs(-3);").unwrap_err().0;
        assert!(e.contains("Abs"), "{e}");
    }

    // The words below are what compiler.dll produced with the command table registered
    // (tools/call_oracle.py); `smr/tests/call_reference.rs` checks thousands more.
    fn with(src: &str) -> Vec<u16> {
        compile_with_commands(src).unwrap().code
    }

    #[test]
    fn symbols_start_after_the_last_command() {
        let c = compile_with_commands("var a%;\nvar b;").unwrap();
        assert_eq!(c.word_of("a%"), Some(126 + 503));
        assert_eq!(c.word_of("b"), Some(126 + 503 + 1));
    }

    #[test]
    fn a_call_is_its_arguments_their_count_the_command_and_call() {
        // Abs is command 429, so its word is 0x7a + 429 = 551.
        assert_eq!(with("var x;\nx := Abs(1.5);"), [629, 630, 1, 551, 37, 22, 121]);
        // Mid$ has one form: no variant number.
        assert_eq!(with("var s$;\ns$ := Mid$(\"hello\",2,3);"), [629, 630, 2, 3, 3, 516, 37, 57, 121]);
    }

    #[test]
    fn a_command_with_several_forms_says_which_one_the_call_fits() {
        // Len(s$) is its first form; Len on a real array its second, on a string array its third.
        assert_eq!(with("var n%;\nn% := Len(\"abc\");"), [629, 630, 1, 1, 447, 37, 21, 121]);
        assert_eq!(with("var a[3];\nvar n%;\nn% := Len(a[]);"), [630, 629, 1, 2, 447, 37, 21, 121]);
        assert_eq!(with("var a$[3];\nvar n%;\nn% := Len(a$[]);"), [630, 629, 1, 3, 447, 37, 21, 121]);
    }

    #[test]
    fn a_command_used_as_a_statement_discards_its_value() {
        // CallAsProc for one that returns a value, Call for one that returns none.
        assert_eq!(with("Len(\"x\");"), [629, 1, 1, 447, 36, 121]);
        assert_eq!(with("var i%;\nEval(i%);"), [629, 1, 122, 37, 121]);
    }

    #[test]
    fn a_command_that_returns_nothing_cannot_be_assigned() {
        let e = compile_with_commands("var r%;\nr% := Eval(1);").unwrap_err().0;
        assert!(e.contains("Eval"), "{e}");
    }
}
