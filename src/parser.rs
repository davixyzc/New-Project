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
    let hidden = p.hidden.pop().unwrap_or_default();
    let body = finish_body(hidden, &[], Vec::new(), body, 1);
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
    /// The hidden temporaries of each function being parsed, innermost last. Each is declared at the start of its
    /// function (see `finish_body`).
    hidden: Vec<Vec<String>>,
    /// The temporaries made so far, so that their names stay apart.
    temps: u32,
}

impl Parser {
    fn new(toks: Vec<Token>, fn_seq: u32) -> Self {
        Parser {
            toks,
            pos: 0,
            fn_seq,
            hidden: vec![Vec::new()],
            temps: 0,
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

    /// Parses a class after its name, if it has one. The result is the expression that makes the class: a call of
    /// `%class` with the name, the superclass (or undefined), the constructor and the members. Instance fields become
    /// assignments at the start of the constructor, or just after its `super(...)` call. Static fields are assigned
    /// once the class exists, in order.
    fn class_rest(&mut self, name: Option<String>, line: u32) -> Result<Expr, String> {
        let parent = if self.eat_word("extends") {
            self.chain(true)?
        } else {
            Expr::Undef
        };
        let derived = !matches!(parent, Expr::Undef);
        self.expect("{")?;
        let class_name = name.unwrap_or_default();
        let mut ctor = None;
        let mut members = Vec::new();
        let mut statics = Vec::new();
        let mut instance = Vec::new();
        let mut field_fns = false;
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let member_line = self.line();
            let is_static = self.is_word("static")
                && !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct("="));
            if is_static {
                self.pos += 1;
            }
            let kind = if (self.is_word("get") || self.is_word("set"))
                && matches!(self.peek_at(1), Tok::Ident(_))
            {
                let kind = if self.is_word("get") { "get" } else { "set" };
                self.pos += 1;
                kind
            } else {
                "method"
            };
            let key = self.ident()?;
            if self.is_punct("(") {
                let def = self.function_rest(Some(key.clone()), member_line)?;
                if key == "constructor" && !is_static && kind == "method" {
                    ctor = Some(def);
                } else {
                    members.push(Expr::Array(vec![
                        Expr::Str(kind.to_string()),
                        Expr::Str(key),
                        Expr::Func(Box::new(def)),
                        Expr::Bool(is_static),
                    ]));
                }
            } else {
                let before = self.fn_seq;
                let init = if self.eat("=") {
                    Some(self.assign()?)
                } else {
                    None
                };
                field_fns |= self.fn_seq != before;
                self.semi()?;
                if is_static {
                    statics.push((key, init));
                } else {
                    instance.push((key, init));
                }
            }
        }
        let mut ctor = match ctor {
            Some(mut def) => {
                def.name = Some(class_name.clone()).filter(|n| !n.is_empty());
                def
            }
            None => default_ctor(&class_name, derived, line),
        };
        if !instance.is_empty() {
            let assigns: Vec<Stmt> = instance
                .into_iter()
                .map(|(key, init)| {
                    let target = Expr::Member(Box::new(Expr::This), key);
                    let value = init.unwrap_or(Expr::Undef);
                    Stmt::Expr(Expr::Assign("=", Box::new(target), Box::new(value)), line)
                })
                .collect();
            let at = ctor
                .body
                .iter()
                .position(|s| matches!(s, Stmt::Expr(Expr::SuperCall(_), _)))
                .map_or(0, |i| i + 1);
            let rest = ctor.body.split_off(at);
            ctor.body.extend(assigns);
            ctor.body.extend(rest);
            ctor.has_inner |= field_fns;
        }
        let made = Expr::Call(
            Box::new(Expr::Ident("%class".to_string())),
            vec![
                Expr::Str(class_name),
                parent,
                Expr::Func(Box::new(ctor)),
                Expr::Array(members),
            ],
        );
        if statics.is_empty() {
            return Ok(made);
        }
        let t = self.hidden_temp();
        let mut parts = vec![Expr::Assign(
            "=",
            Box::new(Expr::Ident(t.clone())),
            Box::new(made),
        )];
        for (key, init) in statics {
            let target = Expr::Member(Box::new(Expr::Ident(t.clone())), key);
            let value = init.unwrap_or(Expr::Undef);
            parts.push(Expr::Assign("=", Box::new(target), Box::new(value)));
        }
        parts.push(Expr::Ident(t));
        Ok(Expr::Sequence(parts))
    }

    /// A new name for a temporary. Source code cannot write it, so it cannot clash with a name in the program.
    fn fresh(&mut self) -> String {
        self.temps += 1;
        format!("%{}", self.temps)
    }

    /// Declares `name` for the whole current function, unless it is declared already (see `finish_body`).
    fn hoist(&mut self, name: &str) {
        if let Some(list) = self.hidden.last_mut() {
            if !list.iter().any(|n| n == name) {
                list.push(name.to_string());
            }
        }
    }

    /// A new temporary that the current function declares at its start, for a destructuring assignment.
    fn hidden_temp(&mut self) -> String {
        let t = self.fresh();
        if let Some(list) = self.hidden.last_mut() {
            list.push(t.clone());
        }
        t
    }

    /// Parses a binding pattern: a name, or an array or object pattern. Patterns nest, and their parts may have
    /// defaults. An array pattern may end with a rest element.
    fn pattern(&mut self) -> Result<Pat, String> {
        if self.eat("[") {
            let mut items = Vec::new();
            let mut rest = None;
            while !self.eat("]") {
                if self.eat(",") {
                    items.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.pattern()?));
                    self.expect("]")?;
                    break;
                }
                let part = self.pattern()?;
                let default = if self.eat("=") {
                    Some(self.assign()?)
                } else {
                    None
                };
                items.push(Some((part, default)));
                if !self.eat(",") {
                    self.expect("]")?;
                    break;
                }
            }
            Ok(Pat::Array(items, rest))
        } else if self.eat("{") {
            let mut props = Vec::new();
            let mut rest = None;
            while !self.eat("}") {
                if self.eat("...") {
                    rest = Some(Box::new(self.pattern()?));
                    self.expect("}")?;
                    break;
                }
                let key = self.ident()?;
                let part = if self.eat(":") {
                    self.pattern()?
                } else {
                    Pat::Target(Expr::Ident(key.clone()))
                };
                let default = if self.eat("=") {
                    Some(self.assign()?)
                } else {
                    None
                };
                props.push((key, part, default));
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            Ok(Pat::Object(props, rest))
        } else {
            Ok(Pat::Target(Expr::Ident(self.ident()?)))
        }
    }

    /// Adds to `binds` the (target, value) pairs that bind `pat` to `source`, in the order they are made. A part that
    /// has parts reads its value once, into a temporary. In a declaration the temporaries are declared by the same
    /// statement, so every target is a name. Otherwise they are hidden locals of the function.
    fn destructure(&mut self, pat: Pat, source: Expr, decl: bool, binds: &mut Vec<(Expr, Expr)>) {
        match pat {
            Pat::Target(target) => binds.push((target, source)),
            Pat::Array(items, rest) => {
                let t = if decl {
                    self.fresh()
                } else {
                    self.hidden_temp()
                };
                let count = items.len();
                binds.push((Expr::Ident(t.clone()), call_items(source)));
                for (i, item) in items.into_iter().enumerate() {
                    if let Some((part, default)) = item {
                        let element = Expr::Index(
                            Box::new(Expr::Ident(t.clone())),
                            Box::new(Expr::Num(i as f64)),
                        );
                        self.destructure(part, with_default(element, default), decl, binds);
                    }
                }
                if let Some(part) = rest {
                    let slice = Expr::Member(Box::new(Expr::Ident(t)), "slice".to_string());
                    let tail = Expr::Call(Box::new(slice), vec![Expr::Num(count as f64)]);
                    self.destructure(*part, tail, decl, binds);
                }
            }
            Pat::Object(props, rest) => {
                let t = if decl {
                    self.fresh()
                } else {
                    self.hidden_temp()
                };
                binds.push((Expr::Ident(t.clone()), source));
                // The rest element gets the properties that the named ones do not take.
                let taken: Vec<Expr> = props.iter().map(|(k, _, _)| Expr::Str(k.clone())).collect();
                for (key, part, default) in props {
                    let property = Expr::Member(Box::new(Expr::Ident(t.clone())), key);
                    self.destructure(part, with_default(property, default), decl, binds);
                }
                if let Some(part) = rest {
                    let args = vec![Expr::Ident(t), Expr::Array(taken)];
                    let others = Expr::Call(Box::new(Expr::Ident("%rest".to_string())), args);
                    self.destructure(*part, others, decl, binds);
                }
            }
        }
    }

    /// The declarations that bind the parameters written as patterns, each from the value of its temporary.
    fn pattern_params(&mut self, patterns: Vec<(Pat, String)>, line: u32) -> Vec<Stmt> {
        let mut out = Vec::new();
        for (pat, temp) in patterns {
            let mut binds = Vec::new();
            self.destructure(pat, Expr::Ident(temp), true, &mut binds);
            out.push(Stmt::Var(VarKind::Let, decls_of(binds), line));
        }
        out
    }

    /// `for (const pattern of iter) body`: the loop variable is a temporary, and the body starts by binding the
    /// pattern from it.
    fn for_in_of_pattern(
        &mut self,
        kind: VarKind,
        pat: Pat,
        name: String,
        line: u32,
    ) -> Result<Stmt, String> {
        let is_of = self.is_word("of");
        self.pos += 1;
        let iter = self.expression()?;
        self.expect(")")?;
        let mut binds = Vec::new();
        self.destructure(pat, Expr::Ident(name.clone()), true, &mut binds);
        let body = self.statement()?;
        let body = Box::new(Stmt::Block(vec![
            Stmt::Var(kind, decls_of(binds), line),
            body,
        ]));
        Ok(if is_of {
            Stmt::ForOf {
                kind: Some(kind),
                name,
                iter,
                body,
                line,
            }
        } else {
            Stmt::ForIn {
                kind: Some(kind),
                name,
                iter,
                body,
                line,
            }
        })
    }

    /// A destructuring assignment, as an expression. The source is read once, into a hidden temporary, each target
    /// is assigned in turn, and the value of the expression is that temporary.
    fn assign_pattern(&mut self, pat: Pat, source: Expr) -> Expr {
        let mut binds = Vec::new();
        self.destructure(pat, source, false, &mut binds);
        let first = match &binds[0].0 {
            Expr::Ident(t) => t.clone(),
            _ => unreachable!("a pattern starts with its temporary"),
        };
        let mut parts: Vec<Expr> = binds
            .into_iter()
            .map(|(target, value)| Expr::Assign("=", Box::new(target), Box::new(value)))
            .collect();
        parts.push(Expr::Ident(first));
        Expr::Sequence(parts)
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
        if self.is_word("class") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.pos += 1;
            let name = self.ident()?;
            let made = self.class_rest(Some(name.clone()), line)?;
            return Ok(Stmt::Var(VarKind::Let, vec![(name, Some(made))], line));
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
        let is_var = self.is_word("var");
        let kind = if self.is_word("const") {
            VarKind::Const
        } else {
            VarKind::Let
        };
        self.pos += 1;
        let mut decls = Vec::new();
        loop {
            if self.is_punct("[") || self.is_punct("{") {
                let pat = self.pattern()?;
                self.expect("=")?;
                let source = self.assign()?;
                let mut binds = Vec::new();
                self.destructure(pat, source, true, &mut binds);
                decls.extend(decls_of(binds));
                if !self.eat(",") {
                    break;
                }
                continue;
            }
            let name = self.ident()?;
            let init = if self.eat("=") {
                let mut value = self.assign()?;
                name_anonymous_class(&mut value, &name);
                Some(value)
            } else {
                None
            };
            decls.push((name, init));
            if !self.eat(",") {
                break;
            }
        }
        if is_var {
            return Ok(self.var_assignments(decls, line));
        }
        Ok(Stmt::Var(kind, decls, line))
    }

    /// `var` declares its names for the whole function, so each one is declared at the start of the function (see
    /// `finish_body`), and an initializer assigns it.
    fn var_assignments(&mut self, decls: Vec<(String, Option<Expr>)>, line: u32) -> Stmt {
        let mut assigns = Vec::new();
        for (name, init) in decls {
            self.hoist(&name);
            if let Some(value) = init {
                assigns.push(Expr::Assign(
                    "=",
                    Box::new(Expr::Ident(name)),
                    Box::new(value),
                ));
            }
        }
        match assigns.len() {
            0 => Stmt::Empty,
            1 => Stmt::Expr(assigns.remove(0), line),
            _ => Stmt::Expr(Expr::Sequence(assigns), line),
        }
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
            let is_var = self.is_word("var");
            self.pos += 1;
            let name = self.ident()?;
            if is_var {
                self.hoist(&name);
                return self.for_in_of(None, name, line);
            }
            return self.for_in_of(Some(kind), name, line);
        }
        if decl_ahead && matches!(self.peek_at(1), Tok::Punct(p) if *p == "[" || *p == "{") {
            let kind = if self.is_word("const") {
                VarKind::Const
            } else {
                VarKind::Let
            };
            self.pos += 1;
            let pat = self.pattern()?;
            let name = self.fresh();
            return self.for_in_of_pattern(kind, pat, name, line);
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
        self.hidden.push(Vec::new());
        self.expect("(")?;
        let (params, rest, patterns) = self.params()?;
        let prologue = self.pattern_params(patterns, line);
        self.expect("{")?;
        let body = self.block_body()?;
        let body = finish_body(
            self.hidden.pop().unwrap_or_default(),
            &params,
            prologue,
            body,
            line,
        );
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

    /// Parses a parameter list. The opening parenthesis has been consumed; this consumes the closing one. A parameter
    /// that is a pattern is received under a temporary name, and its pattern is returned with that name.
    fn params(&mut self) -> Result<(Vec<Param>, Option<String>, Vec<(Pat, String)>), String> {
        let mut params = Vec::new();
        let mut rest = None;
        let mut patterns = Vec::new();
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.ident()?);
                self.expect(")")?;
                break;
            }
            let name = if self.is_punct("[") || self.is_punct("{") {
                let pat = self.pattern()?;
                let temp = self.fresh();
                patterns.push((pat, temp.clone()));
                temp
            } else {
                self.ident()?
            };
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
        Ok((params, rest, patterns))
    }

    fn arrow_body(
        &mut self,
        params: Vec<Param>,
        rest: Option<String>,
        patterns: Vec<(Pat, String)>,
        line: u32,
    ) -> Result<FuncDef, String> {
        self.fn_seq += 1;
        let mine = self.fn_seq;
        self.hidden.push(Vec::new());
        let prologue = self.pattern_params(patterns, line);
        let body = if self.eat("{") {
            self.block_body()?
        } else {
            let e = self.assign()?;
            vec![Stmt::Return(Some(e), line)]
        };
        let body = finish_body(
            self.hidden.pop().unwrap_or_default(),
            &params,
            prologue,
            body,
            line,
        );
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
                Vec::new(),
                line,
            )?;
            return Ok(Some(Expr::Func(Box::new(def))));
        }
        if self.is_punct("(") && self.paren_is_arrow() {
            self.pos += 1;
            let (params, rest, patterns) = self.params()?;
            self.expect("=>")?;
            let def = self.arrow_body(params, rest, patterns, line)?;
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

    /// An expression, which may be a comma-separated sequence. Its value is that of the last item.
    pub fn expression(&mut self) -> Result<Expr, String> {
        let first = self.assign()?;
        if !self.is_punct(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(",") {
            items.push(self.assign()?);
        }
        Ok(Expr::Sequence(items))
    }

    fn assign(&mut self) -> Result<Expr, String> {
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }
        let left = self.conditional()?;
        let op = match self.peek() {
            Tok::Punct(p)
                if matches!(
                    *p,
                    "=" | "+="
                        | "-="
                        | "*="
                        | "/="
                        | "%="
                        | "**="
                        | "&="
                        | "|="
                        | "^="
                        | "<<="
                        | ">>="
                        | ">>>="
                        | "&&="
                        | "||="
                        | "??="
                ) =>
            {
                *p
            }
            _ => return Ok(left),
        };
        if op == "=" && matches!(left, Expr::Array(_) | Expr::Object(_)) {
            let pat = expr_to_pat(left).map_err(|_| self.unexpected("assignment target"))?;
            self.pos += 1;
            let right = self.assign()?;
            return Ok(self.assign_pattern(pat, right));
        }
        check_target(&left).map_err(|_| self.unexpected("assignment target"))?;
        self.pos += 1;
        let right = self.assign()?;
        // `a ||= b` is `a || (a = b)`, and likewise for `&&=` and `??=`. The target is read twice, so it must be a
        // name or a property of a name, which reads the same both times.
        let short_circuit = match op {
            "&&=" => Some("&&"),
            "||=" => Some("||"),
            "??=" => Some("??"),
            _ => None,
        };
        if let Some(logic) = short_circuit {
            if !is_plain_target(&left) {
                return Err(self.unexpected(
                    "a name or a property of a name as the target of `&&=`, `||=` or `??=`",
                ));
            }
            let assign = Expr::Assign("=", Box::new(left.clone()), Box::new(right));
            return Ok(Expr::Logical(logic, Box::new(left), Box::new(assign)));
        }
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
        loop {
            let op: &'static str = match self.peek() {
                Tok::Punct(p) => *p,
                Tok::Ident(s) if s.as_str() == "in" => "in",
                Tok::Ident(s) if s.as_str() == "instanceof" => "instanceof",
                _ => break,
            };
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
            if matches!(p, "!" | "-" | "+" | "~") {
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
        if self.eat_word("void") {
            let e = self.unary()?;
            return Ok(Expr::Unary("void", Box::new(e)));
        }
        if self.eat_word("delete") {
            let e = self.unary()?;
            return Ok(Expr::Unary("delete", Box::new(e)));
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
                    "true" | "false" | "null" | "this" | "function" | "class" | "super"
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
                    "class" => {
                        let line = self.line();
                        let name = match self.peek() {
                            Tok::Ident(s) if s.as_str() != "extends" => Some(s.clone()),
                            _ => None,
                        };
                        if name.is_some() {
                            self.pos += 1;
                        }
                        self.class_rest(name, line)?
                    }
                    "super" => {
                        if self.eat("(") {
                            Expr::SuperCall(self.args()?)
                        } else {
                            self.expect(".")?;
                            let name = match self.peek() {
                                Tok::Ident(s) => s.clone(),
                                _ => return Err(self.unexpected("property name")),
                            };
                            self.pos += 1;
                            Expr::SuperMember(name)
                        }
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
        sub.temps = self.temps;
        let e = sub.expression()?;
        if !sub.at_eof() {
            return Err(sub.unexpected("'}'"));
        }
        self.fn_seq = sub.fn_seq;
        self.temps = sub.temps;
        if let (Some(mine), Some(theirs)) = (self.hidden.last_mut(), sub.hidden.pop()) {
            mine.extend(theirs);
        }
        Ok(e)
    }
}

/// A name, or a property of a name, which reads and writes the same place each time it is used.
fn is_plain_target(e: &Expr) -> bool {
    match e {
        Expr::Ident(_) => true,
        Expr::Member(obj, _) => matches!(&**obj, Expr::Ident(_)),
        Expr::Index(obj, key) => {
            matches!(&**obj, Expr::Ident(_)) && matches!(&**key, Expr::Ident(_) | Expr::Num(_))
        }
        _ => false,
    }
}

/// Gives an anonymous class the name of the variable that it is assigned to, as Node.js does. `init` is the value of
/// the declaration: a class made by `class_rest`, or a sequence that starts with one when it has static fields.
fn name_anonymous_class(init: &mut Expr, name: &str) {
    match init {
        Expr::Sequence(parts) => {
            if let Some(Expr::Assign(_, _, made)) = parts.first_mut() {
                name_anonymous_class(made, name);
            }
        }
        Expr::Call(callee, args) if matches!(&**callee, Expr::Ident(n) if n == "%class") => {
            if let Some(Expr::Str(class_name)) = args.first_mut() {
                if class_name.is_empty() {
                    class_name.push_str(name);
                }
            }
            if let Some(Expr::Func(ctor)) = args.get_mut(2) {
                if ctor.name.is_none() {
                    ctor.name = Some(name.to_string());
                }
            }
        }
        _ => {}
    }
}

/// The constructor of a class that has none. A derived class passes its arguments on to the superclass.
fn default_ctor(name: &str, derived: bool, line: u32) -> FuncDef {
    let (rest, body) = if derived {
        let args = vec![Expr::Spread(Box::new(Expr::Ident("%args".to_string())))];
        (
            Some("%args".to_string()),
            vec![Stmt::Expr(Expr::SuperCall(args), line)],
        )
    } else {
        (None, Vec::new())
    };
    FuncDef {
        name: Some(name.to_string()).filter(|n| !n.is_empty()),
        params: Vec::new(),
        rest,
        body,
        is_arrow: false,
        has_inner: false,
        line,
    }
}

/// A binding or assignment target. In a declaration every target is a name.
enum Pat {
    /// A target that is not a pattern: a name, or in an assignment a property too.
    Target(Expr),
    /// `[a, , b = 1, ...rest]`: each position holds its pattern and default, and `None` for a hole.
    Array(Vec<Option<(Pat, Option<Expr>)>>, Option<Box<Pat>>),
    /// `{a, b: c = 1, ...rest}`: each property is read by its key, and bound to its pattern with its default. The rest
    /// element, if any, gets the other own properties.
    Object(Vec<(String, Pat, Option<Expr>)>, Option<Box<Pat>>),
}

/// `e`, or `default` when `e` is undefined.
fn with_default(e: Expr, default: Option<Expr>) -> Expr {
    match default {
        None => e,
        Some(d) => Expr::Cond(
            Box::new(Expr::Binary(
                "===",
                Box::new(e.clone()),
                Box::new(Expr::Undef),
            )),
            Box::new(d),
            Box::new(e),
        ),
    }
}

/// The call that turns a source into the items that a pattern reads by index (see `%items`).
fn call_items(source: Expr) -> Expr {
    Expr::Call(Box::new(Expr::Ident("%items".to_string())), vec![source])
}

/// The declarators of a declaration that binds each name to its value.
fn decls_of(binds: Vec<(Expr, Expr)>) -> Vec<(String, Option<Expr>)> {
    binds
        .into_iter()
        .map(|(target, value)| match target {
            Expr::Ident(name) => (name, Some(value)),
            _ => unreachable!("a declaration binds names"),
        })
        .collect()
}

/// The statements of a function body: its hidden temporaries are declared first, then the parts of its parameters
/// that are patterns, then the body.
fn finish_body(
    hidden: Vec<String>,
    params: &[Param],
    prologue: Vec<Stmt>,
    body: Vec<Stmt>,
    line: u32,
) -> Vec<Stmt> {
    let mut out = Vec::new();
    // A `var` with the name of a parameter is that parameter, so the name is not declared again.
    let temps: Vec<(String, Option<Expr>)> = hidden
        .into_iter()
        .filter(|t| !params.iter().any(|p| p.name == *t))
        .map(|t| (t, None))
        .collect();
    if !temps.is_empty() {
        out.push(Stmt::Var(VarKind::Let, temps, line));
    }
    out.extend(prologue);
    out.extend(body);
    out
}

/// An array or object literal, written as the target of `=`, as the pattern that it stands for.
fn expr_to_pat(e: Expr) -> Result<Pat, ()> {
    match e {
        Expr::Array(items) => {
            let mut elems = Vec::new();
            let mut rest = None;
            for item in items {
                match item {
                    Expr::Spread(inner) => rest = Some(Box::new(expr_to_pat(*inner)?)),
                    other => elems.push(Some(elem_to_pat(other)?)),
                }
            }
            Ok(Pat::Array(elems, rest))
        }
        Expr::Object(props) => {
            let mut out = Vec::new();
            for (key, value) in props {
                let (part, default) = elem_to_pat(value)?;
                out.push((key, part, default));
            }
            Ok(Pat::Object(out, None))
        }
        target => {
            check_target(&target)?;
            Ok(Pat::Target(target))
        }
    }
}

/// An element of a literal pattern: a target, with a default when it is written `target = default`.
fn elem_to_pat(e: Expr) -> Result<(Pat, Option<Expr>), ()> {
    match e {
        Expr::Assign("=", target, default) => Ok((expr_to_pat(*target)?, Some(*default))),
        other => Ok((expr_to_pat(other)?, None)),
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
        "|" => 4,
        "^" => 5,
        "&" => 6,
        "==" | "!=" | "===" | "!==" => 7,
        "<" | "<=" | ">" | ">=" | "in" | "instanceof" => 8,
        "<<" | ">>" | ">>>" => 9,
        "+" | "-" => 10,
        "*" | "/" | "%" => 11,
        "**" => 12,
        _ => return None,
    })
}
