//! EuroJS abstract syntax tree.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    StrictEq,
    Ne,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
    Undefined,
    Ident(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Assign(Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// Function expression / arrow: params + body.
    Func(Vec<String>, Vec<Stmt>),
    /// Pre-increment/decrement on an lvalue (`++x`, `--x`).
    Update(bool, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let(String, Option<Expr>),
    Expr(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Vec<Stmt>),
    Return(Option<Expr>),
    FuncDecl(String, Vec<String>, Vec<Stmt>),
    Block(Vec<Stmt>),
}
