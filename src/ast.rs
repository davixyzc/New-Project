//! Abstract syntax tree produced by the parser.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VarKind {
    Let,
    Const,
}

#[derive(Debug, Clone)]
pub struct FuncDef {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub rest: Option<String>,
    pub body: Vec<Stmt>,
    pub is_arrow: bool,
    /// True when the function literal contains other functions. In that case its
    /// locals live in heap cells so that closures can capture them.
    pub has_inner: bool,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr, u32),
    Var(VarKind, Vec<(String, Option<Expr>)>, u32),
    Func(Box<FuncDef>),
    Return(Option<Expr>, u32),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>, u32),
    While(Expr, Box<Stmt>, u32),
    DoWhile(Box<Stmt>, Expr, u32),
    For {
        init: Option<Box<Stmt>>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
        line: u32,
    },
    ForOf {
        kind: Option<VarKind>,
        name: String,
        iter: Expr,
        body: Box<Stmt>,
        line: u32,
    },
    ForIn {
        kind: Option<VarKind>,
        name: String,
        iter: Expr,
        body: Box<Stmt>,
        line: u32,
    },
    Break(u32),
    Continue(u32),
    Throw(Expr, u32),
    Try {
        body: Vec<Stmt>,
        param: Option<String>,
        handler: Option<Vec<Stmt>>,
        finalizer: Option<Vec<Stmt>>,
        line: u32,
    },
    /// `case` tests are `None` for `default`.
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>, u32),
    Block(Vec<Stmt>),
    Empty,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(String),
    Template(Vec<String>, Vec<Expr>),
    Bool(bool),
    Null,
    Undef,
    This,
    Ident(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Func(Box<FuncDef>),
    Unary(&'static str, Box<Expr>),
    Update {
        op: &'static str,
        prefix: bool,
        target: Box<Expr>,
    },
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Logical(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    New(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// `...expr`; only valid inside array literals and call arguments.
    Spread(Box<Expr>),
}
