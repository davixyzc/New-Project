//! Recursive-descent parser with precedence climbing for binary operators.

use crate::ast::*;
use crate::lexer::{tokenize, Tok, Token};

const RESERVED: &[&str] = &[
    "var",
    "let",
    "const",
    "function",
    "return",
    "if",
    "else",
    "while",
    "do",
    "for",
    "break",
    "continue",
    "throw",
    "try",
    "catch",
    "finally",
    "new",
    "typeof",
    "in",
    "class",
    "import",
    "export",
    "switch",
    "case",
    "default",
    "delete",
    "instanceof",
    "void",
    "yield",
    "await",
    "extends",
    "super",
    "with",
    "debugger",
    "enum",
];

/// Parses a whole file. The result is the module wrapper function, which takes
/// `exports, require, module, __filename, __dirname` as parameters (as in Node.js).
pub fn parse_program(src: &str) -> Result<FuncDef, String> {
    let mut p = Parser::new(tokenize(src)?, 0);
    let mut body = Vec::new();
    while !p.at_eof() {
        body.push(p.statement()?);
    }
    let params = ["exports", "require", "module", "__filename", "__dirname"]
        .iter()
        .map(|n| Param {
            name: n.to_string(),
            default: None,
        })
        .collect();
    Ok(FuncDef {
        name: None,
        params,
        rest: None,
        body,
        is_arrow: false,
        has_inner: p.fn_seq > 0,
        line: 1,
    })
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Number of function literals started so far. Comparing it before and after a body
    /// tells whether that body contains other functions.
    fn_seq: u32,
}

impl Parser {
    fn new(toks: Vec<Token>, fn_seq: u32) -> Self {
        Parser {
            toks,
            pos: 0,
            fn_seq,
        }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }

    fn line(&self) -> u32 {
        self.toks[self.pos].line
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) if *q == p)
    }

    fn punct_at(&self, n: usize, p: &str) -> bool {
        matches!(self.peek_at(n), Tok::Punct(q) if *q == p)
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> Result<(), String> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{}'", p)))
        }
    }

    fn is_word(&self, w: &str) -> bool {
        self.word_at(0, w)
    }

    fn word_at(&self, n: usize, w: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(s) if s.as_str() == w)
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.peek() {
            Tok::Ident(s) if !RESERVED.contains(&s.as_str()) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err(self.unexpected("identifier")),
        }
    }

    fn unexpected(&self, what: &str) -> String {
        let found = match self.peek() {
            Tok::Num(_) => "number".to_string(),
            Tok::Str(_) => "string".to_string(),
            Tok::Template { .. } => "template".to_string(),
            Tok::Ident(s) => format!("'{}'", s),
            Tok::Punct(p) => format!("'{}'", p),
            Tok::Eof => "end of input".to_string(),
        };
        format!(
            "SyntaxError: expected {} but found {} at line {}",
            what,
            found,
            self.line()
        )
    }

    /// Automatic semicolon insertion: `;` is optional before `}`, at EOF and after a line break.
    fn semi(&mut self) -> Result<(), String> {
        if self.eat(";") || self.is_punct("}") || self.at_eof() || self.toks[self.pos].nl_before {
            return Ok(());
        }
        Err(self.unexpected("';'"))
    }

    fn block_body(&mut self) -> Result<Vec<Stmt>, String> {
        let mut out = Vec::new();
        while !self.eat("}") {
            if self.at_eof() {
                return Err(self.unexpected("'}'"));
            }
            out.push(self.statement()?);
        }
        Ok(out)
    }

    // ----- statements -------------------------------------------------------

    fn statement(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        if self.eat("{") {
            return Ok(Stmt::Block(self.block_body()?));
        }
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        if self.is_word("let") || self.is_word("const") || self.is_word("var") {
            let s = self.var_decl()?;
            self.semi()?;
            return Ok(s);
        }
        if self.is_word("function") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.pos += 1;
            let name = self.ident()?;
            let def = self.function_rest(Some(name), line)?;
            return Ok(Stmt::Func(Box::new(def)));
        }
        if self.eat_word("if") {
            self.expect("(")?;
            let cond = self.expression()?;
            self.expect(")")?;
            let then = Box::new(self.statement()?);
            let otherwise = if self.eat_word("else") {
                Some(Box::new(self.statement()?))
            } else {
                None
            };
            return Ok(Stmt::If(cond, then, otherwise, line));
        }
        if self.eat_word("while") {
            self.expect("(")?;
            let cond = self.expression()?;
            self.expect(")")?;
            let body = Box::new(self.statement()?);
            return Ok(Stmt::While(cond, body, line));
        }
        if self.eat_word("do") {
            let body = Box::new(self.statement()?);
            if !self.eat_word("while") {
                return Err(self.unexpected("'while'"));
            }
            self.expect("(")?;
            let cond = self.expression()?;
            self.expect(")")?;
            self.eat(";");
            return Ok(Stmt::DoWhile(body, cond, line));
        }
        if self.eat_word("for") {
            return self.for_statement(line);
        }
        if self.eat_word("return") {
            let no_value = self.is_punct(";")
                || self.is_punct("}")
                || self.at_eof()
                || self.toks[self.pos].nl_before;
            let value = if no_value {
                None
            } else {
                Some(self.expression()?)
            };
            self.semi()?;
            return Ok(Stmt::Return(value, line));
        }
        if self.eat_word("break") {
            self.semi()?;
            return Ok(Stmt::Break(line));
        }
        if self.eat_word("continue") {
            self.semi()?;
            return Ok(Stmt::Continue(line));
        }
        if self.eat_word("throw") {
            let e = self.expression()?;
            self.semi()?;
            return Ok(Stmt::Throw(e, line));
        }
        if self.eat_word("try") {
            return self.try_statement(line);
        }
        if self.eat_word("switch") {
            return self.switch_statement(line);
        }
        let e = self.expression()?;
        self.semi()?;
        Ok(Stmt::Expr(e, line))
    }

    fn var_decl(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let kind = if self.is_word("const") {
            VarKind::Const
        } else {
            VarKind::Let
        };
        self.pos += 1;
        let mut decls = Vec::new();
        loop {
            if self.is_punct("[") || self.is_punct("{") {
                return Err(format!(
                    "SyntaxError: destructuring is not supported yet at line {}",
                    line
                ));
            }
            let name = self.ident()?;
            let init = if self.eat("=") {
                Some(self.assign()?)
            } else {
                None
            };
            decls.push((name, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(Stmt::Var(kind, decls, line))
    }

    fn for_statement(&mut self, line: u32) -> Result<Stmt, String> {
        self.expect("(")?;
        let decl_ahead = self.is_word("let") || self.is_word("const") || self.is_word("var");
        if decl_ahead
            && matches!(self.peek_at(1), Tok::Ident(_))
            && (self.word_at(2, "of") || self.word_at(2, "in"))
        {
            let kind = if self.is_word("const") {
                VarKind::Const
            } else {
                VarKind::Let
            };
            self.pos += 1;
            let name = self.ident()?;
            return self.for_in_of(Some(kind), name, line);
        }
        if matches!(self.peek(), Tok::Ident(_)) && (self.word_at(1, "of") || self.word_at(1, "in"))
        {
            let name = self.ident()?;
            return self.for_in_of(None, name, line);
        }
        let init = if self.eat(";") {
            None
        } else if decl_ahead {
            let s = self.var_decl()?;
            self.expect(";")?;
            Some(Box::new(s))
        } else {
            let e = self.expression()?;
            self.expect(";")?;
            Some(Box::new(Stmt::Expr(e, line)))
        };
        let test = if self.is_punct(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(";")?;
        let update = if self.is_punct(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::For {
            init,
            test,
            update,
            body,
            line,
        })
    }

    fn for_in_of(
        &mut self,
        kind: Option<VarKind>,
        name: String,
        line: u32,
    ) -> Result<Stmt, String> {
        let is_of = self.is_word("of");
        self.pos += 1;
        let iter = self.expression()?;
        self.expect(")")?;
        let body = Box::new(self.statement()?);
        Ok(if is_of {
            Stmt::ForOf {
                kind,
                name,
                iter,
                body,
                line,
            }
        } else {
            Stmt::ForIn {
                kind,
                name,
                iter,
                body,
                line,
            }
        })
    }

    fn switch_statement(&mut self, line: u32) -> Result<Stmt, String> {
        self.expect("(")?;
        let discriminant = self.expression()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}") {
            let test = if self.eat_word("case") {
                Some(self.expression()?)
            } else if self.eat_word("default") {
                None
            } else {
                return Err(self.unexpected("'case' or 'default'"));
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !(self.is_word("case") || self.is_word("default") || self.is_punct("}")) {
                if self.at_eof() {
                    return Err(self.unexpected("'}'"));
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(discriminant, cases, line))
    }

    fn try_statement(&mut self, line: u32) -> Result<Stmt, String> {
        self.expect("{")?;
        let body = self.block_body()?;
        let mut param = None;
        let mut handler = None;
        if self.eat_word("catch") {
            if self.eat("(") {
                param = Some(self.ident()?);
                self.expect(")")?;
            }
            self.expect("{")?;
            handler = Some(self.block_body()?);
        }
        let finalizer = if self.eat_word("finally") {
            self.expect("{")?;
            Some(self.block_body()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.unexpected("'catch' or 'finally'"));
        }
        Ok(Stmt::Try {
            body,
            param,
            handler,
            finalizer,
            line,
        })
    }

    // ----- functions --------------------------------------------------------

    /// Parses `(params) { body }` after the name (if any) has been consumed.
    fn function_rest(&mut self, name: Option<String>, line: u32) -> Result<FuncDef, String> {
        self.fn_seq += 1;
        let mine = self.fn_seq;
        self.expect("(")?;
        let (params, rest) = self.params()?;
        self.expect("{")?;
        let body = self.block_body()?;
        Ok(FuncDef {
            name,
            params,
            rest,
            body,
            is_arrow: false,
            has_inner: self.fn_seq != mine,
            line,
        })
    }

    /// Parses a parameter list. The opening parenthesis has been consumed; this consumes the closing one.
    fn params(&mut self) -> Result<(Vec<Param>, Option<String>), String> {
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.ident()?);
                self.expect(")")?;
                break;
            }
            let name = self.ident()?;
            let default = if self.eat("=") {
                Some(self.assign()?)
            } else {
                None
            };
            params.push(Param { name, default });
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        Ok((params, rest))
    }

    fn arrow_body(
        &mut self,
        params: Vec<Param>,
        rest: Option<String>,
        line: u32,
    ) -> Result<FuncDef, String> {
        self.fn_seq += 1;
        let mine = self.fn_seq;
        let body = if self.eat("{") {
            self.block_body()?
        } else {
            let e = self.assign()?;
            vec![Stmt::Return(Some(e), line)]
        };
        Ok(FuncDef {
            name: None,
            params,
            rest,
            body,
            is_arrow: true,
            has_inner: self.fn_seq != mine,
            line,
        })
    }

    /// Recognises `x => ...` and `(a, b = 1, ...c) => ...` without backtracking.
    fn try_arrow(&mut self) -> Result<Option<Expr>, String> {
        let line = self.line();
        let single = match self.peek() {
            Tok::Ident(s) if !RESERVED.contains(&s.as_str()) && self.punct_at(1, "=>") => {
                Some(s.clone())
            }
            _ => None,
        };
        if let Some(name) = single {
            self.pos += 2;
            let def = self.arrow_body(
                vec![Param {
                    name,
                    default: None,
                }],
                None,
                line,
            )?;
            return Ok(Some(Expr::Func(Box::new(def))));
        }
        if self.is_punct("(") && self.paren_is_arrow() {
            self.pos += 1;
            let (params, rest) = self.params()?;
            self.expect("=>")?;
            let def = self.arrow_body(params, rest, line)?;
            return Ok(Some(Expr::Func(Box::new(def))));
        }
        Ok(None)
    }

    /// True when the parenthesis at the current position is closed by a `)` followed by `=>`.
    fn paren_is_arrow(&self) -> bool {
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.toks.get(i + 1).map(|t| &t.tok),
                            Some(Tok::Punct("=>"))
                        );
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    // ----- expressions ------------------------------------------------------

    pub fn expression(&mut self) -> Result<Expr, String> {
        self.assign()
    }

    fn assign(&mut self) -> Result<Expr, String> {
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }
        let left = self.conditional()?;
        let op = match self.peek() {
            Tok::Punct(p) if matches!(*p, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=") => *p,
            _ => return Ok(left),
        };
        check_target(&left).map_err(|_| self.unexpected("assignment target"))?;
        self.pos += 1;
        let right = self.assign()?;
        Ok(Expr::Assign(op, Box::new(left), Box::new(right)))
    }

    fn conditional(&mut self) -> Result<Expr, String> {
        let cond = self.binary(1)?;
        if self.eat("?") {
            let a = self.assign()?;
            self.expect(":")?;
            let b = self.assign()?;
            return Ok(Expr::Cond(Box::new(cond), Box::new(a), Box::new(b)));
        }
        Ok(cond)
    }

    fn binary(&mut self, min: u8) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while let Tok::Punct(p) = self.peek() {
            let op = *p;
            let prec = match binop_prec(op) {
                Some(p) if p >= min => p,
                _ => break,
            };
            self.pos += 1;
            // `**` is right-associative; everything else is left-associative.
            let right = if op == "**" {
                self.binary(prec)?
            } else {
                self.binary(prec + 1)?
            };
            left = if matches!(op, "&&" | "||" | "??") {
                Expr::Logical(op, Box::new(left), Box::new(right))
            } else {
                Expr::Binary(op, Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if let Tok::Punct(p) = self.peek() {
            let p = *p;
            if matches!(p, "!" | "-" | "+") {
                self.pos += 1;
                let e = self.unary()?;
                return Ok(Expr::Unary(p, Box::new(e)));
            }
            if matches!(p, "++" | "--") {
                self.pos += 1;
                let target = self.unary()?;
                check_target(&target).map_err(|_| self.unexpected("assignment target"))?;
                return Ok(Expr::Update {
                    op: p,
                    prefix: true,
                    target: Box::new(target),
                });
            }
        }
        if self.eat_word("typeof") {
            let e = self.unary()?;
            return Ok(Expr::Unary("typeof", Box::new(e)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let e = self.chain(true)?;
        if let Tok::Punct(p) = self.peek() {
            let p = *p;
            // A line break before `++`/`--` ends the statement instead (ASI).
            if matches!(p, "++" | "--") && !self.toks[self.pos].nl_before {
                check_target(&e).map_err(|_| self.unexpected("assignment target"))?;
                self.pos += 1;
                return Ok(Expr::Update {
                    op: p,
                    prefix: false,
                    target: Box::new(e),
                });
            }
        }
        Ok(e)
    }

    /// Member accesses, calls and `new`. `allow_call` is false for the callee of `new`.
    fn chain(&mut self, allow_call: bool) -> Result<Expr, String> {
        let mut e = if self.eat_word("new") {
            let callee = self.chain(false)?;
            let args = if self.eat("(") {
                self.args()?
            } else {
                Vec::new()
            };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let name = match self.peek() {
                    Tok::Ident(s) => s.clone(),
                    _ => return Err(self.unexpected("property name")),
                };
                self.pos += 1;
                e = Expr::Member(Box::new(e), name);
            } else if self.eat("[") {
                let key = self.expression()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(key));
            } else if allow_call && self.eat("(") {
                let args = self.args()?;
                e = Expr::Call(Box::new(e), args);
            } else {
                break;
            }
        }
        Ok(e)
    }

    /// Call arguments. The opening parenthesis has been consumed; this consumes the closing one.
    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut out = Vec::new();
        while !self.eat(")") {
            out.push(self.argument()?);
            if !self.eat(",") {
                self.expect(")")?;
                break;
            }
        }
        Ok(out)
    }

    /// An argument or array element, which may be a spread `...expr`.
    fn argument(&mut self) -> Result<Expr, String> {
        if self.eat("...") {
            return Ok(Expr::Spread(Box::new(self.assign()?)));
        }
        self.assign()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let tok = self.peek().clone();
        match tok {
            Tok::Num(n) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Tok::Str(s) => {
                self.pos += 1;
                Ok(Expr::Str(s))
            }
            Tok::Template { quasis, exprs } => {
                self.pos += 1;
                let mut parts = Vec::with_capacity(exprs.len());
                for (src, line) in exprs {
                    parts.push(self.sub_expr(&src, line)?);
                }
                Ok(Expr::Template(quasis, parts))
            }
            Tok::Ident(name) => {
                let literal = matches!(
                    name.as_str(),
                    "true" | "false" | "null" | "this" | "function"
                );
                if !literal && RESERVED.contains(&name.as_str()) {
                    return Err(self.unexpected("expression"));
                }
                self.pos += 1;
                Ok(match name.as_str() {
                    "true" => Expr::Bool(true),
                    "false" => Expr::Bool(false),
                    "null" => Expr::Null,
                    "undefined" => Expr::Undef,
                    "this" => Expr::This,
                    "function" => {
                        let line = self.line();
                        let name = if matches!(self.peek(), Tok::Ident(_)) {
                            Some(self.ident()?)
                        } else {
                            None
                        };
                        Expr::Func(Box::new(self.function_rest(name, line)?))
                    }
                    _ => Expr::Ident(name),
                })
            }
            Tok::Punct("(") => {
                self.pos += 1;
                let e = self.expression()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                self.pos += 1;
                let mut items = Vec::new();
                while !self.eat("]") {
                    items.push(self.argument()?);
                    if !self.eat(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                Ok(Expr::Array(items))
            }
            Tok::Punct("{") => {
                self.pos += 1;
                self.object_literal()
            }
            _ => Err(self.unexpected("expression")),
        }
    }

    /// Object literal body; the opening brace has been consumed.
    fn object_literal(&mut self) -> Result<Expr, String> {
        let mut props = Vec::new();
        while !self.eat("}") {
            let key = match self.peek().clone() {
                Tok::Ident(s) | Tok::Str(s) => s,
                Tok::Num(n) => format!("{}", n),
                _ => return Err(self.unexpected("property name")),
            };
            self.pos += 1;
            if self.is_punct("(") {
                let line = self.line();
                let def = self.function_rest(Some(key.clone()), line)?;
                props.push((key, Expr::Func(Box::new(def))));
            } else if self.eat(":") {
                let value = self.assign()?;
                props.push((key, value));
            } else {
                props.push((key.clone(), Expr::Ident(key)));
            }
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(Expr::Object(props))
    }

    /// Parses the source of a template substitution with a sub-parser that shares the function counter.
    fn sub_expr(&mut self, src: &str, line: u32) -> Result<Expr, String> {
        let mut toks = tokenize(src)?;
        for t in toks.iter_mut() {
            t.line += line - 1;
        }
        let mut sub = Parser::new(toks, self.fn_seq);
        let e = sub.expression()?;
        if !sub.at_eof() {
            return Err(sub.unexpected("'}'"));
        }
        self.fn_seq = sub.fn_seq;
        Ok(e)
    }
}

fn check_target(e: &Expr) -> Result<(), ()> {
    match e {
        Expr::Ident(_) | Expr::Member(..) | Expr::Index(..) => Ok(()),
        _ => Err(()),
    }
}

fn binop_prec(op: &str) -> Option<u8> {
    Some(match op {
        "??" => 1,
        "||" => 2,
        "&&" => 3,
        "==" | "!=" | "===" | "!==" => 4,
        "<" | "<=" | ">" | ">=" => 5,
        "+" | "-" => 6,
        "*" | "/" | "%" => 7,
        "**" => 8,
        _ => return None,
    })
}
