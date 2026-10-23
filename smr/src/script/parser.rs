//! A recursive-descent parser for CED's script language.
//!
//! The grammar is read off Spike2's own scripts, not from a manual: `coef.s2s` for
//! the declarations and the `for` loop, `basedemo.s2s` (43 KB, pulled from an
//! installed Spike2) for everything else — `proc`, `func`, `if/then/else/endif`,
//! `while … wend`, `repeat … until`, `docase … endcase`, member calls like
//! `View(App(3)).FileName$(1)`, and statements separated by semicolons that are
//! sometimes followed by `;` and sometimes not.
//!
//! Both files parse. That is the test: a parser that cannot read the vendor's own
//! demo has not earned the right to be believed about anything else.

use super::lexer::{Tok, Token};

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    /// A bare name, with its type suffix if it has one.
    Name(String),
    /// `name[arg]`. The argument says which of the four forms it was, because
    /// `a[]`, `a[1]`, `a[:2]` and `a[1:2]` are different things and the printer
    /// has to be able to write them back.
    Index(Box<Expr>, IndexArg),
    /// `expr.name` — the member's own name is in the second field.
    Member(Box<Expr>, String),
    /// `callee(args)`.
    Call(Box<Expr>, Vec<Expr>),
    /// A bare `[]`: the anonymous empty array, passed to `ArrConst(calcArr[i%][], [])`
    /// so the call knows what type to build.
    Empty,
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IndexArg {
    /// `a[]` — the whole array.
    Whole,
    /// `a[expr]`
    One(Box<Expr>),
    /// `a[:expr]`
    From(Box<Expr>),
    /// `a[lo:hi]`
    Range(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `var a, b[10], c := 1;`
    Var(Vec<Declarator>),
    /// `const k := 20;`
    Const(Vec<Declarator>),
    /// `proc Name(args) … end`
    Proc(String, Vec<String>, Vec<Stmt>),
    /// `func Name(args) … end`
    Func(String, Vec<String>, Vec<Stmt>),
    /// `lvalue := expr;`
    Assign(Expr, Expr),
    /// A call written as a statement, with or without a semicolon.
    Expr(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Repeat(Vec<Stmt>, Expr),
    For {
        var: Expr,
        from: Expr,
        to: Expr,
        step: Option<Expr>,
        body: Vec<Stmt>,
    },
    Case(Vec<(Expr, Vec<Stmt>)>),
    Return(Option<Expr>),
    Halt,
    /// `break`, `continue` — single-word statements.
    Keyword(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declarator {
    pub name: String,
    /// Array dimensions, if it was declared `name[a, b]`.
    pub dims: Vec<Expr>,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.col, self.message)
    }
}

pub struct Parser {
    toks: Vec<Token>,
    i: usize,
}

type PResult<T> = Result<T, ParseError>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Self { toks, i: 0 }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.i.min(self.toks.len() - 1)].tok
    }

    fn at_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Kw(k) if *k == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn err<T>(&self, msg: &str) -> PResult<T> {
        let t = &self.toks[self.i.min(self.toks.len() - 1)];
        Err(ParseError {
            message: msg.to_string(),
            line: t.line,
            col: t.col,
        })
    }

    /// An optional semicolon. Spike2 scripts put them at the end of statements
    /// almost everywhere and omit them before `else`, `endif`, `next` and `wend`.
    fn semi(&mut self) {
        while self.eat(&Tok::Semi) {}
    }

    fn name(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                self.i += 1;
                Ok(s)
            }
            _ => self.err("expected a name"),
        }
    }

    /// Parse a whole script.
    pub fn program(&mut self) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            self.semi();
            if *self.peek() == Tok::End {
                return Ok(out);
            }
            out.push(self.statement()?);
        }
    }

    fn statement(&mut self) -> PResult<Stmt> {
        // Declarations and blocks first, by keyword.
        if self.eat_kw("var") {
            let d = self.declarators()?;
            return Ok(Stmt::Var(d));
        }
        if self.eat_kw("const") {
            let d = self.declarators()?;
            return Ok(Stmt::Const(d));
        }
        if self.eat_kw("proc") {
            let (name, args) = self.routine_header()?;
            let body = self.block_until(&["end"])?;
            self.eat_kw("end");
            return Ok(Stmt::Proc(name, args, body));
        }
        if self.eat_kw("func") {
            let (name, args) = self.routine_header()?;
            let body = self.block_until(&["end"])?;
            self.eat_kw("end");
            return Ok(Stmt::Func(name, args, body));
        }
        if self.eat_kw("if") {
            let cond = self.expr()?;
            if !self.eat_kw("then") {
                return self.err("expected then");
            }
            let then = self.block_until(&["else", "endif"])?;
            let mut otherwise = Vec::new();
            if self.eat_kw("else") {
                otherwise = self.block_until(&["endif"])?;
            }
            if !self.eat_kw("endif") {
                return self.err("expected endif");
            }
            return Ok(Stmt::If(cond, then, otherwise));
        }
        if self.eat_kw("while") {
            let cond = self.expr()?;
            if !self.eat_kw("do") {
                return self.err("expected do");
            }
            let body = self.block_until(&["wend"])?;
            if !self.eat_kw("wend") {
                return self.err("expected wend");
            }
            return Ok(Stmt::While(cond, body));
        }
        if self.eat_kw("repeat") {
            let body = self.block_until(&["until"])?;
            if !self.eat_kw("until") {
                return self.err("expected until");
            }
            let cond = self.expr()?;
            return Ok(Stmt::Repeat(body, cond));
        }
        if self.eat_kw("for") {
            let var = self.expr()?;
            if !self.eat(&Tok::Assign) {
                return self.err("expected := in for");
            }
            let from = self.expr()?;
            if !self.eat_kw("to") {
                return self.err("expected to");
            }
            let to = self.expr()?;
            let step = if self.eat_kw("step") {
                Some(self.expr()?)
            } else {
                None
            };
            if !self.eat_kw("do") {
                return self.err("expected do");
            }
            let body = self.block_until(&["next"])?;
            if !self.eat_kw("next") {
                return self.err("expected next");
            }
            return Ok(Stmt::For {
                var,
                from,
                to,
                step,
                body,
            });
        }
        if self.eat_kw("docase") {
            let mut arms = Vec::new();
            loop {
                self.semi();
                if self.eat_kw("endcase") {
                    break;
                }
                if !self.eat_kw("case") {
                    return self.err("expected case or endcase");
                }
                let cond = self.expr()?;
                if !self.eat_kw("then") {
                    return self.err("expected then");
                }
                let body = self.block_until(&["case", "endcase"])?;
                arms.push((cond, body));
            }
            return Ok(Stmt::Case(arms));
        }
        if self.eat_kw("halt") {
            return Ok(Stmt::Halt);
        }
        if self.eat_kw("break") {
            return Ok(Stmt::Keyword("break"));
        }
        if self.eat_kw("continue") {
            return Ok(Stmt::Keyword("continue"));
        }
        if self.eat_kw("return") {
            // `return;` is legal, and so is `return iBut%=2;` — a comparison.
            if *self.peek() == Tok::Semi {
                return Ok(Stmt::Return(None));
            }
            let e = self.expr()?;
            return Ok(Stmt::Return(Some(e)));
        }

        // Otherwise: an assignment or a call.
        let e = self.expr()?;
        if self.eat(&Tok::Assign) {
            let rhs = self.expr()?;
            return Ok(Stmt::Assign(e, rhs));
        }
        if let Tok::OpAssign(op) = self.peek().clone() {
            // `list$ += " " + Str$(...)` is how the demo builds a string.
            self.i += 1;
            let rhs = self.expr()?;
            let combined = Expr::Binary(op, Box::new(e.clone()), Box::new(rhs));
            return Ok(Stmt::Assign(e, combined));
        }
        Ok(Stmt::Expr(e))
    }

    /// Statements up to one of `ends`, which is left unconsumed.
    fn block_until(&mut self, ends: &[&str]) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            self.semi();
            if *self.peek() == Tok::End {
                let want = ends.join(" or ");
                return self.err(&format!("unexpected end of script, expected {want}"));
            }
            if let Tok::Kw(k) = self.peek() {
                if ends.iter().any(|e| e == k) {
                    return Ok(out);
                }
            }
            out.push(self.statement()?);
        }
    }

    fn routine_header(&mut self) -> PResult<(String, Vec<String>)> {
        let name = self.name()?;
        let mut args = Vec::new();
        if self.eat(&Tok::LParen) {
            if !self.eat(&Tok::RParen) {
                loop {
                    // A parameter can be an array: `proc AddToWaterfall(xyh%,
                    // data[], incX%, incY%)`. The brackets are kept on the name so
                    // the distinction is not lost.
                    let mut nm = self.name()?;
                    while self.eat(&Tok::LBracket) {
                        if !self.eat(&Tok::RBracket) {
                            return self.err("expected ] in an array parameter");
                        }
                        nm.push_str("[]");
                    }
                    args.push(nm);
                    if self.eat(&Tok::Comma) {
                        continue;
                    }
                    if self.eat(&Tok::RParen) {
                        break;
                    }
                    return self.err("expected , or ) in an argument list");
                }
            }
        }
        Ok((name, args))
    }

    fn declarators(&mut self) -> PResult<Vec<Declarator>> {
        let mut out = Vec::new();
        loop {
            let name = self.name()?;
            // Dimensions repeat rather than take a comma list:
            // `var calcArr[250][FFTPoint%/2];` is in the demo.
            let mut dims = Vec::new();
            while self.eat(&Tok::LBracket) {
                if !self.eat(&Tok::RBracket) {
                    loop {
                        dims.push(self.expr()?);
                        if self.eat(&Tok::Comma) {
                            continue;
                        }
                        if self.eat(&Tok::RBracket) {
                            break;
                        }
                        return self.err("expected , or ] in a dimension list");
                    }
                }
            }
            let init = if self.eat(&Tok::Assign) {
                Some(self.expr()?)
            } else {
                None
            };
            out.push(Declarator { name, dims, init });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    // ── expressions ────────────────────────────────────────────────────────
    fn expr(&mut self) -> PResult<Expr> {
        self.logic_or()
    }

    fn logic_or(&mut self) -> PResult<Expr> {
        let mut left = self.logic_and()?;
        loop {
            let op = if self.eat_kw("or") {
                "or"
            } else if self.eat_kw("xor") {
                "xor"
            } else {
                return Ok(left);
            };
            let right = self.logic_and()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn logic_and(&mut self) -> PResult<Expr> {
        let mut left = self.unary()?;
        loop {
            if !self.eat_kw("and") {
                return Ok(left);
            }
            let right = self.unary()?;
            left = Expr::Binary("and", Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.eat_kw("not") {
            let e = self.unary()?;
            return Ok(Expr::Unary("not", Box::new(e)));
        }
        if self.eat(&Tok::Minus) {
            // Always a unary node: folding `-0.005` into a literal here would end
            // the expression at the literal, so `step -0.005*scl` would stop at
            // -0.005 and then fail looking for `do`.
            let e = self.unary()?;
            return Ok(Expr::Unary("-", Box::new(e)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Eq => "=",
                Tok::Ne => "<>",
                Tok::Lt => "<",
                Tok::Gt => ">",
                Tok::Le => "<=",
                Tok::Ge => ">=",
                _ => return Ok(left),
            };
            self.i += 1;
            let right = self.additive()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut left = self.multiplicative()?;
        loop {
            // `band`/`bor`/`bxor` sit with + and -, which is the level that makes
            // `if flags% band 1 = 1 then` mean what it reads like.
            let op = match self.peek() {
                Tok::Plus => "+",
                Tok::Minus => "-",
                Tok::Kw("band") => "band",
                Tok::Kw("bor") => "bor",
                Tok::Kw("bxor") => "bxor",
                _ => return Ok(left),
            };
            self.i += 1;
            let right = self.multiplicative()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn multiplicative(&mut self) -> PResult<Expr> {
        let mut left = self.postfix()?;
        loop {
            let op = if self.eat(&Tok::Star) {
                "*"
            } else if self.eat(&Tok::Slash) {
                "/"
            } else if self.eat_kw("mod") {
                "mod"
            } else {
                return Ok(left);
            };
            let right = self.postfix()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat(&Tok::Dot) {
                let name = self.name()?;
                e = Expr::Member(Box::new(e), name);
                continue;
            }
            if self.eat(&Tok::LParen) {
                let mut args = Vec::new();
                if !self.eat(&Tok::RParen) {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(&Tok::Comma) {
                            continue;
                        }
                        if self.eat(&Tok::RParen) {
                            break;
                        }
                        return self.err("expected , or ) in a call");
                    }
                }
                e = Expr::Call(Box::new(e), args);
                continue;
            }
            if self.eat(&Tok::LBracket) {
                // `a[:n]` is a slice from the start; `a[]` is the whole array.
                let idx = if self.eat(&Tok::Colon) {
                    IndexArg::From(Box::new(self.expr()?))
                } else if *self.peek() == Tok::RBracket {
                    IndexArg::Whole
                } else {
                    let first = self.expr()?;
                    if self.eat(&Tok::Colon) {
                        IndexArg::Range(Box::new(first), Box::new(self.expr()?))
                    } else {
                        IndexArg::One(Box::new(first))
                    }
                };
                if !self.eat(&Tok::RBracket) {
                    return self.err("expected ]");
                }
                e = Expr::Index(Box::new(e), idx);
                continue;
            }
            return Ok(e);
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Int(v) => {
                self.i += 1;
                Ok(Expr::Int(v))
            }
            Tok::Float(v) => {
                self.i += 1;
                Ok(Expr::Float(v))
            }
            Tok::Str(s) => {
                // Adjacent string literals concatenate, even across a line break:
                // Spike2's demo splits a long message over several lines without a
                // `+` or a comma between the pieces.
                self.i += 1;
                let mut text = s;
                while let Tok::Str(more) = self.peek().clone() {
                    text.push_str(&more);
                    self.i += 1;
                }
                Ok(Expr::Str(text))
            }
            Tok::Ident(s) => {
                self.i += 1;
                Ok(Expr::Name(s))
            }
            Tok::LParen => {
                self.i += 1;
                let e = self.expr()?;
                if !self.eat(&Tok::RParen) {
                    return self.err("expected )");
                }
                Ok(e)
            }
            Tok::LBracket => {
                self.i += 1;
                if !self.eat(&Tok::RBracket) {
                    return self.err("expected ] after [");
                }
                Ok(Expr::Empty)
            }
            // `string` is a keyword but also a type name in declarations; treat it
            // as a name here rather than failing.
            Tok::Kw("string") => {
                self.i += 1;
                Ok(Expr::Name("string".into()))
            }
            _ => self.err("expected an expression"),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// CED's demo script, if it has been pulled from the VM. It is CED's file, so
    /// it lives in gitignored `vmbin/`; the tests that use it skip when it is
    /// absent. Cargo runs tests from the package directory, so both depths are
    /// tried.
    pub fn demo_script() -> Option<String> {
        for p in ["vmbin/basedemo.s2s", "../vmbin/basedemo.s2s"] {
            if let Ok(s) = std::fs::read_to_string(p) {
                eprintln!("found the demo script at {p}");
                return Some(s);
            }
        }
        None
    }

    fn ok(src: &str) -> Vec<Stmt> {
        parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    const COEF_S2S: &str = "var coef[2047], f%, i%, n%, cut[3], wid[3], att[3];\r\ncut[0] := 0.10; wid[0] := 0.05; att[0] := 50;\r\nf% := FileOpen(\"coef.bin\", 9);\r\nfor i% := 0 to 3 - 1 do\r\n    n% := FIRQuick(coef[], 0, cut[i%], wid[i%], att[i%]);\r\n    BWrite(coef[:n%]);\r\nnext;\r\nFileClose();\r\n";

    #[test]
    fn parses_the_coefficient_script() {
        let items = ok(COEF_S2S);
        assert!(matches!(&items[0], Stmt::Var(d) if d.len() == 7));
        assert_eq!(items.iter().filter(|s| matches!(s, Stmt::For { .. })).count(), 1);
        assert!(matches!(items.last(), Some(Stmt::Expr(Expr::Call(..)))));
    }

    #[test]
    fn spike2s_own_demo_parses() {
        let Some(src) = demo_script() else {
            eprintln!("skipping: demo script not present");
            return;
        };
        let items = super::parse(&src).expect("the demo script should parse");
        // Recursive: most of the declarations are inside procs.
        fn tally(stmts: &[Stmt], acc: &mut [usize; 4]) {
            for s in stmts {
                match s {
                    Stmt::Var(_) => acc[0] += 1,
                    Stmt::Const(_) => acc[1] += 1,
                    Stmt::Proc(..) => acc[2] += 1,
                    Stmt::Func(..) => acc[3] += 1,
                    _ => {}
                }
                match s {
                    Stmt::If(_, a, b) => {
                        tally(a, acc);
                        tally(b, acc);
                    }
                    Stmt::While(_, b) | Stmt::For { body: b, .. } => tally(b, acc),
                    Stmt::Repeat(b, _) => tally(b, acc),
                    Stmt::Proc(_, _, b) | Stmt::Func(_, _, b) => tally(b, acc),
                    Stmt::Case(arms) => {
                        for (_, b) in arms {
                            tally(b, acc);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut acc = [0usize; 4];
        tally(&items, &mut acc);
        assert_eq!(acc[2], 32, "procs");
        assert_eq!(acc[3], 7, "funcs");
        assert_eq!(acc[0], 88, "vars");
        assert_eq!(acc[1], 7, "consts");
        // The member call that made the lexer grow a Dot token.
        let has_member = items.iter().any(|s| format!("{s:?}").contains("Member"));
        assert!(has_member, "no member access found");
    }

    #[test]
    fn negative_step_does_not_end_the_expression() {
        // The regression: folding -0.005 into a literal left `*scl` unparsed.
        let items = ok("for d := 0.8 to 0 step -0.005*scl do Yield(1) next;");
        match &items[0] {
            // `-0.005*scl` is a unary minus over a product, which is what it means.
            Stmt::For { step: Some(s), .. } => {
                assert!(format!("{s:?}").contains("\"*\""), "step: {s:?}")
            }
            other => panic!("expected a for loop, got {other:?}"),
        }
    }

    #[test]
    fn adjacent_string_literals_concatenate() {
        let items = ok("Say(\"one \"\n\"two\", 1);");
        match &items[0] {
            Stmt::Expr(Expr::Call(_, args)) => {
                assert_eq!(args[0], Expr::Str("one two".into()));
            }
            other => panic!("expected a call, got {other:?}"),
        }
    }

    #[test]
    fn hex_and_compound_assignment() {
        let items = ok("x := 1+2+0x2010; s$ += \" more\";");
        assert!(matches!(&items[0], Stmt::Assign(_, Expr::Binary("+", ..))));
        match &items[1] {
            Stmt::Assign(_, Expr::Binary("+", _, _)) => {}
            other => panic!("+= should become a binary +, got {other:?}"),
        }
    }

    #[test]
    fn array_parameters_and_multi_dimensional_declarations() {
        let items = ok("proc P(a%, b[])\nvar m[250][300];\nend;");
        match &items[0] {
            Stmt::Proc(_, args, body) => {
                assert_eq!(args, &vec!["a%".to_string(), "b[]".to_string()]);
                match &body[0] {
                    Stmt::Var(d) => assert_eq!(d[0].dims.len(), 2),
                    other => panic!("expected a var, got {other:?}"),
                }
            }
            other => panic!("expected a proc, got {other:?}"),
        }
    }

    #[test]
    fn an_unclosed_if_says_where_it_ran_out() {
        let e = parse("if a then\n b := 1;\n").unwrap_err();
        assert!(e.contains("endif"), "{e}");
    }
}

/// Lex and parse a script in one step.
pub fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = super::lexer::lex(src).map_err(|e| e.to_string())?;
    Parser::new(toks).program().map_err(|e| e.to_string())
}
