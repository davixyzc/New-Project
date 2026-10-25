//! The virtual machine: call frames, the dispatch loop, property access,
//! error objects and the timer-based event loop.

use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::{BufWriter, Stdout, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::builtins::{self, console, string};
use crate::bytecode::{Op, Proto};
use crate::compiler::compile_module;
use crate::value::*;

/// Maximum nesting of JavaScript-level calls before a RangeError is thrown.
const MAX_FRAMES: usize = 10_000;
/// Largest array index that `arr[i] = v` will grow the array to.
const MAX_ARRAY_GROW: usize = 50_000_000;
/// JavaScript calls made from Rust (e.g. a `map` callback) recurse on the Rust stack,
/// so they are capped separately from `MAX_FRAMES`.
const MAX_NESTED_CALLS: usize = 2_000;

pub struct Frame {
    pub closure: Rc<Closure>,
    /// Next instruction to run. Only kept up to date when the frame is not running.
    pub ip: usize,
    /// Index of the first argument (slot 0) on the value stack.
    pub base: usize,
    pub this: Value,
    /// Active `try` handlers: (catch instruction index, stack height to restore).
    pub handlers: Vec<(usize, usize)>,
    /// Set for frames started with `new`: the object to return if the constructor returns nothing.
    pub ctor: Option<Value>,
}

pub struct Timer {
    pub callback: Value,
    pub args: Vec<Value>,
    pub interval: Option<Duration>,
}

pub struct Vm {
    pub stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub globals: HashMap<Rc<str>, Value>,
    /// Module objects (`{ exports }`) keyed by absolute path, so each file runs once.
    pub modules: HashMap<PathBuf, Value>,
    pub out: BufWriter<Stdout>,
    /// Callbacks queued by `process.nextTick`. They run before any timer.
    pub ticks: VecDeque<(Value, Vec<Value>)>,
    /// Min-heap of (due time, timer id).
    pub timers: BinaryHeap<Reverse<(Instant, u64)>>,
    pub timer_table: HashMap<u64, Timer>,
    pub next_timer: u64,
    rng: u64,
    nested_calls: usize,
    pub argv: Vec<String>,
}

fn const_name(proto: &Proto, i: u32) -> &str {
    match &proto.consts[i as usize] {
        Value::Str(s) => s.as_str(),
        _ => "",
    }
}

fn index_of(n: f64) -> Option<usize> {
    if n >= 0.0 && n < MAX_ARRAY_GROW as f64 && n as usize as f64 == n {
        Some(n as usize)
    } else {
        None
    }
}

impl Vm {
    pub fn new(argv: Vec<String>) -> Vm {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        let mut vm = Vm {
            stack: Vec::with_capacity(1024),
            frames: Vec::new(),
            globals: HashMap::new(),
            modules: HashMap::new(),
            out: BufWriter::new(std::io::stdout()),
            ticks: VecDeque::new(),
            timers: BinaryHeap::new(),
            timer_table: HashMap::new(),
            next_timer: 1,
            rng: seed | 1,
            nested_calls: 0,
            argv,
        };
        builtins::install(&mut vm);
        vm
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        self.globals.insert(Rc::from(name), v);
    }

    pub fn write_out(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
    }

    pub fn flush(&mut self) {
        let _ = self.out.flush();
    }

    /// xorshift64* generator, returns a number in [0, 1).
    pub fn random(&mut self) -> f64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Directory of the module that is currently running (used to resolve `require`).
    pub fn current_dir(&self) -> PathBuf {
        match self.frames.last() {
            Some(f) => Path::new(&*f.closure.proto.file)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            None => std::env::current_dir().unwrap_or_default(),
        }
    }

    // ----- entry points -----------------------------------------------------

    /// Runs the script at `path` and then the event loop. Returns the process exit code.
    pub fn run_main(&mut self, path: &Path) -> i32 {
        let result = self.load_module(path);
        self.finish(result)
    }

    /// Runs inline source (`fly -e`) and then the event loop. Returns the process exit code.
    pub fn run_source(&mut self, path: &Path, src: &str) -> i32 {
        let result = self.eval_source(path, src);
        self.finish(result)
    }

    /// Drains the event loop after the main code and reports anything that escaped.
    fn finish(&mut self, result: Result<Value, Value>) -> i32 {
        let result = result.and_then(|_| self.run_event_loop());
        let code = match result {
            Ok(()) => 0,
            Err(e) => {
                self.flush();
                eprintln!("Uncaught {}", console::inspect(&e, true, 0));
                1
            }
        };
        self.flush();
        code
    }

    /// Loads a module by path, running it once and caching its `module` object.
    pub fn load_module(&mut self, path: &Path) -> Result<Value, Value> {
        if let Some(module) = self.modules.get(path).cloned() {
            return Ok(Self::exports_of(&module));
        }
        match std::fs::read_to_string(path) {
            Ok(src) => self.eval_source(path, &src),
            Err(_) => {
                Err(self.make_error("Error", &format!("Cannot find module '{}'", path.display())))
            }
        }
    }

    /// Compiles `src` as the body of module `path` and runs it. Returns `module.exports`.
    pub fn eval_source(&mut self, path: &Path, src: &str) -> Result<Value, Value> {
        let filename = path.display().to_string();
        let proto = match compile_module(&filename, src) {
            Ok(p) => p,
            Err(msg) => {
                let text = msg
                    .strip_prefix("SyntaxError: ")
                    .unwrap_or(&msg)
                    .to_string();
                return Err(self.make_error("SyntaxError", &text));
            }
        };
        let module = Value::obj();
        let exports = Value::obj();
        if let Value::Obj(m) = &module {
            obj_set(m, "exports", exports.clone());
        }
        self.modules.insert(path.to_path_buf(), module.clone());

        let dir = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let func = Value::Func(Rc::new(Closure {
            proto: Rc::new(proto),
            upvals: Vec::new(),
            this: Value::Undef,
        }));
        let args = vec![
            exports,
            builtins::global::require_fn(),
            module.clone(),
            Value::str(&filename),
            Value::str(&dir),
        ];
        self.call(&func, Value::Undef, args)?;
        Ok(Self::exports_of(&module))
    }

    fn exports_of(module: &Value) -> Value {
        match module {
            Value::Obj(m) => obj_get(m, "exports").unwrap_or(Value::Undef),
            _ => Value::Undef,
        }
    }

    // ----- calls ------------------------------------------------------------

    /// Calls any callable value from Rust (used by natives such as `Array.prototype.map`).
    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> Result<Value, Value> {
        match f {
            Value::Native(def) => (def.func)(self, this, args),
            Value::Func(c) => {
                if self.nested_calls >= MAX_NESTED_CALLS {
                    return Err(self.make_error("RangeError", "Maximum call stack size exceeded"));
                }
                self.nested_calls += 1;
                let result = self.call_closure(c.clone(), this, args);
                self.nested_calls -= 1;
                result
            }
            other => {
                Err(self.make_error("TypeError", &format!("{} is not a function", to_str(other))))
            }
        }
    }

    fn call_closure(
        &mut self,
        c: Rc<Closure>,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Value, Value> {
        let depth = self.frames.len();
        let argc = args.len();
        self.stack.push(Value::Undef);
        self.stack.push(this);
        self.stack.extend(args);
        self.enter(c, argc, None)?;
        self.execute(depth)?;
        Ok(self.pop())
    }

    /// Pushes a frame for a closure. Arguments are already on the stack above the callee and `this` slots.
    fn enter(
        &mut self,
        closure: Rc<Closure>,
        argc: usize,
        ctor: Option<Value>,
    ) -> Result<(), Value> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(self.make_error("RangeError", "Maximum call stack size exceeded"));
        }
        let proto = &closure.proto;
        let base = self.stack.len() - argc;
        let fixed = proto.nparams as usize;
        if proto.has_rest {
            let extra: Vec<Value> = if argc > fixed {
                self.stack.drain(base + fixed..).collect()
            } else {
                Vec::new()
            };
            self.stack.resize(base + fixed, Value::Undef);
            self.stack.push(Value::arr(extra));
        } else {
            self.stack.resize(base + fixed, Value::Undef);
        }
        let target = base + proto.nlocals as usize;
        while self.stack.len() < target {
            self.stack.push(Value::Undef);
        }
        let this = if proto.is_arrow {
            closure.this.clone()
        } else {
            self.stack[base - 1].clone()
        };
        self.frames.push(Frame {
            closure,
            ip: 0,
            base,
            this,
            handlers: Vec::new(),
            ctor,
        });
        Ok(())
    }

    /// Calls or constructs the value under the arguments. The stack holds [callee, this, args...].
    fn invoke(&mut self, callee: Value, argc: usize, is_new: bool) -> Result<(), Value> {
        match callee {
            Value::Func(c) => {
                let ctor = if is_new {
                    let obj = Value::obj();
                    let n = self.stack.len();
                    self.stack[n - argc - 1] = obj.clone();
                    Some(obj)
                } else {
                    None
                };
                self.enter(c, argc, ctor)
            }
            Value::Native(def) => {
                let args = self.stack.split_off(self.stack.len() - argc);
                let this = self.pop();
                self.stack.pop();
                let this = if is_new { Value::Undef } else { this };
                let result = (def.func)(self, this, args)?;
                self.stack.push(result);
                Ok(())
            }
            other => {
                let what = if is_new {
                    "is not a constructor"
                } else {
                    "is not a function"
                };
                Err(self.make_error("TypeError", &format!("{} {}", to_str(&other), what)))
            }
        }
    }

    /// Runs frames until the frame at `base_depth` returns, or an exception escapes it.
    fn execute(&mut self, base_depth: usize) -> Result<(), Value> {
        loop {
            match self.run(base_depth) {
                Ok(()) => return Ok(()),
                Err(exc) => self.unwind(exc, base_depth)?,
            }
        }
    }

    /// Transfers control to the nearest handler above `base_depth`, or returns the exception.
    fn unwind(&mut self, exc: Value, base_depth: usize) -> Result<(), Value> {
        while self.frames.len() > base_depth {
            let f = self.frames.last_mut().unwrap();
            if let Some((catch_ip, height)) = f.handlers.pop() {
                f.ip = catch_ip;
                self.stack.truncate(height);
                self.stack.push(exc);
                return Ok(());
            }
            let f = self.frames.pop().unwrap();
            self.stack.truncate(f.base - 2);
        }
        Err(exc)
    }

    #[inline(always)]
    fn pop(&mut self) -> Value {
        self.stack.pop().unwrap_or(Value::Undef)
    }

    #[inline(always)]
    fn top(&self) -> &Value {
        self.stack.last().unwrap()
    }

    /// The two numbers on top of the stack, if both are numbers. Peeks, so nothing is dropped.
    #[inline(always)]
    fn num_pair(&self) -> Option<(f64, f64)> {
        let n = self.stack.len();
        match (&self.stack[n - 2], &self.stack[n - 1]) {
            (Value::Num(x), Value::Num(y)) => Some((*x, *y)),
            _ => None,
        }
    }

    /// Replaces the two operands on top of the stack with one value.
    #[inline(always)]
    fn replace_top2(&mut self, v: Value) {
        self.stack.pop();
        let n = self.stack.len();
        self.stack[n - 1] = v;
    }

    fn pop_pair(&mut self) -> (Value, Value) {
        let b = self.pop();
        let a = self.pop();
        (a, b)
    }

    /// Writes the instruction pointer back to the current frame.
    #[inline(always)]
    fn sync_ip(&mut self, ip: usize) {
        self.frames.last_mut().unwrap().ip = ip;
    }

    /// Code, base and instruction pointer of the innermost frame.
    fn frame_state(&self) -> (Rc<Proto>, usize, usize) {
        let f = self.frames.last().unwrap();
        (f.closure.proto.clone(), f.base, f.ip)
    }

    /// `++` / `--` on a plain numeric local, in place.
    #[inline(always)]
    fn step_local(&mut self, idx: usize, delta: f64) {
        match &mut self.stack[idx] {
            Value::Num(n) => *n += delta,
            other => {
                let n = to_num(other) + delta;
                *other = Value::Num(n);
            }
        }
    }

    /// The dispatch loop. Returns Ok when the frame at `base_depth` returns, Err on a thrown value.
    ///
    /// The instruction pointer lives in a local. It is written back to the frame before
    /// anything that may call, return or raise, because those paths read it from the frame.
    fn run(&mut self, base_depth: usize) -> Result<(), Value> {
        let (mut proto, mut base, mut ip) = self.frame_state();
        loop {
            let op = proto.code[ip];
            ip += 1;
            match op {
                Op::Const(i) => {
                    let v = proto.consts[i as usize].clone();
                    self.stack.push(v);
                }
                Op::Undef => self.stack.push(Value::Undef),
                Op::Null => self.stack.push(Value::Null),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::This => {
                    let this = self.frames.last().unwrap().this.clone();
                    self.stack.push(this);
                }
                Op::Pop => {
                    self.stack.pop();
                }
                Op::Dup => {
                    let v = self.top().clone();
                    self.stack.push(v);
                }
                Op::Swap => {
                    let n = self.stack.len();
                    self.stack.swap(n - 1, n - 2);
                }
                Op::Dup2 => {
                    let n = self.stack.len();
                    let a = self.stack[n - 2].clone();
                    let b = self.stack[n - 1].clone();
                    self.stack.push(a);
                    self.stack.push(b);
                }

                Op::GetLocal(i) => {
                    let v = self.stack[base + i as usize].clone();
                    self.stack.push(v);
                }
                Op::SetLocal(i) => {
                    let v = self.top().clone();
                    self.stack[base + i as usize] = v;
                }
                Op::IncLocal(i) => self.step_local(base + i as usize, 1.0),
                Op::DecLocal(i) => self.step_local(base + i as usize, -1.0),
                Op::NewCell(i) => {
                    self.stack[base + i as usize] =
                        Value::Cell(Rc::new(RefCell::new(Value::Undef)));
                }
                Op::GetBoxed(i) => {
                    let v = match &self.stack[base + i as usize] {
                        Value::Cell(c) => c.borrow().clone(),
                        other => other.clone(),
                    };
                    self.stack.push(v);
                }
                Op::SetBoxed(i) => {
                    let idx = base + i as usize;
                    let v = self.top().clone();
                    match &self.stack[idx] {
                        Value::Cell(c) => *c.borrow_mut() = v,
                        _ => self.stack[idx] = Value::Cell(Rc::new(RefCell::new(v))),
                    }
                }
                Op::BoxParam(i) => {
                    let idx = base + i as usize;
                    let v = std::mem::replace(&mut self.stack[idx], Value::Undef);
                    self.stack[idx] = Value::Cell(Rc::new(RefCell::new(v)));
                }
                Op::Rebox(i) => {
                    let idx = base + i as usize;
                    let v = match &self.stack[idx] {
                        Value::Cell(c) => c.borrow().clone(),
                        other => other.clone(),
                    };
                    self.stack[idx] = Value::Cell(Rc::new(RefCell::new(v)));
                }
                Op::ConcatLocal(i) => {
                    let rhs = self.pop();
                    let old = self.pop();
                    let idx = base + i as usize;
                    let result = match (&old, &rhs) {
                        (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
                        _ => {
                            // Drop the variable's own reference first, so `old` can be extended in place.
                            let released = match &mut self.stack[idx] {
                                Value::Cell(c) => {
                                    std::mem::replace(&mut *c.borrow_mut(), Value::Undef)
                                }
                                slot => std::mem::replace(slot, Value::Undef),
                            };
                            drop(released);
                            concat_owned(old, rhs)
                        }
                    };
                    match &mut self.stack[idx] {
                        Value::Cell(c) => *c.borrow_mut() = result.clone(),
                        slot => *slot = result.clone(),
                    }
                    self.stack.push(result);
                }

                Op::GetUpval(i) => {
                    let v = self.frames.last().unwrap().closure.upvals[i as usize]
                        .borrow()
                        .clone();
                    self.stack.push(v);
                }
                Op::SetUpval(i) => {
                    let v = self.top().clone();
                    *self.frames.last().unwrap().closure.upvals[i as usize].borrow_mut() = v;
                }

                Op::GetGlobal(i) => {
                    self.sync_ip(ip);
                    let name = const_name(&proto, i);
                    let found = self.globals.get(name).cloned();
                    match found {
                        Some(v) => self.stack.push(v),
                        None => {
                            return Err(self
                                .make_error("ReferenceError", &format!("{} is not defined", name)))
                        }
                    }
                }
                Op::GetGlobalOrUndef(i) => {
                    let v = self
                        .globals
                        .get(const_name(&proto, i))
                        .cloned()
                        .unwrap_or(Value::Undef);
                    self.stack.push(v);
                }
                Op::SetGlobal(i) => {
                    let name = const_name(&proto, i);
                    let v = self.top().clone();
                    self.globals.insert(Rc::from(name), v);
                }
                Op::ConstAssign(_) => {
                    self.sync_ip(ip);
                    return Err(self.make_error("TypeError", "Assignment to constant variable."));
                }

                Op::GetProp(i) => {
                    self.sync_ip(ip);
                    let obj = self.pop();
                    let v = self.get_prop(&obj, const_name(&proto, i))?;
                    self.stack.push(v);
                }
                Op::SetProp(i) => {
                    self.sync_ip(ip);
                    let v = self.pop();
                    let obj = self.pop();
                    self.set_prop(&obj, const_name(&proto, i), v.clone())?;
                    self.stack.push(v);
                }
                Op::GetIndex => {
                    self.sync_ip(ip);
                    let (obj, key) = self.pop_pair();
                    let v = self.get_index(&obj, &key)?;
                    self.stack.push(v);
                }
                Op::SetIndex => {
                    self.sync_ip(ip);
                    let v = self.pop();
                    let key = self.pop();
                    let obj = self.pop();
                    self.set_index(&obj, &key, v.clone())?;
                    self.stack.push(v);
                }

                Op::Add => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Num(x + y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(add(&a, &b));
                    }
                },
                Op::Sub => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Num(x - y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Num(to_num(&a) - to_num(&b)));
                    }
                },
                Op::Mul => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Num(x * y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Num(to_num(&a) * to_num(&b)));
                    }
                },
                Op::Div => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Num(x / y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Num(to_num(&a) / to_num(&b)));
                    }
                },
                Op::Mod => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Num(js_mod(x, y))),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Num(js_mod(to_num(&a), to_num(&b))));
                    }
                },
                Op::Pow => {
                    let (a, b) = self.pop_pair();
                    self.stack.push(Value::Num(to_num(&a).powf(to_num(&b))));
                }
                Op::Eq => {
                    let (a, b) = self.pop_pair();
                    self.stack.push(Value::Bool(loose_eq(&a, &b)));
                }
                Op::Ne => {
                    let (a, b) = self.pop_pair();
                    self.stack.push(Value::Bool(!loose_eq(&a, &b)));
                }
                Op::StrictEq => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x == y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Bool(strict_eq(&a, &b)));
                    }
                },
                Op::StrictNe => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x != y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack.push(Value::Bool(!strict_eq(&a, &b)));
                    }
                },
                Op::Lt => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x < y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack
                            .push(Value::Bool(compare(&a, &b) == Some(Ordering::Less)));
                    }
                },
                Op::Le => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x <= y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        let r = matches!(compare(&a, &b), Some(Ordering::Less | Ordering::Equal));
                        self.stack.push(Value::Bool(r));
                    }
                },
                Op::Gt => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x > y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        self.stack
                            .push(Value::Bool(compare(&a, &b) == Some(Ordering::Greater)));
                    }
                },
                Op::Ge => match self.num_pair() {
                    Some((x, y)) => self.replace_top2(Value::Bool(x >= y)),
                    None => {
                        let (a, b) = self.pop_pair();
                        let r =
                            matches!(compare(&a, &b), Some(Ordering::Greater | Ordering::Equal));
                        self.stack.push(Value::Bool(r));
                    }
                },
                Op::Neg => {
                    let a = self.pop();
                    self.stack.push(Value::Num(-to_num(&a)));
                }
                Op::Not => {
                    let a = self.pop();
                    self.stack.push(Value::Bool(!truthy(&a)));
                }
                Op::ToNumber => {
                    let a = self.pop();
                    self.stack.push(Value::Num(to_num(&a)));
                }
                Op::TypeOf => {
                    let a = self.pop();
                    self.stack.push(Value::str(type_of(&a)));
                }
                Op::ToStr => {
                    let a = self.pop();
                    let s = match a {
                        Value::Str(_) => a,
                        other => Value::str(&to_str(&other)),
                    };
                    self.stack.push(s);
                }

                Op::Jump(t) => ip = t as usize,
                Op::JumpIfFalse(t) => {
                    if !truthy(&self.pop()) {
                        ip = t as usize;
                    }
                }
                Op::JumpIfTrue(t) => {
                    if truthy(&self.pop()) {
                        ip = t as usize;
                    }
                }
                Op::JumpIfNotLt(t) => {
                    let less = match self.num_pair() {
                        Some((x, y)) => x < y,
                        None => {
                            let (a, b) = self.pop_pair();
                            compare(&a, &b) == Some(Ordering::Less)
                        }
                    };
                    self.stack.truncate(self.stack.len() - 2);
                    if !less {
                        ip = t as usize;
                    }
                }
                Op::JumpIfNotLe(t) => {
                    let (a, b) = self.pop_pair();
                    if !matches!(compare(&a, &b), Some(Ordering::Less | Ordering::Equal)) {
                        ip = t as usize;
                    }
                }
                Op::JumpIfNotGt(t) => {
                    let (a, b) = self.pop_pair();
                    if compare(&a, &b) != Some(Ordering::Greater) {
                        ip = t as usize;
                    }
                }
                Op::JumpIfNotGe(t) => {
                    let (a, b) = self.pop_pair();
                    if !matches!(compare(&a, &b), Some(Ordering::Greater | Ordering::Equal)) {
                        ip = t as usize;
                    }
                }
                Op::AndJump(t) => {
                    if truthy(self.top()) {
                        self.pop();
                    } else {
                        ip = t as usize;
                    }
                }
                Op::OrJump(t) => {
                    if truthy(self.top()) {
                        ip = t as usize;
                    } else {
                        self.pop();
                    }
                }
                Op::NullishJump(t) => {
                    if self.top().is_nullish() {
                        self.pop();
                    } else {
                        ip = t as usize;
                    }
                }

                Op::Call(argc) => {
                    self.sync_ip(ip);
                    let argc = argc as usize;
                    let callee = self.stack[self.stack.len() - argc - 2].clone();
                    self.invoke(callee, argc, false)?;
                    (proto, base, ip) = self.frame_state();
                }
                Op::New(argc) => {
                    self.sync_ip(ip);
                    let argc = argc as usize;
                    let callee = self.stack[self.stack.len() - argc - 2].clone();
                    self.invoke(callee, argc, true)?;
                    (proto, base, ip) = self.frame_state();
                }
                Op::Return => {
                    let v = self.pop();
                    let f = self.frames.pop().unwrap();
                    self.stack.truncate(f.base - 2);
                    let v = match f.ctor {
                        Some(obj) if !v.is_object() => obj,
                        _ => v,
                    };
                    self.stack.push(v);
                    if self.frames.len() == base_depth {
                        return Ok(());
                    }
                    (proto, base, ip) = self.frame_state();
                }

                Op::Closure(i) => {
                    let child = proto.protos[i as usize].clone();
                    let frame = self.frames.last().unwrap();
                    let mut upvals = Vec::with_capacity(child.upvals.len());
                    for d in &child.upvals {
                        if d.from_local {
                            let cell = match &self.stack[frame.base + d.index as usize] {
                                Value::Cell(c) => c.clone(),
                                other => Rc::new(RefCell::new(other.clone())),
                            };
                            upvals.push(cell);
                        } else {
                            upvals.push(frame.closure.upvals[d.index as usize].clone());
                        }
                    }
                    let this = if child.is_arrow {
                        frame.this.clone()
                    } else {
                        Value::Undef
                    };
                    self.stack.push(Value::Func(Rc::new(Closure {
                        proto: child,
                        upvals,
                        this,
                    })));
                }
                Op::NewArray => self.stack.push(Value::arr(Vec::new())),
                Op::ArrayExtend => {
                    let src = self.pop();
                    let items: Vec<Value> = match &src {
                        Value::Arr(a) => a.borrow().clone(),
                        Value::Str(s) => s.chars().map(|c| Value::str(&c.to_string())).collect(),
                        _ => {
                            self.sync_ip(ip);
                            return Err(self.make_error("TypeError", "value is not iterable"));
                        }
                    };
                    if let Some(Value::Arr(a)) = self.stack.last() {
                        a.borrow_mut().extend(items);
                    }
                }
                Op::CallSpread | Op::NewSpread => {
                    self.sync_ip(ip);
                    let is_new = op == Op::NewSpread;
                    let list = self.pop();
                    let items: Vec<Value> = match &list {
                        Value::Arr(a) => a.borrow().clone(),
                        _ => Vec::new(),
                    };
                    let argc = items.len();
                    self.stack.extend(items);
                    let callee = self.stack[self.stack.len() - argc - 2].clone();
                    self.invoke(callee, argc, is_new)?;
                    (proto, base, ip) = self.frame_state();
                }
                Op::ArrayPush => {
                    let v = self.pop();
                    if let Some(Value::Arr(a)) = self.stack.last() {
                        a.borrow_mut().push(v);
                    }
                }
                Op::NewObject => self.stack.push(Value::obj()),
                Op::InitProp(i) => {
                    let v = self.pop();
                    if let Some(Value::Obj(o)) = self.stack.last() {
                        obj_set(o, const_name(&proto, i), v);
                    }
                }
                Op::ToKeys => {
                    let v = self.pop();
                    let keys: Vec<Value> = match &v {
                        Value::Obj(o) => o.borrow().iter().map(|(k, _)| Value::str(k)).collect(),
                        Value::Arr(a) => (0..a.borrow().len())
                            .map(|i| Value::str(&i.to_string()))
                            .collect(),
                        Value::Str(s) => (0..s.chars().count())
                            .map(|i| Value::str(&i.to_string()))
                            .collect(),
                        _ => Vec::new(),
                    };
                    self.stack.push(Value::arr(keys));
                }

                Op::Throw => {
                    let v = self.pop();
                    return Err(v);
                }
                Op::PushHandler(t) => {
                    let height = self.stack.len();
                    self.frames
                        .last_mut()
                        .unwrap()
                        .handlers
                        .push((t as usize, height));
                }
                Op::PopHandler => {
                    self.frames.last_mut().unwrap().handlers.pop();
                }
            }
        }
    }

    // ----- properties -------------------------------------------------------

    pub fn get_prop(&mut self, obj: &Value, name: &str) -> Result<Value, Value> {
        Ok(match obj {
            Value::Obj(o) => obj_get(o, name).unwrap_or(Value::Undef),
            Value::Arr(a) => {
                if name == "length" {
                    Value::Num(a.borrow().len() as f64)
                } else {
                    builtins::array::method(name)
                        .map(Value::Native)
                        .unwrap_or(Value::Undef)
                }
            }
            Value::Str(s) => {
                if name == "length" {
                    Value::Num(string::char_len(s) as f64)
                } else {
                    string::method(name)
                        .map(Value::Native)
                        .unwrap_or(Value::Undef)
                }
            }
            Value::Num(_) => string::number_method(name)
                .map(Value::Native)
                .unwrap_or(Value::Undef),
            Value::Func(c) => {
                if name == "name" {
                    Value::str(&c.proto.name)
                } else {
                    Value::Undef
                }
            }
            Value::Native(d) => {
                if name == "name" {
                    Value::str(d.name)
                } else {
                    Value::Undef
                }
            }
            Value::Undef | Value::Null => {
                let msg = format!(
                    "Cannot read properties of {} (reading '{}')",
                    to_str(obj),
                    name
                );
                return Err(self.make_error("TypeError", &msg));
            }
            _ => Value::Undef,
        })
    }

    pub fn set_prop(&mut self, obj: &Value, name: &str, v: Value) -> Result<(), Value> {
        match obj {
            Value::Obj(o) => {
                obj_set(o, name, v);
                Ok(())
            }
            Value::Arr(a) if name == "length" => {
                let n = to_num(&v).max(0.0) as usize;
                a.borrow_mut().resize(n, Value::Undef);
                Ok(())
            }
            _ => {
                let msg = format!("Cannot set property '{}' of {}", name, to_str(obj));
                Err(self.make_error("TypeError", &msg))
            }
        }
    }

    pub fn get_index(&mut self, obj: &Value, key: &Value) -> Result<Value, Value> {
        match (obj, key) {
            (Value::Arr(a), Value::Num(n)) => {
                let a = a.borrow();
                Ok(index_of(*n)
                    .and_then(|i| a.get(i).cloned())
                    .unwrap_or(Value::Undef))
            }
            (Value::Str(s), Value::Num(n)) => Ok(index_of(*n)
                .and_then(|i| string::char_at(s, i))
                .map(|c| Value::str(&c.to_string()))
                .unwrap_or(Value::Undef)),
            (Value::Obj(_) | Value::Arr(_) | Value::Func(_) | Value::Native(_), Value::Str(k)) => {
                let k = k.clone();
                self.get_prop(obj, &k)
            }
            _ => {
                let k = to_str(key);
                self.get_prop(obj, &k)
            }
        }
    }

    pub fn set_index(&mut self, obj: &Value, key: &Value, v: Value) -> Result<(), Value> {
        if let (Value::Arr(a), Value::Num(n)) = (obj, key) {
            let i = match index_of(*n) {
                Some(i) => i,
                None => return Err(self.make_error("RangeError", "Invalid array index")),
            };
            let mut a = a.borrow_mut();
            if i >= a.len() {
                a.resize(i + 1, Value::Undef);
            }
            a[i] = v;
            return Ok(());
        }
        let k = to_str(key);
        self.set_prop(obj, &k, v)
    }

    // ----- errors -----------------------------------------------------------

    /// "file:line" of the instruction that is running in the innermost frame.
    pub fn location(&self) -> String {
        match self.frames.last() {
            Some(f) => {
                let line = f
                    .closure
                    .proto
                    .lines
                    .get(f.ip.saturating_sub(1))
                    .copied()
                    .unwrap_or(0);
                format!("{}:{}", f.closure.proto.file, line)
            }
            None => String::new(),
        }
    }

    /// Builds an error object (`name`, `message`, `stack`) for a built-in error kind.
    pub fn make_error(&mut self, kind: &str, msg: &str) -> Value {
        let loc = self.location();
        let stack = if loc.is_empty() {
            format!("{}: {}", kind, msg)
        } else {
            format!("{}: {}\n    at {}", kind, msg, loc)
        };
        make_obj(vec![
            ("name", Value::str(kind)),
            ("message", Value::str(msg)),
            ("stack", Value::str(&stack)),
        ])
    }

    // ----- timers and the event loop ---------------------------------------

    pub fn add_timer(
        &mut self,
        callback: Value,
        delay: Duration,
        args: Vec<Value>,
        repeat: bool,
    ) -> u64 {
        let id = self.next_timer;
        self.next_timer += 1;
        self.timers.push(Reverse((Instant::now() + delay, id)));
        let interval = if repeat {
            Some(delay.max(Duration::from_millis(1)))
        } else {
            None
        };
        self.timer_table.insert(
            id,
            Timer {
                callback,
                args,
                interval,
            },
        );
        id
    }

    pub fn clear_timer(&mut self, id: u64) {
        self.timer_table.remove(&id);
    }

    /// Runs queued `process.nextTick` callbacks, including ones queued by the callbacks themselves.
    fn run_ticks(&mut self) -> Result<(), Value> {
        while let Some((callback, args)) = self.ticks.pop_front() {
            self.call(&callback, Value::Undef, args)?;
        }
        Ok(())
    }

    fn run_event_loop(&mut self) -> Result<(), Value> {
        self.run_ticks()?;
        while let Some(Reverse((due, id))) = self.timers.pop() {
            let (callback, args, interval) = match self.timer_table.get(&id) {
                Some(t) => (t.callback.clone(), t.args.clone(), t.interval),
                None => continue,
            };
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            match interval {
                Some(every) => self.timers.push(Reverse((Instant::now() + every, id))),
                None => {
                    self.timer_table.remove(&id);
                }
            }
            self.call(&callback, Value::Undef, args)?;
            self.run_ticks()?;
        }
        Ok(())
    }
}
