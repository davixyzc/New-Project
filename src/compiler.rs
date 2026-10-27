//! Compiles the AST into bytecode prototypes.
//!
//! Scope resolution happens here: every name becomes a frame slot, a captured cell,
//! an upvalue or a global lookup. A local lives in a heap cell only when some nested
//! function refers to its name, so the other locals of a function stay in plain slots.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::*;
use crate::bytecode::{Op, Proto, UpvalDesc};
use crate::parser::parse_program;
use crate::value::{PropHint, Value};

/// Compiles one source file into the prototype of its module wrapper function.
pub fn compile_module(file: &str, src: &str) -> Result<Proto, String> {
    let mut def = parse_program(src)?;
    crate::scalar::run(&mut def);
    let mut c = Compiler {
        funcs: Vec::new(),
        file: Rc::from(file),
    };
    c.function(&def)
}

struct Compiler {
    funcs: Vec<FuncState>,
    file: Rc<str>,
}

struct FuncState {
    /// Names that nested functions refer to. Locals with these names live in cells.
    captured: HashSet<String>,
    /// Parameters that nested functions refer to, which are copied into them, since nothing assigns them (see
    /// `UpvalDesc::by_value`). They are not cells.
    by_value: HashSet<String>,
    /// The slots of the parameters in `by_value`.
    by_value_slots: HashSet<u32>,
    /// For each slot, whether its current occupant is a cell.
    slot_boxed: Vec<bool>,
    code: Vec<Op>,
    lines: Vec<u32>,
    consts: Vec<Value>,
    string_consts: HashMap<String, u32>,
    names: Vec<Rc<str>>,
    name_index: HashMap<String, u32>,
    protos: Vec<Rc<Proto>>,
    /// Captured names: (name, where to load it from, is_const).
    upvals: Vec<(String, UpvalDesc, bool)>,
    scopes: Vec<Scope>,
    next_slot: u32,
    max_slot: u32,
    loops: Vec<LoopCtx>,
    /// Number of `try` handlers that are active at the current point.
    handlers: u32,
    /// `try` blocks with a `finally` whose body is being compiled, innermost last.
    finally_stack: Vec<FinallyCtx>,
    line: u32,
}

/// A `try` block with a `finally`, while its body is being compiled.
struct FinallyCtx {
    /// Handler depth outside the `try`. Leaving the `try` pops back to this depth.
    handlers: u32,
    body: Vec<Stmt>,
}

struct Scope {
    vars: Vec<Var>,
    first_slot: u32,
}

struct Var {
    name: String,
    slot: u32,
    is_const: bool,
}

#[derive(Default)]
struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    /// Handler depth when the loop started; jumps out of the loop must pop deeper handlers.
    handlers: u32,
    /// False for `switch`, which `break` can leave but `continue` skips over.
    is_loop: bool,
}

enum Ref {
    Local { slot: u32, is_const: bool },
    Upval { index: u32, is_const: bool },
    Global,
}

impl FuncState {
    fn new(captured: HashSet<String>, by_value: HashSet<String>, line: u32) -> Self {
        FuncState {
            captured,
            by_value,
            by_value_slots: HashSet::new(),
            slot_boxed: Vec::new(),
            code: Vec::new(),
            lines: Vec::new(),
            consts: Vec::new(),
            string_consts: HashMap::new(),
            names: Vec::new(),
            name_index: HashMap::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            scopes: Vec::new(),
            next_slot: 0,
            max_slot: 0,
            loops: Vec::new(),
            handlers: 0,
            finally_stack: Vec::new(),
            line,
        }
    }
}

// ----- capture analysis -------------------------------------------------------

/// What the body of a function mentions. Over-approximating is safe: a local that is boxed needlessly only runs a
/// little slower, and a parameter that is not copied only keeps its cell.
#[derive(Default)]
struct Mentions {
    /// The names that nested functions refer to.
    captured: HashSet<String>,
    /// The names that the function, or a function nested in it, assigns or declares.
    written: HashSet<String>,
}

fn mentions_of(def: &FuncDef) -> Mentions {
    let mut out = Mentions::default();
    if def.has_inner {
        for p in &def.params {
            if let Some(d) = &p.default {
                scan_expr(d, false, &mut out);
            }
        }
        for s in &def.body {
            scan_stmt(s, false, &mut out);
        }
    }
    out
}

/// Records a name when inside a nested function. Outside one, names are not captures.
fn note(name: &str, nested: bool, out: &mut Mentions) {
    if nested && !out.captured.contains(name) {
        out.captured.insert(name.to_string());
    }
}

/// Records a name that is assigned or declared.
fn wrote(name: &str, out: &mut Mentions) {
    if !out.written.contains(name) {
        out.written.insert(name.to_string());
    }
}

/// Records the names that an assignment target writes: a name, or the names inside a destructuring pattern.
fn wrote_target(e: &Expr, out: &mut Mentions) {
    match e {
        Expr::Ident(name) => wrote(name, out),
        Expr::Array(items) => {
            for item in items {
                wrote_target(item, out);
            }
        }
        Expr::Object(props) => {
            for (_, v) in props {
                wrote_target(v, out);
            }
        }
        Expr::Spread(x) => wrote_target(x, out),
        Expr::Assign(_, target, _) => wrote_target(target, out),
        _ => {}
    }
}

/// Everything a nested function mentions is a potential capture. Its own parameters, rest parameter and name are
/// declarations, and so are written names of the enclosing function.
fn scan_func(def: &FuncDef, out: &mut Mentions) {
    for p in &def.params {
        wrote(&p.name, out);
        if let Some(d) = &p.default {
            scan_expr(d, true, out);
        }
    }
    if let Some(rest) = &def.rest {
        wrote(rest, out);
    }
    if let Some(name) = &def.name {
        wrote(name, out);
    }
    for s in &def.body {
        scan_stmt(s, true, out);
    }
}

fn scan_stmt(s: &Stmt, nested: bool, out: &mut Mentions) {
    match s {
        Stmt::Expr(e, _) | Stmt::Throw(e, _) => scan_expr(e, nested, out),
        Stmt::Var(_, decls, _) => {
            for (name, init) in decls {
                note(name, nested, out);
                wrote(name, out);
                if let Some(e) = init {
                    scan_expr(e, nested, out);
                }
            }
        }
        Stmt::Func(def) => scan_func(def, out),
        Stmt::Return(value, _) => {
            if let Some(e) = value {
                scan_expr(e, nested, out);
            }
        }
        Stmt::If(cond, then, otherwise, _) => {
            scan_expr(cond, nested, out);
            scan_stmt(then, nested, out);
            if let Some(e) = otherwise {
                scan_stmt(e, nested, out);
            }
        }
        Stmt::While(cond, body, _) => {
            scan_expr(cond, nested, out);
            scan_stmt(body, nested, out);
        }
        Stmt::DoWhile(body, cond, _) => {
            scan_stmt(body, nested, out);
            scan_expr(cond, nested, out);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                scan_stmt(i, nested, out);
            }
            if let Some(t) = test {
                scan_expr(t, nested, out);
            }
            if let Some(u) = update {
                scan_expr(u, nested, out);
            }
            scan_stmt(body, nested, out);
        }
        Stmt::ForOf {
            name, iter, body, ..
        }
        | Stmt::ForIn {
            name, iter, body, ..
        } => {
            note(name, nested, out);
            wrote(name, out);
            scan_expr(iter, nested, out);
            scan_stmt(body, nested, out);
        }
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Empty => {}
        Stmt::Try {
            body,
            param,
            handler,
            finalizer,
            ..
        } => {
            for s in body {
                scan_stmt(s, nested, out);
            }
            if let Some(p) = param {
                note(p, nested, out);
                wrote(p, out);
            }
            for s in handler.iter().flatten() {
                scan_stmt(s, nested, out);
            }
            for s in finalizer.iter().flatten() {
                scan_stmt(s, nested, out);
            }
        }
        Stmt::Switch(discriminant, cases, _) => {
            scan_expr(discriminant, nested, out);
            for (test, body) in cases {
                if let Some(t) = test {
                    scan_expr(t, nested, out);
                }
                for s in body {
                    scan_stmt(s, nested, out);
                }
            }
        }
        Stmt::Block(stmts) => {
            for s in stmts {
                scan_stmt(s, nested, out);
            }
        }
    }
}

fn scan_expr(e: &Expr, nested: bool, out: &mut Mentions) {
    match e {
        Expr::Ident(name) => note(name, nested, out),
        Expr::Func(def) => scan_func(def, out),
        Expr::Template(_, parts) | Expr::Array(parts) | Expr::Sequence(parts) => {
            for p in parts {
                scan_expr(p, nested, out);
            }
        }
        Expr::Object(props) => {
            for (_, v) in props {
                scan_expr(v, nested, out);
            }
        }
        Expr::Unary(_, x) | Expr::Spread(x) | Expr::Member(x, _) => scan_expr(x, nested, out),
        Expr::Update { target, .. } => {
            wrote_target(target, out);
            scan_expr(target, nested, out);
        }
        Expr::Assign(_, a, b) => {
            wrote_target(a, out);
            scan_expr(a, nested, out);
            scan_expr(b, nested, out);
        }
        Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Index(a, b) => {
            scan_expr(a, nested, out);
            scan_expr(b, nested, out);
        }
        Expr::Cond(a, b, c) => {
            scan_expr(a, nested, out);
            scan_expr(b, nested, out);
            scan_expr(c, nested, out);
        }
        Expr::Call(callee, args) | Expr::New(callee, args) => {
            scan_expr(callee, nested, out);
            for a in args {
                scan_expr(a, nested, out);
            }
        }
        Expr::SuperCall(args) => {
            for a in args {
                scan_expr(a, nested, out);
            }
        }
        Expr::SuperMember(_)
        | Expr::Num(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Undef
        | Expr::This => {}
    }
}

/// True when evaluating `e` cannot assign to a variable, so a local read before `e` and a
/// local read after `e` hold the same value.
fn is_pure(e: &Expr) -> bool {
    match e {
        Expr::Num(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Undef
        | Expr::This
        | Expr::Ident(_)
        | Expr::Func(_) => true,
        Expr::Template(_, parts) | Expr::Array(parts) => parts.iter().all(is_pure),
        Expr::Object(props) => props.iter().all(|(_, v)| is_pure(v)),
        Expr::Unary(_, x) | Expr::Spread(x) | Expr::Member(x, _) => is_pure(x),
        Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Index(a, b) => {
            is_pure(a) && is_pure(b)
        }
        Expr::Cond(a, b, c) => is_pure(a) && is_pure(b) && is_pure(c),
        Expr::Sequence(items) => items.iter().all(is_pure),
        Expr::Update { .. }
        | Expr::Assign(..)
        | Expr::Call(..)
        | Expr::New(..)
        | Expr::SuperMember(_)
        | Expr::SuperCall(_) => false,
    }
}

/// Whether evaluating `e` may assign a variable. Only assignments and updates can change a plain local of the
/// running frame: a closure that reaches a local makes it a cell, and cells are not plain locals, so calls cannot.
fn may_assign(e: &Expr) -> bool {
    match e {
        Expr::Num(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Undef
        | Expr::This
        | Expr::Ident(_)
        | Expr::Func(_) => false,
        Expr::Template(_, parts) | Expr::Array(parts) | Expr::Sequence(parts) => {
            parts.iter().any(may_assign)
        }
        Expr::Object(props) => props.iter().any(|(_, v)| may_assign(v)),
        Expr::Unary(_, x) | Expr::Spread(x) | Expr::Member(x, _) => may_assign(x),
        Expr::Update { .. } | Expr::Assign(..) => true,
        Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Index(a, b) => {
            may_assign(a) || may_assign(b)
        }
        Expr::Cond(a, b, c) => may_assign(a) || may_assign(b) || may_assign(c),
        Expr::Call(callee, args) | Expr::New(callee, args) => {
            may_assign(callee) || args.iter().any(may_assign)
        }
        Expr::SuperCall(args) => args.iter().any(may_assign),
        Expr::SuperMember(_) => false,
    }
}

/// For `x += e` and `x = x + e`, returns `e`.
fn concat_rhs<'a>(op: &str, name: &str, value: &'a Expr) -> Option<&'a Expr> {
    match (op, value) {
        ("+=", _) => Some(value),
        ("=", Expr::Binary("+", lhs, rhs)) if matches!(&**lhs, Expr::Ident(n) if n.as_str() == name) => {
            Some(&**rhs)
        }
        _ => None,
    }
}

fn binop(op: &str) -> Op {
    match op {
        "+" => Op::Add,
        "-" => Op::Sub,
        "*" => Op::Mul,
        "/" => Op::Div,
        "%" => Op::Mod,
        "**" => Op::Pow,
        "==" => Op::Eq,
        "!=" => Op::Ne,
        "===" => Op::StrictEq,
        "!==" => Op::StrictNe,
        "<" => Op::Lt,
        "<=" => Op::Le,
        ">" => Op::Gt,
        ">=" => Op::Ge,
        "&" => Op::BitAnd,
        "|" => Op::BitOr,
        "^" => Op::BitXor,
        "<<" => Op::Shl,
        ">>" => Op::Shr,
        ">>>" => Op::UShr,
        "in" => Op::In,
        "instanceof" => Op::InstanceOf,
        _ => unreachable!("unknown binary operator {}", op),
    }
}

/// The superinstruction for `local <op> constant`, if there is one.
fn fused_local_const(op: &str) -> Option<fn(u32, u32) -> Op> {
    match op {
        "+" => Some(Op::AddLK),
        "-" => Some(Op::SubLK),
        "*" => Some(Op::MulLK),
        "%" => Some(Op::ModLK),
        _ => None,
    }
}

impl Compiler {
    fn fs(&mut self) -> &mut FuncState {
        self.funcs.last_mut().unwrap()
    }

    fn emit(&mut self, op: Op) -> usize {
        let fs = self.fs();
        fs.code.push(op);
        fs.lines.push(fs.line);
        fs.code.len() - 1
    }

    fn here(&mut self) -> usize {
        self.fs().code.len()
    }

    fn line(&mut self, line: u32) {
        self.fs().line = line;
    }

    fn syntax_error(&mut self, msg: &str) -> String {
        format!("SyntaxError: {} at line {}", msg, self.fs().line)
    }

    /// Sets the target of a jump emitted earlier. Other instructions are left alone.
    fn patch_to(&mut self, at: usize, target: u32) {
        let fs = self.fs();
        fs.code[at] = match fs.code[at] {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfNotLt(_) => Op::JumpIfNotLt(target),
            Op::JumpIfNotLe(_) => Op::JumpIfNotLe(target),
            Op::JumpIfNotGt(_) => Op::JumpIfNotGt(target),
            Op::JumpIfNotGe(_) => Op::JumpIfNotGe(target),
            Op::JumpIfStrictEq(_) => Op::JumpIfStrictEq(target),
            Op::JumpIfNotStrictEq(_) => Op::JumpIfNotStrictEq(target),
            Op::JumpIfLtLK(s, k, _) => Op::JumpIfLtLK(s, k, target),
            Op::JumpIfLtLL(a, b, _) => Op::JumpIfLtLL(a, b, target),
            Op::JumpIfNotLtLK(s, k, _) => Op::JumpIfNotLtLK(s, k, target),
            Op::JumpIfNotLtLL(a, b, _) => Op::JumpIfNotLtLL(a, b, target),
            Op::AndJump(_) => Op::AndJump(target),
            Op::OrJump(_) => Op::OrJump(target),
            Op::NullishJump(_) => Op::NullishJump(target),
            Op::PushHandler(_) => Op::PushHandler(target),
            other => other,
        };
    }

    /// Points the jump at `at` to the next instruction emitted.
    fn patch(&mut self, at: usize) {
        let target = self.here() as u32;
        self.patch_to(at, target);
    }

    /// A string value in the constant pool (literals).
    fn str_const(&mut self, s: &str) -> u32 {
        let fs = self.fs();
        if let Some(&i) = fs.string_consts.get(s) {
            return i;
        }
        let i = fs.consts.len() as u32;
        fs.consts.push(Value::str(s));
        fs.string_consts.insert(s.to_string(), i);
        i
    }

    /// A property or variable name in the name table. Objects share these keys.
    fn name_const(&mut self, s: &str) -> u32 {
        let fs = self.fs();
        if let Some(&i) = fs.name_index.get(s) {
            return i;
        }
        let i = fs.names.len() as u32;
        fs.names.push(crate::value::intern_name(s));
        fs.name_index.insert(s.to_string(), i);
        i
    }

    fn num_const(&mut self, n: f64) -> u32 {
        let fs = self.fs();
        let i = fs.consts.len() as u32;
        fs.consts.push(Value::Num(n));
        i
    }

    fn push_num(&mut self, n: f64) {
        let i = self.num_const(n);
        self.emit(Op::Const(i));
    }

    // ----- scopes and slots -------------------------------------------------

    fn enter_scope(&mut self) {
        let fs = self.fs();
        let first_slot = fs.next_slot;
        fs.scopes.push(Scope {
            vars: Vec::new(),
            first_slot,
        });
    }

    fn exit_scope(&mut self) {
        let fs = self.fs();
        let scope = fs.scopes.pop().unwrap();
        fs.next_slot = scope.first_slot;
    }

    fn alloc_slot(&mut self) -> u32 {
        let fs = self.fs();
        let slot = fs.next_slot;
        fs.next_slot += 1;
        fs.max_slot = fs.max_slot.max(fs.next_slot);
        if fs.slot_boxed.len() < fs.next_slot as usize {
            fs.slot_boxed.resize(fs.next_slot as usize, false);
        }
        slot
    }

    fn slot_is_boxed(&self, slot: u32) -> bool {
        self.funcs
            .last()
            .unwrap()
            .slot_boxed
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Declares a variable of the current scope. It is a cell only if a nested function uses its name.
    fn declare(&mut self, name: &str, is_const: bool) -> u32 {
        let slot = self.alloc_slot();
        let by_value = self.fs().by_value.contains(name);
        if by_value {
            self.fs().by_value_slots.insert(slot);
        }
        let boxed = self.fs().captured.contains(name) && !by_value;
        let was_boxed = std::mem::replace(&mut self.fs().slot_boxed[slot as usize], boxed);
        if was_boxed && !boxed {
            // The slot may still hold the cell of an earlier variable. Clear it before use.
            self.emit(Op::Undef);
            self.emit(Op::SetLocal(slot));
            self.emit(Op::Pop);
        }
        self.fs().scopes.last_mut().unwrap().vars.push(Var {
            name: name.to_string(),
            slot,
            is_const,
        });
        slot
    }

    /// A hidden slot for compiler temporaries. Temporaries are always written before they are read.
    fn temp(&mut self) -> u32 {
        let slot = self.alloc_slot();
        self.fs().slot_boxed[slot as usize] = false;
        slot
    }

    fn get_slot(&mut self, slot: u32) {
        if self.slot_is_boxed(slot) {
            self.emit(Op::GetBoxed(slot));
        } else {
            self.emit(Op::GetLocal(slot));
        }
    }

    /// Stores the top of the stack into a slot, leaving the value in place.
    fn set_slot(&mut self, slot: u32) {
        if self.slot_is_boxed(slot) {
            self.emit(Op::SetBoxed(slot));
        } else {
            self.emit(Op::SetLocal(slot));
        }
    }

    fn resolve(&mut self, name: &str) -> Ref {
        let level = self.funcs.len() - 1;
        self.resolve_at(level, name)
    }

    fn resolve_at(&mut self, level: usize, name: &str) -> Ref {
        let fs = &self.funcs[level];
        for scope in fs.scopes.iter().rev() {
            if let Some(v) = scope.vars.iter().rev().find(|v| v.name == name) {
                return Ref::Local {
                    slot: v.slot,
                    is_const: v.is_const,
                };
            }
        }
        if let Some(i) = fs.upvals.iter().position(|(n, _, _)| n == name) {
            return Ref::Upval {
                index: i as u32,
                is_const: fs.upvals[i].2,
            };
        }
        if level == 0 {
            return Ref::Global;
        }
        // Not ours: capture it from the enclosing function. Its locals are cells
        // because a nested function refers to the name, so the capture can share the cell.
        let (desc, is_const) = match self.resolve_at(level - 1, name) {
            Ref::Local { slot, is_const } => (
                UpvalDesc {
                    from_local: true,
                    by_value: self.funcs[level - 1].by_value_slots.contains(&slot),
                    index: slot,
                },
                is_const,
            ),
            Ref::Upval { index, is_const } => (
                UpvalDesc {
                    from_local: false,
                    by_value: false,
                    index,
                },
                is_const,
            ),
            Ref::Global => return Ref::Global,
        };
        let fs = &mut self.funcs[level];
        fs.upvals.push((name.to_string(), desc, is_const));
        Ref::Upval {
            index: (fs.upvals.len() - 1) as u32,
            is_const,
        }
    }

    /// The slot of a plain local that is neither a cell nor a constant.
    fn plain_local(&mut self, name: &str) -> Option<u32> {
        match self.resolve(name) {
            Ref::Local {
                slot,
                is_const: false,
            } if !self.slot_is_boxed(slot) => Some(slot),
            _ => None,
        }
    }

    /// Whether `e` is a literal, a plain local, or arithmetic on those. Such an expression cannot throw and cannot
    /// assign, so it may run before the operands that come first without changing what the program does.
    fn simple_operand(&mut self, e: &Expr) -> bool {
        match e {
            Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Undef | Expr::This => {
                true
            }
            Expr::Ident(_) => self.local_operand(e).is_some(),
            Expr::Binary(op, a, b) => {
                matches!(*op, "+" | "-" | "*" | "/" | "%")
                    && self.simple_operand(a)
                    && self.simple_operand(b)
            }
            _ => false,
        }
    }

    /// The slot of a plain local that `e` reads (constant or not), if `e` is one.
    fn local_operand(&mut self, e: &Expr) -> Option<u32> {
        if let Expr::Ident(name) = e {
            if let Ref::Local { slot, .. } = self.resolve(name) {
                if !self.slot_is_boxed(slot) {
                    return Some(slot);
                }
            }
        }
        None
    }

    fn load_var(&mut self, name: &str) {
        match self.resolve(name) {
            Ref::Local { slot, .. } => self.get_slot(slot),
            Ref::Upval { index, .. } => {
                self.emit(Op::GetUpval(index));
            }
            Ref::Global => {
                let c = self.name_const(name);
                self.emit(Op::GetGlobal(c));
            }
        }
    }

    /// Assignment: stores the top of the stack and leaves it there.
    fn store_var(&mut self, name: &str) {
        match self.resolve(name) {
            Ref::Local {
                slot,
                is_const: false,
            } => self.set_slot(slot),
            Ref::Upval {
                index,
                is_const: false,
            } => {
                self.emit(Op::SetUpval(index));
            }
            Ref::Local { is_const: true, .. } | Ref::Upval { is_const: true, .. } => {
                let c = self.name_const(name);
                self.emit(Op::ConstAssign(c));
            }
            Ref::Global => {
                let c = self.name_const(name);
                self.emit(Op::SetGlobal(c));
            }
        }
    }

    /// Initialises a declared variable from the top of the stack and pops it.
    /// `const` is allowed here, because this is the declaration itself.
    fn init_var_pop(&mut self, name: &str) {
        match self.resolve(name) {
            Ref::Local { slot, .. } if !self.slot_is_boxed(slot) => {
                self.emit(Op::PopToLocal(slot));
            }
            Ref::Local { slot, .. } => {
                self.set_slot(slot);
                self.emit(Op::Pop);
            }
            _ => {
                self.store_var(name);
                self.emit(Op::Pop);
            }
        }
    }

    // ----- functions --------------------------------------------------------

    fn function(&mut self, def: &FuncDef) -> Result<Proto, String> {
        let mentions = mentions_of(def);
        // A parameter that nested functions refer to is copied into them when nothing writes it. A default value
        // replaces the argument, so such a parameter is not copied.
        let by_value = def
            .params
            .iter()
            .filter(|p| {
                p.default.is_none()
                    && mentions.captured.contains(&p.name)
                    && !mentions.written.contains(&p.name)
            })
            .map(|p| p.name.clone())
            .collect();
        self.funcs
            .push(FuncState::new(mentions.captured, by_value, def.line));
        self.enter_scope();
        let nfixed = def.params.len() as u32;
        for p in &def.params {
            self.declare(&p.name, false);
        }
        if let Some(rest) = &def.rest {
            self.declare(rest, false);
        }
        let nargs = nfixed + u32::from(def.rest.is_some());
        for i in 0..nargs {
            if self.slot_is_boxed(i) {
                self.emit(Op::BoxParam(i));
            }
        }
        for (i, p) in def.params.iter().enumerate() {
            if let Some(default) = &p.default {
                let slot = i as u32;
                self.get_slot(slot);
                self.emit(Op::Undef);
                self.emit(Op::StrictEq);
                let skip = self.emit(Op::JumpIfFalse(0));
                self.expr(default)?;
                self.set_slot(slot);
                self.emit(Op::Pop);
                self.patch(skip);
            }
        }
        self.declare_block(&def.body)?;
        for s in &def.body {
            self.stmt(s)?;
        }
        self.emit(Op::Undef);
        self.emit(Op::Return);

        let fs = self.funcs.pop().unwrap();
        Ok(Proto {
            native: Default::default(),
            name: def.name.clone().unwrap_or_default(),
            file: self.file.clone(),
            code: fs.code,
            lines: fs.lines,
            consts: fs.consts,
            hints: vec![PropHint::default(); fs.names.len()],
            global_hints: vec![Cell::new(0); fs.names.len()],
            call_entry: Cell::new(0),
            call_frame_len: Cell::new(0),
            call_argc: Cell::new(u32::MAX),
            numeric_entry: Cell::new(0),
            numeric_argc: Cell::new(u32::MAX),
            numeric_bool: Cell::new(false),
            method_entry: Cell::new(0),
            method_argc: Cell::new(u32::MAX),
            names: fs.names,
            protos: fs.protos,
            upvals: fs.upvals.iter().map(|(_, d, _)| *d).collect(),
            nparams: nfixed,
            has_rest: def.rest.is_some(),
            nlocals: fs.max_slot,
            is_arrow: def.is_arrow,
        })
    }

    /// Compiles a nested function and returns its index in the current prototype's table.
    /// An anonymous function takes `name_hint` as its name, as in `const f = () => ...`.
    fn child(&mut self, def: &FuncDef, name_hint: Option<&str>) -> Result<u32, String> {
        let mut proto = self.function(def)?;
        if proto.name.is_empty() {
            if let Some(name) = name_hint {
                proto.name = name.to_string();
            }
        }
        let fs = self.fs();
        fs.protos.push(Rc::new(proto));
        Ok((fs.protos.len() - 1) as u32)
    }

    /// Declares the lexical names of a block (`let`, `const`, function declarations),
    /// creates their cells, and instantiates hoisted functions. Returns the declared slots.
    fn declare_block(&mut self, stmts: &[Stmt]) -> Result<Vec<u32>, String> {
        let mut declared = Vec::new();
        let mut funcs: Vec<(&FuncDef, u32)> = Vec::new();
        for s in stmts {
            match s {
                Stmt::Var(kind, decls, _) => {
                    for (name, _) in decls {
                        declared.push(self.declare(name, *kind == VarKind::Const));
                    }
                }
                Stmt::Func(def) => {
                    let slot = self.declare(def.name.as_deref().unwrap_or(""), false);
                    declared.push(slot);
                    funcs.push((&**def, slot));
                }
                _ => {}
            }
        }
        for &slot in &declared {
            if self.slot_is_boxed(slot) {
                self.emit(Op::NewCell(slot));
            }
        }
        for (def, slot) in funcs {
            let idx = self.child(def, None)?;
            self.emit(Op::Closure(idx));
            self.set_slot(slot);
            self.emit(Op::Pop);
        }
        Ok(declared)
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        self.enter_scope();
        self.declare_block(stmts)?;
        for s in stmts {
            self.stmt(s)?;
        }
        self.exit_scope();
        Ok(())
    }

    // ----- statements -------------------------------------------------------

    fn loop_push(&mut self) {
        let fs = self.fs();
        let handlers = fs.handlers;
        fs.loops.push(LoopCtx {
            handlers,
            is_loop: true,
            ..LoopCtx::default()
        });
    }

    fn loop_pop(&mut self) -> LoopCtx {
        self.fs().loops.pop().unwrap()
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Expr(e, line) => {
                self.line(*line);
                self.expr_discard(e)?;
            }
            Stmt::Var(_, decls, line) => {
                self.line(*line);
                for (name, init) in decls {
                    match init {
                        Some(Expr::Func(def)) => {
                            let idx = self.child(def, Some(name))?;
                            self.emit(Op::Closure(idx));
                        }
                        Some(e) => self.expr(e)?,
                        None => {
                            self.emit(Op::Undef);
                        }
                    }
                    self.init_var_pop(name);
                }
            }
            Stmt::Func(..) | Stmt::Empty => {}
            Stmt::Return(value, line) => {
                self.line(*line);
                match value {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                self.leave_try_blocks(0)?;
                self.emit(Op::Return);
            }
            Stmt::If(cond, then, otherwise, line) => {
                self.line(*line);
                let to_else = self.jump_if_false(cond)?;
                self.stmt(then)?;
                match otherwise {
                    Some(e) => {
                        let to_end = self.emit(Op::Jump(0));
                        self.patch_all(to_else);
                        self.stmt(e)?;
                        self.patch(to_end);
                    }
                    None => self.patch_all(to_else),
                }
            }
            Stmt::While(cond, body, line) => {
                // The test sits at the bottom, so each iteration takes one jump, not two.
                self.line(*line);
                let to_test = self.emit(Op::Jump(0));
                let body_start = self.here() as u32;
                self.loop_push();
                self.stmt(body)?;
                let ctx = self.loop_pop();
                let test = self.here() as u32;
                for c in ctx.continues {
                    self.patch_to(c, test);
                }
                self.patch_to(to_test, test);
                let back = self.jump_if_true(cond)?;
                self.patch_to(back, body_start);
                for b in ctx.breaks {
                    self.patch(b);
                }
            }
            Stmt::DoWhile(body, cond, line) => {
                self.line(*line);
                let start = self.here() as u32;
                self.loop_push();
                self.stmt(body)?;
                let ctx = self.loop_pop();
                let cont = self.here() as u32;
                for c in ctx.continues {
                    self.patch_to(c, cont);
                }
                let back = self.jump_if_true(cond)?;
                self.patch_to(back, start);
                for b in ctx.breaks {
                    self.patch(b);
                }
            }
            Stmt::For {
                init,
                test,
                update,
                body,
                line,
            } => {
                self.line(*line);
                self.enter_scope();
                let mut loop_vars = Vec::new();
                if let Some(init) = init {
                    loop_vars = self.declare_block(std::slice::from_ref(&**init))?;
                    self.stmt(init)?;
                }
                let to_test = self.emit(Op::Jump(0));
                let body_start = self.here() as u32;
                self.loop_push();
                self.stmt(body)?;
                let ctx = self.loop_pop();
                let cont = self.here() as u32;
                for c in ctx.continues {
                    self.patch_to(c, cont);
                }
                // Each iteration gets fresh copies of captured loop variables, so closures
                // created in earlier iterations keep the values they saw.
                for slot in loop_vars {
                    if self.slot_is_boxed(slot) {
                        self.emit(Op::Rebox(slot));
                    }
                }
                if let Some(u) = update {
                    self.expr_discard(u)?;
                }
                let at_test = self.here() as u32;
                self.patch_to(to_test, at_test);
                match test {
                    Some(t) => {
                        let back = self.jump_if_true(t)?;
                        self.patch_to(back, body_start);
                    }
                    None => {
                        self.emit(Op::Jump(body_start));
                    }
                }
                for b in ctx.breaks {
                    self.patch(b);
                }
                self.exit_scope();
            }
            Stmt::ForOf {
                kind,
                name,
                iter,
                body,
                line,
            } => {
                self.for_each(*kind, name, iter, body, *line, false)?;
            }
            Stmt::ForIn {
                kind,
                name,
                iter,
                body,
                line,
            } => {
                self.for_each(*kind, name, iter, body, *line, true)?;
            }
            Stmt::Break(line) => {
                self.line(*line);
                let loop_handlers = match self.fs().loops.last() {
                    Some(l) => l.handlers,
                    None => return Err(self.syntax_error("'break' outside of a loop")),
                };
                self.leave_try_blocks(loop_handlers)?;
                let j = self.emit(Op::Jump(0));
                self.fs().loops.last_mut().unwrap().breaks.push(j);
            }
            Stmt::Continue(line) => {
                self.line(*line);
                let target = match self.fs().loops.iter().rposition(|l| l.is_loop) {
                    Some(i) => i,
                    None => return Err(self.syntax_error("'continue' outside of a loop")),
                };
                let loop_handlers = self.fs().loops[target].handlers;
                self.leave_try_blocks(loop_handlers)?;
                let j = self.emit(Op::Jump(0));
                self.fs().loops[target].continues.push(j);
            }
            Stmt::Switch(discriminant, cases, line) => {
                self.line(*line);
                self.switch_stmt(discriminant, cases)?;
            }
            Stmt::Throw(e, line) => {
                self.line(*line);
                self.expr(e)?;
                self.emit(Op::Throw);
            }
            Stmt::Try {
                body,
                param,
                handler,
                finalizer,
                line,
            } => {
                self.line(*line);
                self.try_stmt(
                    body,
                    param.as_deref(),
                    handler.as_deref(),
                    finalizer.as_deref(),
                )?;
            }
            Stmt::Block(stmts) => self.block(stmts)?,
        }
        Ok(())
    }

    fn switch_stmt(
        &mut self,
        discriminant: &Expr,
        cases: &[(Option<Expr>, Vec<Stmt>)],
    ) -> Result<(), String> {
        self.enter_scope();
        for (_, body) in cases {
            self.declare_block(body)?;
        }
        self.expr(discriminant)?;
        let value = self.temp();
        self.set_slot(value);
        self.emit(Op::Pop);

        // Dispatch: compare with each case in order, otherwise go to `default` or the end.
        let mut to_body = Vec::with_capacity(cases.len());
        for (test, _) in cases {
            match test {
                Some(t) => {
                    self.get_slot(value);
                    self.expr(t)?;
                    to_body.push(Some(self.emit(Op::JumpIfStrictEq(0))));
                }
                None => to_body.push(None),
            }
        }
        let to_default = self.emit(Op::Jump(0));

        // Bodies run in source order, so a case without `break` falls through to the next.
        let handlers = self.fs().handlers;
        self.fs().loops.push(LoopCtx {
            handlers,
            is_loop: false,
            ..LoopCtx::default()
        });
        let mut default_at = None;
        for (i, (test, body)) in cases.iter().enumerate() {
            let here = self.here() as u32;
            if let Some(j) = to_body[i] {
                self.patch_to(j, here);
            }
            if test.is_none() {
                default_at = Some(here);
            }
            for s in body {
                self.stmt(s)?;
            }
        }
        let end = self.here() as u32;
        self.patch_to(to_default, default_at.unwrap_or(end));
        let ctx = self.loop_pop();
        for b in ctx.breaks {
            self.patch_to(b, end);
        }
        self.exit_scope();
        Ok(())
    }

    /// Pushes call arguments, then emits the call. Spread arguments are gathered into one array first.
    fn call_args(&mut self, args: &[Expr], plain: fn(u32) -> Op, spread: Op) -> Result<(), String> {
        if args.iter().any(|a| matches!(a, Expr::Spread(_))) {
            self.emit(Op::NewArray);
            for a in args {
                match a {
                    Expr::Spread(inner) => {
                        self.expr(inner)?;
                        self.emit(Op::ArrayExtend);
                    }
                    other => {
                        self.expr(other)?;
                        self.emit(Op::ArrayPush);
                    }
                }
            }
            self.emit(spread);
        } else {
            for a in args {
                self.expr(a)?;
            }
            self.emit(plain(args.len() as u32));
        }
        Ok(())
    }

    /// Leaves the `try` blocks entered above `depth` handlers, innermost first: pops their
    /// handlers and runs their `finally` bodies inline. Used by return, break and continue.
    fn leave_try_blocks(&mut self, depth: u32) -> Result<(), String> {
        let at_exit = self.fs().handlers;
        let mut left = Vec::new();
        loop {
            let ctx_handlers = match self.fs().finally_stack.last() {
                Some(ctx) if ctx.handlers >= depth => ctx.handlers,
                _ => break,
            };
            let ctx = self.fs().finally_stack.pop().unwrap();
            self.pop_handlers_to(ctx_handlers);
            self.fs().handlers = ctx_handlers;
            // Compiled with this `try` already left, so a `return` inside it cannot run it again.
            self.block(&ctx.body)?;
            left.push(ctx);
        }
        self.pop_handlers_to(depth);
        self.fs().handlers = at_exit;
        while let Some(ctx) = left.pop() {
            self.fs().finally_stack.push(ctx);
        }
        Ok(())
    }

    /// Emits `PopHandler` for every `try` entered since the given depth (used by break/continue).
    fn pop_handlers_to(&mut self, depth: u32) {
        let current = self.fs().handlers;
        for _ in depth..current {
            self.emit(Op::PopHandler);
        }
    }

    fn for_each(
        &mut self,
        kind: Option<VarKind>,
        name: &str,
        iter: &Expr,
        body: &Stmt,
        line: u32,
        keys: bool,
    ) -> Result<(), String> {
        self.line(line);
        self.enter_scope();
        let list = self.temp();
        let index = self.temp();
        self.expr(iter)?;
        if keys {
            self.emit(Op::ToKeys);
        } else {
            self.emit(Op::ToItems);
        }
        self.set_slot(list);
        self.emit(Op::Pop);
        self.push_num(0.0);
        self.set_slot(index);
        self.emit(Op::Pop);

        let length = self.name_const("length");
        let start = self.here() as u32;
        self.get_slot(index);
        self.get_slot(list);
        self.emit(Op::GetProp(length));
        let exit = self.emit(Op::JumpIfNotLt(0));

        // Per-iteration scope: a `let`/`const` binding is a new cell on every pass.
        self.enter_scope();
        let target = match kind {
            Some(k) => {
                let slot = self.declare(name, k == VarKind::Const);
                if self.slot_is_boxed(slot) {
                    self.emit(Op::NewCell(slot));
                }
                Some(slot)
            }
            None => None,
        };
        self.get_slot(list);
        self.get_slot(index);
        self.emit(Op::GetIndex);
        match target {
            Some(slot) => self.set_slot(slot),
            None => self.store_var(name),
        }
        self.emit(Op::Pop);

        self.loop_push();
        self.stmt(body)?;
        let ctx = self.loop_pop();
        let cont = self.here() as u32;
        for c in ctx.continues {
            self.patch_to(c, cont);
        }
        self.exit_scope();

        self.emit(Op::IncLocal(index));
        self.emit(Op::Jump(start));
        self.patch(exit);
        for b in ctx.breaks {
            self.patch(b);
        }
        self.exit_scope();
        Ok(())
    }

    fn try_stmt(
        &mut self,
        body: &[Stmt],
        param: Option<&str>,
        handler: Option<&[Stmt]>,
        finalizer: Option<&[Stmt]>,
    ) -> Result<(), String> {
        // With a `finally`, an outer handler catches anything thrown by the try or catch parts.
        let outer = match finalizer {
            Some(fin) => {
                let handlers = self.fs().handlers;
                let j = self.emit(Op::PushHandler(0));
                self.fs().handlers += 1;
                self.fs().finally_stack.push(FinallyCtx {
                    handlers,
                    body: fin.to_vec(),
                });
                Some(j)
            }
            None => None,
        };
        match handler {
            Some(catch_body) => {
                let to_catch = self.emit(Op::PushHandler(0));
                self.fs().handlers += 1;
                self.block(body)?;
                self.emit(Op::PopHandler);
                self.fs().handlers -= 1;
                let to_end = self.emit(Op::Jump(0));

                // Entered with the thrown value on the stack.
                self.patch(to_catch);
                self.enter_scope();
                if let Some(name) = param {
                    let slot = self.declare(name, false);
                    if self.slot_is_boxed(slot) {
                        self.emit(Op::NewCell(slot));
                    }
                    self.set_slot(slot);
                }
                self.emit(Op::Pop);
                self.declare_block(catch_body)?;
                for s in catch_body {
                    self.stmt(s)?;
                }
                self.exit_scope();
                self.patch(to_end);
            }
            None => self.block(body)?,
        }

        if let Some(fin) = finalizer {
            self.fs().finally_stack.pop();
            self.emit(Op::PopHandler);
            self.fs().handlers -= 1;
            self.block(fin)?;
            let to_end = self.emit(Op::Jump(0));

            // Exceptional path: run the finalizer, then rethrow the saved value.
            self.patch(outer.unwrap());
            self.enter_scope();
            let saved = self.temp();
            self.set_slot(saved);
            self.emit(Op::Pop);
            self.block(fin)?;
            self.get_slot(saved);
            self.emit(Op::Throw);
            self.exit_scope();
            self.patch(to_end);
        }
        Ok(())
    }

    // ----- expressions ------------------------------------------------------

    fn expr(&mut self, e: &Expr) -> Result<(), String> {
        match e {
            Expr::Num(n) => self.push_num(*n),
            Expr::Str(s) => {
                let c = self.str_const(s);
                self.emit(Op::Const(c));
            }
            Expr::Template(quasis, exprs) => {
                let c = self.str_const(&quasis[0]);
                self.emit(Op::Const(c));
                for (i, part) in exprs.iter().enumerate() {
                    self.expr(part)?;
                    self.emit(Op::ToStr);
                    self.emit(Op::Add);
                    let q = &quasis[i + 1];
                    if !q.is_empty() {
                        let c = self.str_const(q);
                        self.emit(Op::Const(c));
                        self.emit(Op::Add);
                    }
                }
            }
            Expr::Bool(true) => {
                self.emit(Op::True);
            }
            Expr::Bool(false) => {
                self.emit(Op::False);
            }
            Expr::Null => {
                self.emit(Op::Null);
            }
            Expr::Undef => {
                self.emit(Op::Undef);
            }
            Expr::This => {
                self.emit(Op::This);
            }
            Expr::Ident(name) => self.load_var(name),
            Expr::Array(items) => {
                self.emit(Op::NewArray);
                for item in items {
                    match item {
                        Expr::Spread(inner) => {
                            self.expr(inner)?;
                            self.emit(Op::ArrayExtend);
                        }
                        other => {
                            self.expr(other)?;
                            self.emit(Op::ArrayPush);
                        }
                    }
                }
            }
            Expr::Object(props) => {
                self.emit(Op::NewObject(props.len() as u32));
                for (key, value) in props {
                    self.expr(value)?;
                    let c = self.name_const(key);
                    self.emit(Op::InitProp(c));
                }
            }
            Expr::Func(def) => {
                let idx = self.child(def, None)?;
                self.emit(Op::Closure(idx));
            }
            Expr::Unary(op, x) => {
                // `typeof name` must not throw for an undeclared global.
                if let ("typeof", Expr::Ident(name)) = (*op, &**x) {
                    if let Ref::Global = self.resolve(name) {
                        let c = self.name_const(name);
                        self.emit(Op::GetGlobalOrUndef(c));
                        self.emit(Op::TypeOf);
                        return Ok(());
                    }
                }
                if *op == "delete" {
                    // A property is removed from its object, and the result says whether it is gone. Anything that is
                    // not a property is evaluated, and the result is true.
                    match &**x {
                        Expr::Member(obj, name) => {
                            self.expr(obj)?;
                            let c = self.name_const(name);
                            self.emit(Op::DeleteProp(c));
                        }
                        Expr::Index(obj, key) => {
                            self.expr(obj)?;
                            self.expr(key)?;
                            self.emit(Op::DeleteIndex);
                        }
                        other => {
                            self.expr(other)?;
                            self.emit(Op::Pop);
                            self.emit(Op::True);
                        }
                    }
                    return Ok(());
                }
                if *op == "void" {
                    // `void x` evaluates `x`, then yields undefined.
                    self.expr(x)?;
                    self.emit(Op::Pop);
                    self.emit(Op::Undef);
                    return Ok(());
                }
                self.expr(x)?;
                let code = match *op {
                    "-" => Op::Neg,
                    "!" => Op::Not,
                    "+" => Op::ToNumber,
                    "~" => Op::BitNot,
                    _ => Op::TypeOf,
                };
                self.emit(code);
            }
            Expr::Sequence(items) => {
                if let Some((last, rest)) = items.split_last() {
                    for item in rest {
                        self.expr(item)?;
                        self.emit(Op::Pop);
                    }
                    self.expr(last)?;
                }
            }
            Expr::Update { op, prefix, target } => self.update(op, *prefix, target)?,
            Expr::Binary(op, a, b) => {
                if let (Some(fused), Expr::Num(c)) = (fused_local_const(op), &**b) {
                    if let Some(slot) = self.local_operand(a) {
                        let k = self.num_const(*c);
                        self.emit(fused(slot, k));
                        return Ok(());
                    }
                }
                self.expr(a)?;
                self.expr(b)?;
                self.emit(binop(op));
            }
            Expr::Logical(op, a, b) => {
                self.expr(a)?;
                let jump = match *op {
                    "&&" => Op::AndJump(0),
                    "||" => Op::OrJump(0),
                    _ => Op::NullishJump(0),
                };
                let j = self.emit(jump);
                self.expr(b)?;
                self.patch(j);
            }
            Expr::Assign(op, target, value) => self.assign(op, target, value)?,
            Expr::Cond(cond, a, b) => {
                let to_else = self.jump_if_false(cond)?;
                self.expr(a)?;
                let to_end = self.emit(Op::Jump(0));
                self.patch_all(to_else);
                self.expr(b)?;
                self.patch(to_end);
            }
            Expr::Call(callee, args) => {
                // Stack layout for calls: [callee, this, args...]
                match &**callee {
                    Expr::Member(obj, name) => {
                        let c = self.name_const(name);
                        match self.local_operand(obj) {
                            Some(slot) => {
                                self.emit(Op::GetLocalProp(slot, c));
                                self.emit(Op::GetLocal(slot));
                            }
                            None => {
                                self.expr(obj)?;
                                self.emit(Op::Dup);
                                self.emit(Op::GetProp(c));
                                self.emit(Op::Swap);
                            }
                        }
                    }
                    Expr::SuperMember(name) => {
                        // A method of `super` runs with the `this` of the running method.
                        let c = self.name_const(name);
                        self.emit(Op::GetSuper(c));
                        self.emit(Op::This);
                    }
                    Expr::Index(obj, key) => {
                        self.expr(obj)?;
                        self.emit(Op::Dup);
                        self.expr(key)?;
                        self.emit(Op::GetIndex);
                        self.emit(Op::Swap);
                    }
                    other => {
                        self.expr(other)?;
                        self.emit(Op::Undef);
                    }
                }
                self.call_args(args, Op::Call, Op::CallSpread)?;
            }
            Expr::New(callee, args) => {
                self.expr(callee)?;
                self.emit(Op::Undef);
                self.call_args(args, Op::New, Op::NewSpread)?;
            }
            Expr::SuperMember(name) => {
                let c = self.name_const(name);
                self.emit(Op::GetSuper(c));
            }
            Expr::SuperCall(args) => {
                // The superclass is called with this object as `this`, as the constructor of a derived class does.
                self.emit(Op::GetSuperCtor);
                self.emit(Op::This);
                self.call_args(args, Op::Call, Op::CallSpread)?;
            }
            Expr::Spread(_) => {
                return Err(
                    self.syntax_error("spread is only supported in arrays and call arguments")
                )
            }
            Expr::Member(obj, name) => {
                let c = self.name_const(name);
                match self.local_operand(obj) {
                    Some(slot) => {
                        self.emit(Op::GetLocalProp(slot, c));
                    }
                    None => {
                        self.expr(obj)?;
                        self.emit(Op::GetProp(c));
                    }
                }
            }
            Expr::Index(obj, key) => {
                // `local[key]` and `local[k1][k2]` read the array in its slot, without copying it (see `GetIndexLocal`).
                // The second key of the two-level form must not call anything, since the first read may throw.
                if let Some(slot) = self.local_operand(obj) {
                    if !may_assign(key) {
                        self.expr(key)?;
                        self.emit(Op::GetIndexLocal(slot));
                        return Ok(());
                    }
                }
                if let Expr::Index(inner, k1) = &**obj {
                    if let Some(slot) = self.local_operand(inner) {
                        if !may_assign(k1) && self.simple_operand(key) {
                            self.expr(k1)?;
                            self.expr(key)?;
                            self.emit(Op::GetIndex2Local(slot));
                            return Ok(());
                        }
                    }
                }
                self.expr(obj)?;
                self.expr(key)?;
                self.emit(Op::GetIndex);
            }
        }
        Ok(())
    }

    /// Emits a jump that is taken when `cond` is false. The caller patches its target.
    /// Comparisons fuse with the jump, and comparisons on plain locals fuse entirely.
    /// Points each of `jumps` at the next instruction.
    fn patch_all(&mut self, jumps: Vec<usize>) {
        for j in jumps {
            self.patch(j);
        }
    }

    /// Emits the jumps that are taken when `cond` is false. The caller patches their targets. A `&&` needs
    /// one jump per operand, so that each operand is tested on its own, without a value in between.
    fn jump_if_false(&mut self, cond: &Expr) -> Result<Vec<usize>, String> {
        if let Expr::Logical("&&", a, b) = cond {
            let mut jumps = self.jump_if_false(a)?;
            jumps.extend(self.jump_if_false(b)?);
            return Ok(jumps);
        }
        if let Expr::Unary("!", x) = cond {
            return Ok(vec![self.jump_if_true(x)?]);
        }
        if let Expr::Binary(op, a, b) = cond {
            if *op == "<" {
                if let Some(slot) = self.local_operand(a) {
                    if let Expr::Num(c) = &**b {
                        let k = self.num_const(*c);
                        return Ok(vec![self.emit(Op::JumpIfNotLtLK(slot, k, 0))]);
                    }
                    if let Some(other) = self.local_operand(b) {
                        return Ok(vec![self.emit(Op::JumpIfNotLtLL(slot, other, 0))]);
                    }
                }
            }
            let fused = match *op {
                "<" => Some(Op::JumpIfNotLt(0)),
                "<=" => Some(Op::JumpIfNotLe(0)),
                ">" => Some(Op::JumpIfNotGt(0)),
                ">=" => Some(Op::JumpIfNotGe(0)),
                "===" => Some(Op::JumpIfNotStrictEq(0)),
                "!==" => Some(Op::JumpIfStrictEq(0)),
                _ => None,
            };
            if let Some(jump) = fused {
                self.expr(a)?;
                self.expr(b)?;
                return Ok(vec![self.emit(jump)]);
            }
        }
        self.expr(cond)?;
        Ok(vec![self.emit(Op::JumpIfFalse(0))])
    }

    /// Emits a jump that is taken when `cond` is true. The caller patches its target. A `&&` skips to the code
    /// after the jump when its first operand is false, and jumps when its second operand is true.
    fn jump_if_true(&mut self, cond: &Expr) -> Result<usize, String> {
        if let Expr::Logical("&&", a, b) = cond {
            let skip = self.jump_if_false(a)?;
            let back = self.jump_if_true(b)?;
            for j in skip {
                self.patch(j);
            }
            return Ok(back);
        }
        if let Expr::Binary(op, a, b) = cond {
            if *op == "<" {
                if let Some(slot) = self.local_operand(a) {
                    if let Expr::Num(c) = &**b {
                        let k = self.num_const(*c);
                        return Ok(self.emit(Op::JumpIfLtLK(slot, k, 0)));
                    }
                    if let Some(other) = self.local_operand(b) {
                        return Ok(self.emit(Op::JumpIfLtLL(slot, other, 0)));
                    }
                }
            }
            // The comparison jumps on its false result past the jump that is taken when it is true. A NaN operand makes
            // every ordering false, as the fused jump on false does.
            let not_true = match *op {
                "<" => Some(Op::JumpIfNotLt(0)),
                "<=" => Some(Op::JumpIfNotLe(0)),
                ">" => Some(Op::JumpIfNotGt(0)),
                ">=" => Some(Op::JumpIfNotGe(0)),
                _ => None,
            };
            // An equality jumps on its true result directly.
            let equal = match *op {
                "===" => Some(Op::JumpIfStrictEq(0)),
                "!==" => Some(Op::JumpIfNotStrictEq(0)),
                _ => None,
            };
            if let Some(jump) = equal {
                self.expr(a)?;
                self.expr(b)?;
                return Ok(self.emit(jump));
            }
            if let Some(skip) = not_true {
                self.expr(a)?;
                self.expr(b)?;
                let past = self.emit(skip);
                let back = self.emit(Op::Jump(0));
                self.patch(past);
                return Ok(back);
            }
        }
        self.expr(cond)?;
        Ok(self.emit(Op::JumpIfTrue(0)))
    }

    /// Compiles an expression whose value is discarded. Assignments to plain locals and
    /// increments of them write the local directly, without leaving a value to pop.
    fn expr_discard(&mut self, e: &Expr) -> Result<(), String> {
        if let Expr::Update { op, target, .. } = e {
            if let Expr::Ident(name) = &**target {
                if let Some(slot) = self.plain_local(name) {
                    let step = if *op == "++" {
                        Op::IncLocal(slot)
                    } else {
                        Op::DecLocal(slot)
                    };
                    self.emit(step);
                    return Ok(());
                }
            }
        }
        if let Expr::Assign(op, target, value) = e {
            if let Expr::Ident(name) = &**target {
                if *op == "=" || *op == "+=" {
                    if let Some(slot) = self.plain_local(name) {
                        self.assign_plain_local(op, name, slot, value)?;
                        return Ok(());
                    }
                }
            }
        }
        match e {
            Expr::Update { op, target, .. } => self.update(op, true, target)?,
            other => self.expr(other)?,
        }
        self.emit(Op::Pop);
        Ok(())
    }

    /// `x = e` or `x += e` as a statement, for a plain local `x` at `slot`.
    fn assign_plain_local(
        &mut self,
        op: &str,
        name: &str,
        slot: u32,
        value: &Expr,
    ) -> Result<(), String> {
        if op == "=" {
            if let Expr::Binary("+", lhs, rhs) = value {
                let reads_itself = matches!(&**lhs, Expr::Ident(n) if n.as_str() == name);
                if reads_itself && is_pure(rhs) {
                    // `x = x + e` with a side-effect-free `e` is the same as `x += e`.
                    self.expr(rhs)?;
                    self.emit(Op::AddToLocal(slot));
                    return Ok(());
                }
            }
            self.expr(value)?;
            self.emit(Op::PopToLocal(slot));
        } else if is_pure(value) {
            self.expr(value)?;
            self.emit(Op::AddToLocal(slot));
        } else {
            // The right side might assign to `x`, so the old value is read first.
            self.get_slot(slot);
            self.expr(value)?;
            self.emit(Op::ConcatLocal(slot));
            self.emit(Op::Pop);
        }
        Ok(())
    }

    fn assign(&mut self, op: &str, target: &Expr, value: &Expr) -> Result<(), String> {
        // `x += e` and `x = x + e` on a plain local append in place (see `Op::ConcatLocal`).
        if let Expr::Ident(name) = target {
            if let Some(rhs) = concat_rhs(op, name, value) {
                if let Ref::Local {
                    slot,
                    is_const: false,
                } = self.resolve(name)
                {
                    if !self.slot_is_boxed(slot) {
                        self.get_slot(slot);
                        self.expr(rhs)?;
                        self.emit(Op::ConcatLocal(slot));
                        return Ok(());
                    }
                }
            }
        }
        let compound = if op == "=" {
            None
        } else {
            Some(binop(&op[..op.len() - 1]))
        };
        match target {
            Expr::Ident(name) => {
                if let Some(b) = compound {
                    self.load_var(name);
                    self.expr(value)?;
                    self.emit(b);
                } else {
                    self.expr(value)?;
                }
                self.store_var(name);
            }
            Expr::Member(obj, name) => {
                self.expr(obj)?;
                let c = self.name_const(name);
                if let Some(b) = compound {
                    self.emit(Op::Dup);
                    self.emit(Op::GetProp(c));
                    self.expr(value)?;
                    self.emit(b);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::SetProp(c));
            }
            Expr::Index(obj, key) => {
                if compound.is_none() {
                    // `local[key] = value` and `local[k1][k2] = value` write in place (see `SetIndexLocal`). The
                    // two-level form needs simple operands, since the first read may throw before they run.
                    if let Some(slot) = self.local_operand(obj) {
                        if !may_assign(key) && !may_assign(value) {
                            self.expr(key)?;
                            self.expr(value)?;
                            self.emit(Op::SetIndexLocal(slot));
                            return Ok(());
                        }
                    }
                    if let Expr::Index(inner, k1) = &**obj {
                        if let Some(slot) = self.local_operand(inner) {
                            if !may_assign(k1)
                                && self.simple_operand(key)
                                && self.simple_operand(value)
                            {
                                self.expr(k1)?;
                                self.expr(key)?;
                                self.expr(value)?;
                                self.emit(Op::SetIndex2Local(slot));
                                return Ok(());
                            }
                        }
                    }
                }
                self.expr(obj)?;
                self.expr(key)?;
                if let Some(b) = compound {
                    self.emit(Op::Dup2);
                    self.emit(Op::GetIndex);
                    self.expr(value)?;
                    self.emit(b);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::SetIndex);
            }
            _ => return Err(self.syntax_error("invalid assignment target")),
        }
        Ok(())
    }

    fn update(&mut self, op: &str, prefix: bool, target: &Expr) -> Result<(), String> {
        let step = if op == "++" { Op::Add } else { Op::Sub };
        let mark = self.fs().next_slot;
        match target {
            Expr::Ident(name) => {
                self.load_var(name);
                if prefix {
                    self.push_num(1.0);
                    self.emit(step);
                    self.store_var(name);
                } else {
                    // Keep the old value as the result, store the new one.
                    self.emit(Op::ToNumber);
                    self.emit(Op::Dup);
                    self.push_num(1.0);
                    self.emit(step);
                    self.store_var(name);
                    self.emit(Op::Pop);
                }
            }
            Expr::Member(obj, name) => {
                let c = self.name_const(name);
                if prefix {
                    self.expr(obj)?;
                    self.emit(Op::Dup);
                    self.emit(Op::GetProp(c));
                    self.push_num(1.0);
                    self.emit(step);
                    self.emit(Op::SetProp(c));
                } else {
                    let obj_slot = self.temp();
                    self.expr(obj)?;
                    self.set_slot(obj_slot);
                    self.emit(Op::Pop);
                    self.get_slot(obj_slot);
                    self.emit(Op::GetProp(c));
                    self.emit(Op::ToNumber);
                    self.emit(Op::Dup);
                    self.push_num(1.0);
                    self.emit(step);
                    self.get_slot(obj_slot);
                    self.emit(Op::Swap);
                    self.emit(Op::SetProp(c));
                    self.emit(Op::Pop);
                }
            }
            Expr::Index(obj, key) => {
                if prefix {
                    self.expr(obj)?;
                    self.expr(key)?;
                    self.emit(Op::Dup2);
                    self.emit(Op::GetIndex);
                    self.push_num(1.0);
                    self.emit(step);
                    self.emit(Op::SetIndex);
                } else {
                    let obj_slot = self.temp();
                    let key_slot = self.temp();
                    let new_slot = self.temp();
                    self.expr(obj)?;
                    self.set_slot(obj_slot);
                    self.emit(Op::Pop);
                    self.expr(key)?;
                    self.set_slot(key_slot);
                    self.emit(Op::Pop);
                    self.get_slot(obj_slot);
                    self.get_slot(key_slot);
                    self.emit(Op::GetIndex);
                    self.emit(Op::ToNumber);
                    self.emit(Op::Dup);
                    self.push_num(1.0);
                    self.emit(step);
                    self.set_slot(new_slot);
                    self.emit(Op::Pop);
                    self.get_slot(obj_slot);
                    self.get_slot(key_slot);
                    self.get_slot(new_slot);
                    self.emit(Op::SetIndex);
                    self.emit(Op::Pop);
                }
            }
            _ => return Err(self.syntax_error("invalid update target")),
        }
        self.fs().next_slot = mark;
        Ok(())
    }
}
