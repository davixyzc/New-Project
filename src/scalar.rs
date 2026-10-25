//! Scalar replacement of object literals that do not escape.
//!
//! `const p = { x: a, y: b };` followed only by property accesses such as `p.x` becomes two
//! variables, `p.x` and `p.y`, and no object is built. The rewrite only happens when the object
//! cannot be observed: `p` is declared once in its function, and every use reads or writes one of
//! its properties. Passing `p` on, indexing it, calling a method on it or shadowing its name keeps
//! the object. Reading a property the literal does not set still gives `undefined`.

use std::collections::HashMap;

use crate::ast::{Expr, FuncDef, Stmt, VarKind};

/// How many times each name is declared in a function, counting nested functions too.
type Counts = HashMap<String, usize>;

/// Rewrites the function `def`, and every function nested in it.
pub fn run(def: &mut FuncDef) {
    let mut counts = Counts::new();
    count_function(def, &mut counts);
    let mut body = std::mem::take(&mut def.body);
    rewrite_block(&mut body, &counts);
    nested_stmts(&mut body);
    def.body = body;
}

// ----- declarations ---------------------------------------------------------

/// Rewrites every object literal declared directly in `stmts`, and in the blocks inside them.
fn rewrite_block(stmts: &mut Vec<Stmt>, counts: &Counts) {
    let mut i = 0;
    while i < stmts.len() {
        let mut j = 0;
        while j < declarator_count(&stmts[i]) {
            if try_rewrite(stmts, i, j, counts) {
                break;
            }
            j += 1;
        }
        rewrite_nested_blocks(&mut stmts[i], counts);
        i += 1;
    }
}

fn declarator_count(s: &Stmt) -> usize {
    match s {
        Stmt::Var(_, decls, _) => decls.len(),
        _ => 0,
    }
}

/// Rewrites declarator `j` of the statement at `i` if it is an object literal that qualifies.
/// The statement is replaced by up to three: the declarators before, the object's properties as
/// `let` variables, and the declarators after. The statements after it are rewritten too.
fn try_rewrite(stmts: &mut Vec<Stmt>, i: usize, j: usize, counts: &Counts) -> bool {
    let Stmt::Var(kind, decls, line) = &stmts[i] else {
        return false;
    };
    let Some((name, Some(Expr::Object(props)))) = decls.get(j) else {
        return false;
    };
    let (kind, line) = (*kind, *line);
    let (name, props) = (name.clone(), props.clone());
    let before = decls[..j].to_vec();
    let later = decls[j + 1..].to_vec();

    let fields: HashMap<String, String> = props
        .iter()
        .map(|(key, _)| (key.clone(), format!("{name}.{key}")))
        .collect();
    if counts.get(&name) != Some(&1)
        || fields.len() != props.len()
        || mentions_in(&stmts[..i], &name)
        || props.iter().any(|(_, value)| mentions_expr(value, &name))
    {
        return false;
    }

    // Every later use must be a property access, or the object stays.
    let mut cx = Ctx {
        name: &name,
        fields: &fields,
        escapes: 0,
        mentions: 0,
    };
    let mut later = later;
    for (_, init) in later.iter_mut() {
        if let Some(e) = init {
            rewrite_expr(e, &mut cx);
        }
    }
    let mut tail = stmts[i + 1..].to_vec();
    rewrite_stmts(&mut tail, &mut cx);
    if cx.escapes > 0 {
        return false;
    }

    let field_decls: Vec<(String, Option<Expr>)> = props
        .into_iter()
        .map(|(key, value)| (format!("{name}.{key}"), Some(value)))
        .collect();
    let mut replacement = Vec::new();
    if !before.is_empty() {
        replacement.push(Stmt::Var(kind, before, line));
    }
    replacement.push(Stmt::Var(VarKind::Let, field_decls, line));
    if !later.is_empty() {
        replacement.push(Stmt::Var(kind, later, line));
    }
    stmts.truncate(i);
    stmts.extend(replacement);
    stmts.extend(tail);
    true
}

/// Rewrites the blocks nested in a statement, which are scopes of their own.
fn rewrite_nested_blocks(s: &mut Stmt, counts: &Counts) {
    match s {
        Stmt::Block(stmts) => rewrite_block(stmts, counts),
        Stmt::If(_, then, otherwise, _) => {
            rewrite_nested_blocks(then, counts);
            if let Some(o) = otherwise {
                rewrite_nested_blocks(o, counts);
            }
        }
        Stmt::While(_, body, _)
        | Stmt::DoWhile(body, _, _)
        | Stmt::For { body, .. }
        | Stmt::ForOf { body, .. }
        | Stmt::ForIn { body, .. } => rewrite_nested_blocks(body, counts),
        Stmt::Try {
            body,
            handler,
            finalizer,
            ..
        } => {
            rewrite_block(body, counts);
            for block in handler.iter_mut().chain(finalizer.iter_mut()) {
                rewrite_block(block, counts);
            }
        }
        Stmt::Switch(_, cases, _) => {
            for (_, body) in cases {
                rewrite_block(body, counts);
            }
        }
        _ => {}
    }
}

// ----- uses -----------------------------------------------------------------

/// What the rewrite of one object needs to know about the uses of its name.
struct Ctx<'a> {
    name: &'a str,
    /// The variable that holds each property, by property name.
    fields: &'a HashMap<String, String>,
    /// Uses the rewrite cannot replace. The object must stay if there are any.
    escapes: usize,
    /// Every use of the name, replaced or not.
    mentions: usize,
}

fn is_name(e: &Expr, cx: &Ctx) -> bool {
    matches!(e, Expr::Ident(n) if n == cx.name)
}

fn rewrite_expr(e: &mut Expr, cx: &mut Ctx) {
    if let Expr::Ident(n) = e {
        if n == cx.name {
            cx.mentions += 1;
            cx.escapes += 1;
        }
        return;
    }
    if let Expr::Member(object, key) = e {
        if is_name(object, cx) {
            cx.mentions += 1;
            let replacement = match cx.fields.get(key.as_str()) {
                Some(field) => Expr::Ident(field.clone()),
                None => Expr::Undef,
            };
            *e = replacement;
            return;
        }
    }
    match e {
        Expr::Assign(_, target, value) => {
            rewrite_target(target, cx);
            rewrite_expr(value, cx);
        }
        Expr::Update { target, .. } => rewrite_target(target, cx),
        Expr::Call(callee, args) | Expr::New(callee, args) => {
            // A method call passes the object as `this`, so the object must exist.
            if matches!(callee.as_ref(), Expr::Member(object, _) if is_name(object, cx)) {
                cx.mentions += 1;
                cx.escapes += 1;
            } else {
                rewrite_expr(callee, cx);
            }
            for arg in args {
                rewrite_expr(arg, cx);
            }
        }
        Expr::Func(def) => rewrite_function(def, cx),
        other => {
            for child in children_mut(other) {
                rewrite_expr(child, cx);
            }
        }
    }
}

/// An assignment or update target. Writing a property the literal sets assigns its variable.
fn rewrite_target(target: &mut Expr, cx: &mut Ctx) {
    if let Expr::Member(object, key) = target {
        if is_name(object, cx) {
            cx.mentions += 1;
            match cx.fields.get(key.as_str()).cloned() {
                Some(field) => *target = Expr::Ident(field),
                // A property the literal does not set would change the object's shape.
                None => cx.escapes += 1,
            }
            return;
        }
    }
    rewrite_expr(target, cx);
}

fn rewrite_function(def: &mut FuncDef, cx: &mut Ctx) {
    for param in &mut def.params {
        if let Some(default) = &mut param.default {
            rewrite_expr(default, cx);
        }
    }
    rewrite_stmts(&mut def.body, cx);
}

fn rewrite_stmts(stmts: &mut [Stmt], cx: &mut Ctx) {
    for s in stmts {
        rewrite_stmt(s, cx);
    }
}

fn rewrite_stmt(s: &mut Stmt, cx: &mut Ctx) {
    match s {
        Stmt::Expr(e, _) | Stmt::Throw(e, _) => rewrite_expr(e, cx),
        Stmt::Var(_, decls, _) => {
            for (_, init) in decls {
                if let Some(e) = init {
                    rewrite_expr(e, cx);
                }
            }
        }
        Stmt::Func(def) => rewrite_function(def, cx),
        Stmt::Return(value, _) => {
            if let Some(e) = value {
                rewrite_expr(e, cx);
            }
        }
        Stmt::If(test, then, otherwise, _) => {
            rewrite_expr(test, cx);
            rewrite_stmt(then, cx);
            if let Some(o) = otherwise {
                rewrite_stmt(o, cx);
            }
        }
        Stmt::While(test, body, _) => {
            rewrite_expr(test, cx);
            rewrite_stmt(body, cx);
        }
        Stmt::DoWhile(body, test, _) => {
            rewrite_stmt(body, cx);
            rewrite_expr(test, cx);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                rewrite_stmt(i, cx);
            }
            if let Some(t) = test {
                rewrite_expr(t, cx);
            }
            if let Some(u) = update {
                rewrite_expr(u, cx);
            }
            rewrite_stmt(body, cx);
        }
        Stmt::ForOf {
            name, iter, body, ..
        }
        | Stmt::ForIn {
            name, iter, body, ..
        } => {
            // The loop variable is assigned, so the name cannot stand for the object.
            if name == cx.name {
                cx.mentions += 1;
                cx.escapes += 1;
            }
            rewrite_expr(iter, cx);
            rewrite_stmt(body, cx);
        }
        Stmt::Try {
            body,
            handler,
            finalizer,
            ..
        } => {
            rewrite_stmts(body, cx);
            for block in handler.iter_mut().chain(finalizer.iter_mut()) {
                rewrite_stmts(block, cx);
            }
        }
        Stmt::Switch(disc, cases, _) => {
            rewrite_expr(disc, cx);
            for (test, body) in cases {
                if let Some(t) = test {
                    rewrite_expr(t, cx);
                }
                rewrite_stmts(body, cx);
            }
        }
        Stmt::Block(stmts) => rewrite_stmts(stmts, cx),
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty => {}
    }
}

/// Whether `name` is used anywhere in `stmts`.
fn mentions_in(stmts: &[Stmt], name: &str) -> bool {
    let fields = HashMap::new();
    let mut cx = Ctx {
        name,
        fields: &fields,
        escapes: 0,
        mentions: 0,
    };
    rewrite_stmts(&mut stmts.to_vec(), &mut cx);
    cx.mentions > 0
}

fn mentions_expr(e: &Expr, name: &str) -> bool {
    let fields = HashMap::new();
    let mut cx = Ctx {
        name,
        fields: &fields,
        escapes: 0,
        mentions: 0,
    };
    rewrite_expr(&mut e.clone(), &mut cx);
    cx.mentions > 0
}

// ----- functions nested in a function --------------------------------------

/// Runs the rewrite on each function that is declared directly in `stmts`.
fn nested_stmts(stmts: &mut [Stmt]) {
    for s in stmts {
        nested_stmt(s);
    }
}

fn nested_stmt(s: &mut Stmt) {
    match s {
        Stmt::Expr(e, _) | Stmt::Throw(e, _) => nested_expr(e),
        Stmt::Var(_, decls, _) => {
            for (_, init) in decls {
                if let Some(e) = init {
                    nested_expr(e);
                }
            }
        }
        Stmt::Func(def) => run(def),
        Stmt::Return(value, _) => {
            if let Some(e) = value {
                nested_expr(e);
            }
        }
        Stmt::If(test, then, otherwise, _) => {
            nested_expr(test);
            nested_stmt(then);
            if let Some(o) = otherwise {
                nested_stmt(o);
            }
        }
        Stmt::While(test, body, _) => {
            nested_expr(test);
            nested_stmt(body);
        }
        Stmt::DoWhile(body, test, _) => {
            nested_stmt(body);
            nested_expr(test);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                nested_stmt(i);
            }
            if let Some(t) = test {
                nested_expr(t);
            }
            if let Some(u) = update {
                nested_expr(u);
            }
            nested_stmt(body);
        }
        Stmt::ForOf { iter, body, .. } | Stmt::ForIn { iter, body, .. } => {
            nested_expr(iter);
            nested_stmt(body);
        }
        Stmt::Try {
            body,
            handler,
            finalizer,
            ..
        } => {
            nested_stmts(body);
            for block in handler.iter_mut().chain(finalizer.iter_mut()) {
                nested_stmts(block);
            }
        }
        Stmt::Switch(disc, cases, _) => {
            nested_expr(disc);
            for (test, body) in cases {
                if let Some(t) = test {
                    nested_expr(t);
                }
                nested_stmts(body);
            }
        }
        Stmt::Block(stmts) => nested_stmts(stmts),
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty => {}
    }
}

fn nested_expr(e: &mut Expr) {
    match e {
        Expr::Func(def) => run(def),
        other => {
            for child in children_mut(other) {
                nested_expr(child);
            }
        }
    }
}

// ----- declarations counted per function ------------------------------------

fn bump(counts: &mut Counts, name: &str) {
    *counts.entry(name.to_string()).or_insert(0) += 1;
}

fn count_function(def: &FuncDef, counts: &mut Counts) {
    if let Some(name) = &def.name {
        bump(counts, name);
    }
    for param in &def.params {
        bump(counts, &param.name);
        if let Some(default) = &param.default {
            count_expr(default, counts);
        }
    }
    if let Some(rest) = &def.rest {
        bump(counts, rest);
    }
    for s in &def.body {
        count_stmt(s, counts);
    }
}

fn count_stmt(s: &Stmt, counts: &mut Counts) {
    match s {
        Stmt::Expr(e, _) | Stmt::Throw(e, _) => count_expr(e, counts),
        Stmt::Var(_, decls, _) => {
            for (name, init) in decls {
                bump(counts, name);
                if let Some(e) = init {
                    count_expr(e, counts);
                }
            }
        }
        Stmt::Func(def) => count_function(def, counts),
        Stmt::Return(value, _) => {
            if let Some(e) = value {
                count_expr(e, counts);
            }
        }
        Stmt::If(test, then, otherwise, _) => {
            count_expr(test, counts);
            count_stmt(then, counts);
            if let Some(o) = otherwise {
                count_stmt(o, counts);
            }
        }
        Stmt::While(test, body, _) => {
            count_expr(test, counts);
            count_stmt(body, counts);
        }
        Stmt::DoWhile(body, test, _) => {
            count_stmt(body, counts);
            count_expr(test, counts);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                count_stmt(i, counts);
            }
            if let Some(t) = test {
                count_expr(t, counts);
            }
            if let Some(u) = update {
                count_expr(u, counts);
            }
            count_stmt(body, counts);
        }
        Stmt::ForOf {
            name, iter, body, ..
        }
        | Stmt::ForIn {
            name, iter, body, ..
        } => {
            bump(counts, name);
            count_expr(iter, counts);
            count_stmt(body, counts);
        }
        Stmt::Try {
            body,
            param,
            handler,
            finalizer,
            ..
        } => {
            if let Some(p) = param {
                bump(counts, p);
            }
            for s in body {
                count_stmt(s, counts);
            }
            for block in handler.iter().chain(finalizer.iter()) {
                for s in block {
                    count_stmt(s, counts);
                }
            }
        }
        Stmt::Switch(disc, cases, _) => {
            count_expr(disc, counts);
            for (test, body) in cases {
                if let Some(t) = test {
                    count_expr(t, counts);
                }
                for s in body {
                    count_stmt(s, counts);
                }
            }
        }
        Stmt::Block(stmts) => {
            for s in stmts {
                count_stmt(s, counts);
            }
        }
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty => {}
    }
}

fn count_expr(e: &Expr, counts: &mut Counts) {
    if let Expr::Func(def) = e {
        count_function(def, counts);
        return;
    }
    for child in children(e) {
        count_expr(child, counts);
    }
}

// ----- expression children --------------------------------------------------

/// The direct sub-expressions of `e`. Function bodies are not included.
fn children(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Template(_, parts) | Expr::Array(parts) => parts.iter().collect(),
        Expr::Object(props) => props.iter().map(|(_, value)| value).collect(),
        Expr::Unary(_, operand) | Expr::Spread(operand) | Expr::Member(operand, _) => {
            vec![operand.as_ref()]
        }
        Expr::Update { target, .. } => vec![target.as_ref()],
        Expr::Binary(_, a, b)
        | Expr::Logical(_, a, b)
        | Expr::Assign(_, a, b)
        | Expr::Index(a, b) => {
            vec![a.as_ref(), b.as_ref()]
        }
        Expr::Cond(a, b, c) => vec![a.as_ref(), b.as_ref(), c.as_ref()],
        Expr::Call(callee, args) | Expr::New(callee, args) => std::iter::once(callee.as_ref())
            .chain(args.iter())
            .collect(),
        _ => Vec::new(),
    }
}

/// The direct sub-expressions of `e`, mutably. Function bodies are not included.
fn children_mut(e: &mut Expr) -> Vec<&mut Expr> {
    match e {
        Expr::Template(_, parts) | Expr::Array(parts) => parts.iter_mut().collect(),
        Expr::Object(props) => props.iter_mut().map(|(_, value)| value).collect(),
        Expr::Unary(_, operand) | Expr::Spread(operand) | Expr::Member(operand, _) => {
            vec![operand.as_mut()]
        }
        Expr::Update { target, .. } => vec![target.as_mut()],
        Expr::Binary(_, a, b)
        | Expr::Logical(_, a, b)
        | Expr::Assign(_, a, b)
        | Expr::Index(a, b) => {
            vec![a.as_mut(), b.as_mut()]
        }
        Expr::Cond(a, b, c) => vec![a.as_mut(), b.as_mut(), c.as_mut()],
        Expr::Call(callee, args) | Expr::New(callee, args) => std::iter::once(callee.as_mut())
            .chain(args.iter_mut())
            .collect(),
        _ => Vec::new(),
    }
}
