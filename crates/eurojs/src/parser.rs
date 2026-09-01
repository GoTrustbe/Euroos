//! EuroJS parser: tokens → AST (recursive descent, with precedence climbing).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast::{BinOp, Expr, Stmt};
use crate::lexer::Tok;

pub struct Parser {
    t: Vec<Tok>,
    i: usize,
}

type P<T> = Result<T, String>;

impl Parser {
    pub fn new(t: Vec<Tok>) -> Self {
        Parser { t, i: 0 }
    }

    fn peek(&self) -> &Tok {
        &self.t[self.i]
    }
    fn next(&mut self) -> Tok {
        let t = self.t[self.i].clone();
        if self.i + 1 < self.t.len() {
            self.i += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok) -> P<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(alloc::format!("expected {:?}, got {:?}", t, self.peek()))
        }
    }

    pub fn parse_program(&mut self) -> P<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
        }
        Ok(stmts)
    }

    fn block(&mut self) -> P<Vec<Stmt>> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn statement(&mut self) -> P<Stmt> {
        match self.peek().clone() {
            Tok::Let => {
                self.next();
                let name = self.ident()?;
                let init = if self.eat(&Tok::Assign) { Some(self.expr()?) } else { None };
                self.eat(&Tok::Semi);
                Ok(Stmt::Let(name, init))
            }
            Tok::Function => {
                self.next();
                let name = self.ident()?;
                let params = self.params()?;
                let body = self.block()?;
                Ok(Stmt::FuncDecl(name, params, body))
            }
            Tok::Return => {
                self.next();
                let e = if *self.peek() == Tok::Semi || *self.peek() == Tok::RBrace {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.eat(&Tok::Semi);
                Ok(Stmt::Return(e))
            }
            Tok::If => {
                self.next();
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let then = self.block_or_stmt()?;
                let els = if self.eat(&Tok::Else) {
                    if *self.peek() == Tok::If {
                        alloc::vec![self.statement()?]
                    } else {
                        self.block_or_stmt()?
                    }
                } else {
                    Vec::new()
                };
                Ok(Stmt::If(cond, then, els))
            }
            Tok::While => {
                self.next();
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let body = self.block_or_stmt()?;
                Ok(Stmt::While(cond, body))
            }
            Tok::For => {
                self.next();
                self.expect(&Tok::LParen)?;
                let init = if *self.peek() == Tok::Semi {
                    self.next();
                    None
                } else {
                    let s = self.statement()?; // consumes the ';'
                    Some(Box::new(s))
                };
                let cond = if *self.peek() == Tok::Semi { None } else { Some(self.expr()?) };
                self.expect(&Tok::Semi)?;
                let step = if *self.peek() == Tok::RParen { None } else { Some(self.expr()?) };
                self.expect(&Tok::RParen)?;
                let body = self.block_or_stmt()?;
                Ok(Stmt::For(init, cond, step, body))
            }
            Tok::LBrace => Ok(Stmt::Block(self.block()?)),
            _ => {
                let e = self.expr()?;
                self.eat(&Tok::Semi);
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn block_or_stmt(&mut self) -> P<Vec<Stmt>> {
        if *self.peek() == Tok::LBrace {
            self.block()
        } else {
            Ok(alloc::vec![self.statement()?])
        }
    }

    fn ident(&mut self) -> P<String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            other => Err(alloc::format!("expected identifier, got {other:?}")),
        }
    }

    fn params(&mut self) -> P<Vec<String>> {
        self.expect(&Tok::LParen)?;
        let mut ps = Vec::new();
        while *self.peek() != Tok::RParen {
            ps.push(self.ident()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(ps)
    }

    // ── expressions ──

    fn expr(&mut self) -> P<Expr> {
        self.assignment()
    }

    fn assignment(&mut self) -> P<Expr> {
        let left = self.ternary()?;
        if self.eat(&Tok::Assign) {
            let right = self.assignment()?;
            return Ok(Expr::Assign(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn ternary(&mut self) -> P<Expr> {
        let c = self.binary(0)?;
        if self.eat(&Tok::Question) {
            let then = self.assignment()?;
            self.expect(&Tok::Colon)?;
            let els = self.assignment()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(then), Box::new(els)));
        }
        Ok(c)
    }

    /// Precedence climbing for binary/logical operators.
    fn binary(&mut self, min_prec: u8) -> P<Expr> {
        let mut left = self.unary()?;
        loop {
            let (prec, _) = match prec_of(self.peek()) {
                Some(p) => p,
                None => break,
            };
            if prec < min_prec {
                break;
            }
            let op = self.next();
            let right = self.binary(prec + 1)?;
            left = match op {
                Tok::AndAnd => Expr::And(Box::new(left), Box::new(right)),
                Tok::OrOr => Expr::Or(Box::new(left), Box::new(right)),
                _ => Expr::Bin(bin_of(&op), Box::new(left), Box::new(right)),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> P<Expr> {
        match self.peek() {
            Tok::Minus => {
                self.next();
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Tok::Bang => {
                self.next();
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Tok::Plus => {
                self.next();
                self.unary()
            }
            Tok::PlusPlus => {
                self.next();
                Ok(Expr::Update(true, Box::new(self.unary()?)))
            }
            Tok::MinusMinus => {
                self.next();
                Ok(Expr::Update(false, Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> P<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.next();
                    let name = self.ident()?;
                    e = Expr::Member(Box::new(e), name);
                }
                Tok::LBracket => {
                    self.next();
                    let idx = self.expr()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                Tok::LParen => {
                    let args = self.args()?;
                    e = Expr::Call(Box::new(e), args);
                }
                // Postfix ++/-- (`i++`); for our purposes equivalent to an Update
                // on the lvalue (the old-value semantics don't matter as a statement).
                Tok::PlusPlus => {
                    self.next();
                    e = Expr::Update(true, Box::new(e));
                    break;
                }
                Tok::MinusMinus => {
                    self.next();
                    e = Expr::Update(false, Box::new(e));
                    break;
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn args(&mut self) -> P<Vec<Expr>> {
        self.expect(&Tok::LParen)?;
        let mut a = Vec::new();
        while *self.peek() != Tok::RParen {
            a.push(self.assignment()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(a)
    }

    fn primary(&mut self) -> P<Expr> {
        match self.next() {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Null => Ok(Expr::Null),
            Tok::Undefined => Ok(Expr::Undefined),
            Tok::Ident(s) => {
                // arrow with one parameter: x => expr
                if *self.peek() == Tok::Arrow {
                    self.next();
                    let body = self.arrow_body()?;
                    return Ok(Expr::Func(alloc::vec![s], body));
                }
                Ok(Expr::Ident(s))
            }
            Tok::LParen => {
                // Could be an arrow parameter list: (a, b) => ...
                let save = self.i - 1;
                if let Some(params) = self.try_arrow_params(save) {
                    let body = self.arrow_body()?;
                    return Ok(Expr::Func(params, body));
                }
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                let mut items = Vec::new();
                while *self.peek() != Tok::RBracket {
                    items.push(self.assignment()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBracket)?;
                Ok(Expr::Array(items))
            }
            Tok::LBrace => {
                let mut props = Vec::new();
                while *self.peek() != Tok::RBrace {
                    let key = match self.next() {
                        Tok::Ident(s) => s,
                        Tok::Str(s) => s,
                        other => return Err(alloc::format!("invalid object key {other:?}")),
                    };
                    self.expect(&Tok::Colon)?;
                    let val = self.assignment()?;
                    props.push((key, val));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(&Tok::RBrace)?;
                Ok(Expr::Object(props))
            }
            Tok::Function => {
                let params = self.params()?;
                let body = self.block()?;
                Ok(Expr::Func(params, body))
            }
            other => Err(alloc::format!("unexpected token {other:?}")),
        }
    }

    fn arrow_body(&mut self) -> P<Vec<Stmt>> {
        if *self.peek() == Tok::LBrace {
            self.block()
        } else {
            // expression-body arrow → implicit return
            let e = self.assignment()?;
            Ok(alloc::vec![Stmt::Return(Some(e))])
        }
    }

    /// Try to recognize `(params) =>` starting from the opening `(` at index `lparen`.
    fn try_arrow_params(&mut self, lparen: usize) -> Option<Vec<String>> {
        // We are right after the '('. Scan ahead to the matching ')' and check
        // whether a '=>' follows; if so, parse the parameter list.
        let mut j = lparen + 1;
        let mut params = Vec::new();
        loop {
            match self.t.get(j)? {
                Tok::RParen => {
                    j += 1;
                    break;
                }
                Tok::Ident(s) => {
                    params.push(s.clone());
                    j += 1;
                    match self.t.get(j)? {
                        Tok::Comma => j += 1,
                        Tok::RParen => {
                            j += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        if self.t.get(j) == Some(&Tok::Arrow) {
            self.i = j + 1; // consume ') =>'
            Some(params)
        } else {
            None
        }
    }
}

fn prec_of(t: &Tok) -> Option<(u8, ())> {
    let p = match t {
        Tok::OrOr => 1,
        Tok::AndAnd => 2,
        Tok::EqEq | Tok::NotEq | Tok::EqEqEq | Tok::NotEqEq => 3,
        Tok::Lt | Tok::Gt | Tok::Le | Tok::Ge => 4,
        Tok::Plus | Tok::Minus => 5,
        Tok::Star | Tok::Slash | Tok::Percent => 6,
        _ => return None,
    };
    Some((p, ()))
}

fn bin_of(t: &Tok) -> BinOp {
    match t {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Star => BinOp::Mul,
        Tok::Slash => BinOp::Div,
        Tok::Percent => BinOp::Mod,
        Tok::EqEq => BinOp::Eq,
        Tok::EqEqEq => BinOp::StrictEq,
        Tok::NotEq => BinOp::Ne,
        Tok::NotEqEq => BinOp::StrictNe,
        Tok::Lt => BinOp::Lt,
        Tok::Gt => BinOp::Gt,
        Tok::Le => BinOp::Le,
        Tok::Ge => BinOp::Ge,
        _ => BinOp::Add,
    }
}
