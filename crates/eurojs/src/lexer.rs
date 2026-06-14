//! EuroJS lexer: source → tokens.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    // keywords
    Let,
    Function,
    Return,
    If,
    Else,
    While,
    For,
    True,
    False,
    Null,
    Undefined,
    // operators / punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    EqEq,
    EqEqEq,
    NotEq,
    NotEqEq,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    PlusPlus,
    MinusMinus,
    Arrow,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Dot,
    Colon,
    Question,
    Eof,
}

pub fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        // whitespace
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        // comment
        if ch == '/' && i + 1 < c.len() && c[i + 1] == '/' {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && i + 1 < c.len() && c[i + 1] == '*' {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // numbers
        if ch.is_ascii_digit() || (ch == '.' && i + 1 < c.len() && c[i + 1].is_ascii_digit()) {
            let start = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') {
                i += 1;
                if i < c.len() && (c[i] == '+' || c[i] == '-') {
                    i += 1;
                }
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let s: String = c[start..i].iter().collect();
            let n = s.parse::<f64>().map_err(|_| "invalid number".to_string())?;
            out.push(Tok::Num(n));
            continue;
        }
        // strings
        if ch == '"' || ch == '\'' {
            let quote = ch;
            i += 1;
            let mut s = String::new();
            while i < c.len() && c[i] != quote {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                    s.push(match c[i] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '\\' => '\\',
                        '\'' => '\'',
                        '"' => '"',
                        '0' => '\0',
                        other => other,
                    });
                } else {
                    s.push(c[i]);
                }
                i += 1;
            }
            i += 1; // closing quote
            out.push(Tok::Str(s));
            continue;
        }
        // identifiers / keywords
        if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                i += 1;
            }
            let word: String = c[start..i].iter().collect();
            out.push(match word.as_str() {
                "let" | "var" | "const" => Tok::Let,
                "function" => Tok::Function,
                "return" => Tok::Return,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "for" => Tok::For,
                "true" => Tok::True,
                "false" => Tok::False,
                "null" => Tok::Null,
                "undefined" => Tok::Undefined,
                _ => Tok::Ident(word),
            });
            continue;
        }
        // operators (longest first)
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let three: String = c[i..(i + 3).min(c.len())].iter().collect();
        if three == "===" {
            out.push(Tok::EqEqEq);
            i += 3;
            continue;
        }
        if three == "!==" {
            out.push(Tok::NotEqEq);
            i += 3;
            continue;
        }
        let t2 = match two.as_str() {
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::NotEq),
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            "&&" => Some(Tok::AndAnd),
            "||" => Some(Tok::OrOr),
            "++" => Some(Tok::PlusPlus),
            "--" => Some(Tok::MinusMinus),
            "=>" => Some(Tok::Arrow),
            _ => None,
        };
        if let Some(t) = t2 {
            out.push(t);
            i += 2;
            continue;
        }
        let t1 = match ch {
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '=' => Tok::Assign,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Bang,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ',' => Tok::Comma,
            ';' => Tok::Semi,
            '.' => Tok::Dot,
            ':' => Tok::Colon,
            '?' => Tok::Question,
            _ => return Err(alloc::format!("unknown character '{ch}'")),
        };
        out.push(t1);
        i += 1;
    }
    out.push(Tok::Eof);
    Ok(out)
}
