//! A lexer for CED's script language.
//!
//! The vocabulary comes from `compiler.dll`: the keyword table is a list of plain
//! strings in the binary, and `CScriptMgr`'s own disassembler names the
//! instructions the language compiles to. The shape of the syntax comes from
//! Spike2 scripts themselves — `coef.s2s`, which calls `FIRQuick` and writes the
//! coefficients out, is in the test below.
//!
//! What this is not: a parser, an interpreter, or anything that claims to agree
//! with the original. It turns text into tokens, and the tests check that it does
//! so on a real script.

use std::fmt;

/// Keywords, matched without regard to case. This is the list recovered from
/// `compiler.dll`'s string table.
/// Every one of these appears as a standalone string in `compiler.dll`. Notably
/// absent: `downto`, `div`, `array`, `begin` and `elseif` — words an outsider
/// would guess at and the binary does not have.
const KEYWORDS: &[&str] = &[
    "and", "call", "case", "const", "do", "else", "end", "endcase", "endif", "for", "func", "if",
    "mod", "next", "not", "or", "proc", "repeat", "return", "step", "string", "then", "to",
    "until", "var", "while", "xor",
];

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// An identifier, including any type suffix: `coef`, `i%`, `s$`.
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// One of [`KEYWORDS`], lowercased.
    Kw(&'static str),
    /// `:=`
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    /// `=` — comparison, since assignment is `:=`.
    Eq,
    /// `<>`
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Comma,
    Semi,
    Colon,
    /// Member access: `View(3).FileName$(1)` is in Spike2's own demo script.
    Dot,
    LParen,
    RParen,
    LBracket,
    RBracket,
    /// End of input; the lexer always ends with exactly one of these.
    End,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "ident {s}"),
            Tok::Kw(s) => write!(f, "keyword {s}"),
            Tok::Int(v) => write!(f, "int {v}"),
            Tok::Float(v) => write!(f, "float {v}"),
            Tok::Str(s) => write!(f, "string {s:?}"),
            other => write!(f, "{}", format!("{other:?}").to_lowercase()),
        }
    }
}

/// A token and where it started, so a later parser can point at the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.col, self.message)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Split a script into tokens.
///
/// Identifier type suffixes are part of the identifier: `i%` and `s$` are single
/// tokens, which is how they are written and how the sample script uses them.
pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);

    // Advance one character, keeping the line and column honest in the presence of
    // CRLF, which Spike2 scripts are written with.
    macro_rules! bump {
        () => {{
            if chars[i] == '\n' {
                line += 1;
                col = 1;
            } else if chars[i] != '\r' {
                col += 1;
            }
            i += 1;
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '\r' || c == '\n' || c == ' ' || c == '\t' {
            bump!();
            continue;
        }
        // A comment runs from an apostrophe to the end of the line, which is how
        // Spike2's own demo script is written throughout.
        if c == '\'' {
            while i < chars.len() && chars[i] != '\n' {
                bump!();
            }
            continue;
        }
        let (start_line, start_col) = (line, col);
        let push = |tok: Tok, out: &mut Vec<Token>| out.push(Token {
            tok,
            line: start_line,
            col: start_col,
        });

        if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_body(chars[i]) {
                bump!();
            }
            // A type suffix, if present, belongs to the name.
            if i < chars.len() && (chars[i] == '%' || chars[i] == '$') {
                bump!();
            }
            let text: String = chars[start..i].iter().collect();
            let lower = text.to_ascii_lowercase();
            match KEYWORDS.iter().find(|k| **k == lower) {
                Some(k) if text.len() == k.len() => push(Tok::Kw(k), &mut out),
                _ => push(Tok::Ident(text), &mut out),
            }
            continue;
        }

        if c.is_ascii_digit() || (c == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            let mut is_float = false;
            while i < chars.len() && chars[i].is_ascii_digit() {
                bump!();
            }
            if i < chars.len() && chars[i] == '.' {
                is_float = true;
                bump!();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    bump!();
                }
            }
            let text: String = chars[start..i].iter().collect();
            if is_float {
                match text.parse::<f64>() {
                    Ok(v) => push(Tok::Float(v), &mut out),
                    Err(_) => {
                        return Err(LexError {
                            message: format!("bad number {text}"),
                            line: start_line,
                            col: start_col,
                        })
                    }
                }
            } else {
                match text.parse::<i64>() {
                    Ok(v) => push(Tok::Int(v), &mut out),
                    Err(_) => {
                        return Err(LexError {
                            message: format!("bad number {text}"),
                            line: start_line,
                            col: start_col,
                        })
                    }
                }
            }
            continue;
        }

        if c == '"' {
            bump!();
            let mut s = String::new();
            let mut closed = false;
            while i < chars.len() {
                match chars[i] {
                    '"' => {
                        bump!();
                        closed = true;
                        break;
                    }
                    '\\' => {
                        bump!();
                        if i < chars.len() {
                            let e = chars[i];
                            s.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                '\\' => '\\',
                                '"' => '"',
                                other => other,
                            });
                            bump!();
                        }
                    }
                    other => {
                        s.push(other);
                        bump!();
                    }
                }
            }
            if !closed {
                return Err(LexError {
                    message: "unterminated string".to_string(),
                    line: start_line,
                    col: start_col,
                });
            }
            push(Tok::Str(s), &mut out);
            continue;
        }

        // Two-character operators first.
        let next = if i + 1 < chars.len() { Some(chars[i + 1]) } else { None };
        let two = match (c, next) {
            (':', Some('=')) => Some(Tok::Assign),
            ('<', Some('>')) => Some(Tok::Ne),
            ('<', Some('=')) => Some(Tok::Le),
            ('>', Some('=')) => Some(Tok::Ge),
            _ => None,
        };
        if let Some(t) = two {
            bump!();
            bump!();
            push(t, &mut out);
            continue;
        }

        let one = match c {
            '+' => Some(Tok::Plus),
            '-' => Some(Tok::Minus),
            '*' => Some(Tok::Star),
            '/' => Some(Tok::Slash),
            '=' => Some(Tok::Eq),
            '<' => Some(Tok::Lt),
            '>' => Some(Tok::Gt),
            ',' => Some(Tok::Comma),
            '.' => Some(Tok::Dot),
            ';' => Some(Tok::Semi),
            ':' => Some(Tok::Colon),
            '(' => Some(Tok::LParen),
            ')' => Some(Tok::RParen),
            '[' => Some(Tok::LBracket),
            ']' => Some(Tok::RBracket),
            _ => None,
        };
        match one {
            Some(t) => {
                bump!();
                push(t, &mut out);
            }
            None => {
                return Err(LexError {
                    message: format!("unexpected character {c:?}"),
                    line: start_line,
                    col: start_col,
                })
            }
        }
    }

    out.push(Token {
        tok: Tok::End,
        line,
        col,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Spike2 script: it designs three filters with `FIRQuick` and writes
    /// the coefficients to a binary file. Written for the filter work, kept as a
    /// lexer fixture because it exercises the syntax that matters.
    const COEF_S2S: &str = "var coef[2047], f%, i%, n%, cut[3], wid[3], att[3];\r\n\
cut[0] := 0.10; wid[0] := 0.05; att[0] := 50;\r\n\
f% := FileOpen(\"C:\\\\Users\\\\Hilal\\\\Documents\\\\Spike8\\\\coef.bin\", 9);\r\n\
BWrite(3);\r\n\
for i% := 0 to 3 - 1 do\r\n\
    n% := FIRQuick(coef[], 0, cut[i%], wid[i%], att[i%]);\r\n\
    BWrite(cut[i%]); BWrite(wid[i%]); BWrite(att[i%]); BWrite(n%); BWrite(coef[:n%]);\r\n\
next;\r\n\
FileClose();\r\n";

    #[test]
    fn lexes_a_real_script() {
        let toks = lex(COEF_S2S).expect("lex");
        assert_eq!(toks.first().unwrap().tok, Tok::Kw("var"));
        assert_eq!(toks[1].tok, Tok::Ident("coef".into()));
        assert_eq!(toks[2].tok, Tok::LBracket);
        assert_eq!(toks[3].tok, Tok::Int(2047));
        assert_eq!(toks.last().unwrap().tok, Tok::End);

        // The loop header, which is the one construct with two keywords in it.
        let i = toks
            .iter()
            .position(|t| t.tok == Tok::Kw("for"))
            .expect("for");
        assert_eq!(toks[i + 1].tok, Tok::Ident("i%".into()));
        assert_eq!(toks[i + 2].tok, Tok::Assign);
        assert_eq!(toks[i + 3].tok, Tok::Int(0));
        assert_eq!(toks[i + 4].tok, Tok::Kw("to"));
        assert!(toks.iter().any(|t| t.tok == Tok::Kw("next")));
    }

    #[test]
    fn type_suffixes_stay_attached() {
        let toks = lex("i% := n% + 1;").unwrap();
        assert_eq!(toks[0].tok, Tok::Ident("i%".into()));
        assert_eq!(toks[2].tok, Tok::Ident("n%".into()));
    }

    #[test]
    fn slice_syntax_is_colon_inside_brackets() {
        let toks = lex("coef[:n%]").unwrap();
        assert_eq!(toks[0].tok, Tok::Ident("coef".into()));
        assert_eq!(toks[1].tok, Tok::LBracket);
        assert_eq!(toks[2].tok, Tok::Colon);
        assert_eq!(toks[3].tok, Tok::Ident("n%".into()));
        assert_eq!(toks[4].tok, Tok::RBracket);
    }

    #[test]
    fn strings_keep_their_escapes() {
        let toks = lex(r#""C:\\Users\\Hilal""#).unwrap();
        assert_eq!(toks[0].tok, Tok::Str(r"C:\Users\Hilal".into()));
    }

    #[test]
    fn keywords_are_case_insensitive() {
        for text in ["for", "FOR", "For"] {
            let toks = lex(text).unwrap();
            assert_eq!(toks[0].tok, Tok::Kw("for"), "{text}");
        }
        // ... but an identifier that merely starts with one is not a keyword.
        let toks = lex("format").unwrap();
        assert_eq!(toks[0].tok, Tok::Ident("format".into()));
    }

    #[test]
    fn comparison_operators_are_two_characters_where_they_should_be() {
        let toks = lex("a <> b <= c >= d").unwrap();
        assert!(toks.iter().any(|t| t.tok == Tok::Ne));
        assert!(toks.iter().any(|t| t.tok == Tok::Le));
        assert!(toks.iter().any(|t| t.tok == Tok::Ge));
    }

    #[test]
    fn comments_run_to_the_end_of_the_line() {
        let toks = lex("var a := 1; ' set it\nvar b := 2;").unwrap();
        let vars = toks.iter().filter(|t| t.tok == Tok::Kw("var")).count();
        assert_eq!(vars, 2);
        assert!(toks.iter().all(|t| t.tok != Tok::Ident("set".into())));
    }

    /// Spike2's own demonstration script, if it has been pulled from the VM. It is
    /// CED's file, so it is not in the repository; without it the test says so and
    /// passes.
    #[test]
    fn lexes_spike2s_own_demo_script() {
        let path = std::path::Path::new("vmbin/basedemo.s2s");
        let Ok(src) = std::fs::read_to_string(path) else {
            eprintln!("skipping: {} not present", path.display());
            return;
        };
        let toks = lex(&src).expect("the demo script should lex");
        // 43 KB of script: a couple of thousand tokens at least.
        assert!(toks.len() > 1500, "only {} tokens", toks.len());
        assert_eq!(toks.last().unwrap().tok, Tok::End);
        // The constructs that make it a real language sample rather than a snippet.
        for kw in ["proc", "func", "for", "while", "repeat", "case", "if", "then"] {
            assert!(
                toks.iter().any(|t| t.tok == Tok::Kw(kw)),
                "no {kw} in the demo"
            );
        }
    }

    #[test]
    fn unterminated_string_is_an_error_with_a_position() {
        let e = lex("\"abc").unwrap_err();
        assert!(e.message.contains("unterminated"));
        assert_eq!((e.line, e.col), (1, 1));
    }
}
