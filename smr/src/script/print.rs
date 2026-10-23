//! Print an AST back as source, and check the round trip.
//!
//! Not cosmetics: parsing, printing and parsing again must give the same tree. A
//! printer that loses a distinction — precedence, a slice, the difference between
//! `a[1]` and `a[:1]` — shows up as a mismatch, and every mismatch is either a
//! printer bug or a parser one. It is a cheap consistency check on the grammar
//! while the language has no runtime to test against.

use super::parser::{Declarator, Expr, IndexArg, Stmt};

/// Parenthesise by precedence so the printed text re-parses to the same tree.
fn prec(e: &Expr) -> u8 {
    match e {
        // The levels are the vendor compiler's, which is not quite the usual
        // ladder: `bor`/`bxor` sit with `or`, and a comparison binds tighter than
        // `band`.
        Expr::Ternary(..) => 0,
        Expr::Binary(op, ..) => match *op {
            "or" | "xor" | "bor" | "bxor" => 1,
            "and" | "band" => 2,
            "=" | "<>" | "<" | ">" | "<=" | ">=" => 3,
            "+" | "-" => 4,
            "*" | "/" | "mod" => 5,
            _ => 7,
        },
        Expr::Unary(..) => 6,
        _ => 7,
    }
}

pub fn expr(e: &Expr, out: &mut String) {
    match e {
        Expr::Int(v) => out.push_str(&v.to_string()),
        Expr::Float(v) => {
            // Keep a decimal point so it re-lexes as a float, and keep the sign
            // inside the literal only when it is part of the value.
            let s = format!("{v}");
            out.push_str(&s);
            if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("NaN") {
                out.push_str(".0");
            }
        }
        Expr::Str(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    other => out.push(other),
                }
            }
            out.push('"');
        }
        Expr::Name(n) => out.push_str(n),
        Expr::Empty => out.push_str("[]"),
        Expr::Index(base, idx) => {
            expr(base, out);
            out.push('[');
            match idx {
                IndexArg::Whole => {}
                IndexArg::One(i) => expr(i, out),
                IndexArg::From(hi) => {
                    out.push(':');
                    expr(hi, out);
                }
                IndexArg::Range(lo, hi) => {
                    expr(lo, out);
                    out.push(':');
                    expr(hi, out);
                }
            }
            out.push(']');
        }
        Expr::Member(base, name) => {
            expr(base, out);
            out.push('.');
            out.push_str(name);
        }
        Expr::Call(callee, args) => {
            expr(callee, out);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                expr(a, out);
            }
            out.push(')');
        }
        Expr::Unary(op, inner) => {
            out.push_str(op);
            out.push(' ');
            expr_prec(inner, 6, out);
        }
        Expr::Binary(op, l, r) => {
            let p = prec(e);
            expr_prec(l, p, out);
            out.push(' ');
            out.push_str(op);
            out.push(' ');
            expr_prec(r, p + 1, out);
        }
        Expr::Ternary(c, a, b) => {
            expr_prec(c, 1, out);
            out.push_str(" ? ");
            expr_prec(a, 0, out);
            out.push_str(" : ");
            expr_prec(b, 0, out);
        }
        Expr::Paren(inner) => {
            out.push('(');
            expr(inner, out);
            out.push(')');
        }
    }
}

fn expr_prec(e: &Expr, min: u8, out: &mut String) {
    if prec(e) < min {
        out.push('(');
        expr(e, out);
        out.push(')');
    } else {
        expr(e, out);
    }
}

fn declarators(d: &[Declarator], out: &mut String) {
    for (i, x) in d.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&x.name);
        for dim in &x.dims {
            out.push('[');
            expr(dim, out);
            out.push(']');
        }
        if let Some(init) = &x.init {
            out.push_str(" := ");
            expr(init, out);
        }
    }
}

pub fn stmts(list: &[Stmt], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for s in list {
        out.push_str(&pad);
        stmt(s, indent, out);
        out.push('\n');
    }
}

fn stmt(s: &Stmt, indent: usize, out: &mut String) {
    match s {
        Stmt::Var(d) => {
            out.push_str("var ");
            declarators(d, out);
            out.push(';');
        }
        Stmt::Const(d) => {
            out.push_str("const ");
            declarators(d, out);
            out.push(';');
        }
        Stmt::Proc(name, args, body) => {
            out.push_str("proc ");
            out.push_str(name);
            out.push('(');
            out.push_str(&args.join(", "));
            out.push_str(")\n");
            stmts(body, indent + 1, out);
            out.push_str("end;");
        }
        Stmt::Func(name, args, body) => {
            out.push_str("func ");
            out.push_str(name);
            out.push('(');
            out.push_str(&args.join(", "));
            out.push_str(")\n");
            stmts(body, indent + 1, out);
            out.push_str("end;");
        }
        Stmt::Assign(l, r) => {
            expr(l, out);
            out.push_str(" := ");
            expr(r, out);
            out.push(';');
        }
        Stmt::CompoundAssign(l, op, r) => {
            expr(l, out);
            out.push(' ');
            out.push_str(op);
            out.push_str("= ");
            expr(r, out);
            out.push(';');
        }
        Stmt::Resize(target, dims) => {
            out.push_str("resize ");
            expr(target, out);
            for d in dims {
                out.push('[');
                expr(d, out);
                out.push(']');
            }
            out.push(';');
        }
        Stmt::Expr(e) => {
            expr(e, out);
            out.push(';');
        }
        Stmt::If(cond, then, otherwise) => {
            out.push_str("if ");
            expr(cond, out);
            out.push_str(" then\n");
            stmts(then, indent + 1, out);
            if !otherwise.is_empty() {
                out.push_str(&"  ".repeat(indent));
                out.push_str("else\n");
                stmts(otherwise, indent + 1, out);
            }
            out.push_str(&"  ".repeat(indent));
            out.push_str("endif;");
        }
        Stmt::While(cond, body) => {
            out.push_str("while ");
            expr(cond, out);
            out.push_str(" do\n");
            stmts(body, indent + 1, out);
            out.push_str(&"  ".repeat(indent));
            out.push_str("wend;");
        }
        Stmt::Repeat(body, cond) => {
            out.push_str("repeat\n");
            stmts(body, indent + 1, out);
            out.push_str(&"  ".repeat(indent));
            out.push_str("until ");
            expr(cond, out);
            out.push(';');
        }
        Stmt::For {
            var,
            from,
            to,
            step,
            body,
        } => {
            out.push_str("for ");
            expr(var, out);
            out.push_str(" := ");
            expr(from, out);
            out.push_str(" to ");
            expr(to, out);
            if let Some(st) = step {
                out.push_str(" step ");
                expr(st, out);
            }
            out.push_str(" do\n");
            stmts(body, indent + 1, out);
            out.push_str(&"  ".repeat(indent));
            out.push_str("next;");
        }
        Stmt::Case(arms, otherwise) => {
            out.push_str("docase\n");
            for (cond, body) in arms {
                out.push_str(&"  ".repeat(indent + 1));
                out.push_str("case ");
                expr(cond, out);
                out.push_str(" then\n");
                stmts(body, indent + 2, out);
            }
            if !otherwise.is_empty() {
                out.push_str(&"  ".repeat(indent + 1));
                out.push_str("else\n");
                stmts(otherwise, indent + 2, out);
            }
            out.push_str(&"  ".repeat(indent));
            out.push_str("endcase;");
        }
        Stmt::Return(e) => {
            out.push_str("return");
            if let Some(e) = e {
                out.push(' ');
                expr(e, out);
            }
            out.push(';');
        }
        Stmt::Halt => out.push_str("halt;"),
        Stmt::Keyword(k) => {
            out.push_str(k);
            out.push(';');
        }
    }
}

/// The source text for a parsed program.
pub fn program(items: &[Stmt]) -> String {
    let mut out = String::new();
    stmts(items, 0, &mut out);
    out
}
