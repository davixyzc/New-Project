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
use crate::jit::{self, Native};
use crate::value::*;

/// Maximum nesting of JavaScript-level calls before a RangeError is thrown.
const MAX_FRAMES: usize = 10_000;
/// Largest array index that `arr[i] = v` will grow the array to.
pub(crate) const MAX_ARRAY_GROW: usize = 50_000_000;
/// JavaScript calls made from Rust (e.g. a `map` callback) recurse on the Rust stack,
/// so they are capped separately from `MAX_FRAMES`.
const MAX_NESTED_CALLS: usize = 2_000;

#[repr(C)]
pub struct Frame {
    /// The running closure. The callee slot (`base - 2`) holds the `Value::Func` that owns it.
    closure: *const Closure,
    /// Next instruction to run. Only kept up to date when the frame is not running.
    pub ip: usize,
    /// Index of the first argument (slot 0) on the value stack. `this` sits at `base - 1`.
    pub base: usize,
    /// Active `try` handlers: (catch instruction index, stack height to restore). None until the
    /// first `try` of the frame. `None` is a null pointer, which native code can write as zero.
    #[allow(clippy::box_collection)]
    pub handlers: Option<Box<Vec<(usize, usize)>>>,
    /// Set for frames started with `new`: the object in the `this` slot is returned if the
    /// constructor returns nothing.
    pub ctor: bool,
    /// Set for the frame of a `Callback` session. Its return keeps the frame (see `do_return`).
    pub session: bool,
}

/// The frames of the calls in progress. The buffer has a fixed layout, so native code can push a
/// frame by writing it at `len` (see `jit::codegen`).
#[repr(C)]
pub struct FrameStack {
    buf: *mut Frame,
    len: usize,
    cap: usize,
}

impl FrameStack {
    fn new() -> FrameStack {
        FrameStack {
            buf: std::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            cap: 0,
        }
    }

    /// Makes room for one more frame.
    #[cold]
    fn grow(&mut self) {
        let cap = (self.cap * 2).max(16);
        let layout = std::alloc::Layout::array::<Frame>(cap).expect("frame stack fits in memory");
        // SAFETY: `layout` has a non-zero size. A buffer that exists was allocated with the layout
        // of `self.cap` frames, which is what `realloc` is given.
        let buf = unsafe {
            if self.cap == 0 {
                std::alloc::alloc(layout)
            } else {
                let old = std::alloc::Layout::array::<Frame>(self.cap).expect("frame stack fits");
                std::alloc::realloc(self.buf as *mut u8, old, layout.size())
            }
        };
        if buf.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        self.buf = buf as *mut Frame;
        self.cap = cap;
    }

    #[inline]
    pub fn push(&mut self, frame: Frame) {
        if self.len == self.cap {
            self.grow();
        }
        // SAFETY: the slot at `len` is inside the buffer, and it holds no frame.
        unsafe { self.buf.add(self.len).write(frame) }
        self.len += 1;
    }

    #[inline]
    pub fn pop(&mut self) -> Option<Frame> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the frame at `len` was initialized, and nothing reads it after this.
        Some(unsafe { self.buf.add(self.len).read() })
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Pops the last frame, which the caller knows is there, and drops it.
    #[inline]
    pub fn discard_last(&mut self) {
        debug_assert!(self.len > 0);
        self.len -= 1;
        // SAFETY: the frame at `len` was initialized, and nothing reads it after this.
        unsafe { self.buf.add(self.len).drop_in_place() }
    }

    #[inline]
    pub fn last(&self) -> Option<&Frame> {
        if self.len == 0 {
            return None;
        }
        // SAFETY: the frames below `len` are initialized.
        Some(unsafe { &*self.buf.add(self.len - 1) })
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut Frame> {
        if self.len == 0 {
            return None;
        }
        // SAFETY: the frames below `len` are initialized, and `self` is borrowed mutably.
        Some(unsafe { &mut *self.buf.add(self.len - 1) })
    }

    #[inline]
    pub fn get_mut(&mut self, i: usize) -> Option<&mut Frame> {
        if i >= self.len {
            return None;
        }
        // SAFETY: as in `last_mut`.
        Some(unsafe { &mut *self.buf.add(i) })
    }
}

impl Drop for FrameStack {
    fn drop(&mut self) {
        for i in 0..self.len {
            // SAFETY: the frames below `len` are initialized, and each is dropped once.
            unsafe { self.buf.add(i).drop_in_place() }
        }
        if self.cap > 0 {
            let layout = std::alloc::Layout::array::<Frame>(self.cap).expect("frame stack fits");
            // SAFETY: the buffer was allocated with this layout.
            unsafe { std::alloc::dealloc(self.buf as *mut u8, layout) }
        }
    }
}

impl Frame {
    /// The running closure.
    #[inline(always)]
    pub fn closure(&self) -> &Closure {
        // SAFETY: the callee slot below `base` holds the `Value::Func` that owns the closure.
        unsafe { &*self.closure }
    }
}

/// A function that a builtin calls many times in a row, such as the callback of `map`.
///
/// A compiled function with no rest parameter stays on the stack from call to call: each call
/// writes its arguments, resets its locals and runs its code, without pushing or popping a frame.
/// Any other function goes through `Vm::call_moved`. Call `finish` after the last call, which pops
/// the frame. A call that fails has already popped it.
pub enum Callback {
    Session(Session),
    Generic {
        f: Value,
        this: Value,
    },
    /// A function that native code runs on numbers. A call with numbers takes that path, and any
    /// other call goes through `base`.
    Numbers {
        closure: Rc<Closure>,
        nparams: usize,
        base: Box<Callback>,
    },
}

pub struct Session {
    closure: Rc<Closure>,
    /// The compiled code. It belongs to the prototype that `closure` owns.
    code: *const Native,
    this: Value,
    nparams: usize,
    nlocals: usize,
    /// Index of the first argument. The callee slot is `base - 2`, and `this` is at `base - 1`.
    base: usize,
    /// The frame's index in `Vm::frames`.
    depth: usize,
    /// Whether the frame is on the stack.
    alive: bool,
}

impl Callback {
    /// Prepares calls to `f` with `this`. Nothing is pushed until the first call.
    pub fn new(vm: &Vm, f: &Value, this: Value) -> Callback {
        let base = Callback::interpreted(vm, f, this);
        if let Value::Func(c) = f {
            if let Some(nparams) = jit::numbers_arity(c) {
                return Callback::Numbers {
                    closure: c.clone(),
                    nparams,
                    base: Box::new(base),
                };
            }
        }
        base
    }

    /// The callback through the interpreter: a session for a compiled function, otherwise a generic call.
    fn interpreted(vm: &Vm, f: &Value, this: Value) -> Callback {
        if let Value::Func(c) = f {
            let proto: *const Proto = Rc::as_ptr(&c.proto);
            // SAFETY: `c` owns the prototype, and the session keeps `c` alive.
            let (has_rest, nparams, nlocals) = unsafe {
                (
                    (*proto).has_rest,
                    (*proto).nparams as usize,
                    (*proto).nlocals as usize,
                )
            };
            if !has_rest && vm.nested_calls < MAX_NESTED_CALLS && vm.frames.len() < MAX_FRAMES {
                if let Some(code) = unsafe { (*proto).native() } {
                    return Callback::Session(Session {
                        closure: c.clone(),
                        code: code as *const Native,
                        this,
                        nparams,
                        nlocals,
                        base: 0,
                        depth: 0,
                        alive: false,
                    });
                }
            }
        }
        Callback::Generic { f: f.clone(), this }
    }

    /// How many arguments the function can read. A builtin need not build the others.
    pub fn arity(&self) -> usize {
        match self {
            Callback::Session(s) => s.nparams,
            Callback::Generic { .. } => usize::MAX,
            Callback::Numbers { base, .. } => base.arity(),
        }
    }

    /// Calls the function with `args`, as `Vm::call_moved` does.
    pub fn call<const N: usize>(&mut self, vm: &mut Vm, args: [Value; N]) -> Result<Value, Value> {
        match self {
            Callback::Session(s) => s.call(vm, args),
            Callback::Generic { f, this } => vm.call_moved(f, this.clone(), args),
            Callback::Numbers {
                closure,
                nparams,
                base,
            } => {
                let mut nums = [0.0; jit::MAX_NUMBER_ARGS];
                let numeric = *nparams <= N
                    && args
                        .iter()
                        .take(*nparams)
                        .enumerate()
                        .all(|(j, arg)| match arg {
                            Value::Num(x) => {
                                nums[j] = *x;
                                true
                            }
                            _ => false,
                        });
                if numeric {
                    if let Some(v) = jit::call_numbers(closure, &nums[..*nparams]) {
                        return Ok(v);
                    }
                }
                base.call(vm, args)
            }
        }
    }

    /// The function's entry for numbers, for a builtin that makes many calls in a row with numbers.
    pub fn numeric_entry(&self) -> Option<jit::Numeric<'_>> {
        match self {
            Callback::Numbers { closure, .. } => jit::numeric_entry(closure),
            _ => None,
        }
    }

    /// Ends the calls, popping the session's frame if it is still on the stack.
    pub fn finish(mut self, vm: &mut Vm) {
        self.finish_calls(vm);
    }

    fn finish_calls(&mut self, vm: &mut Vm) {
        match self {
            Callback::Session(s) => s.finish(vm),
            Callback::Generic { .. } => {}
            Callback::Numbers { base, .. } => base.finish_calls(vm),
        }
    }
}

impl Session {
    fn call<const N: usize>(&mut self, vm: &mut Vm, args: [Value; N]) -> Result<Value, Value> {
        if !self.alive {
            self.push_frame(vm);
        }
        // SAFETY: `code` is alive as long as `closure` is.
        let code = unsafe { &*self.code };
        let need = self.base + code.frame_len();
        if need > vm.stack.capacity() {
            vm.stack.reserve(need - vm.stack.len());
        }
        // Between calls the stack ends at `base`, so the arguments and the other locals are written
        // from there. Arguments past the parameters are dropped, and missing ones are `undefined`.
        let args = std::mem::ManuallyDrop::new(args);
        // SAFETY: the capacity for the frame was reserved above, and the slots written are the
        // frame's own. Each argument is read once, and `args` is never dropped.
        unsafe {
            let p = vm.stack.as_mut_ptr();
            for i in 0..N {
                let v = std::ptr::read(&args[i]);
                if i < self.nparams {
                    std::ptr::write(p.add(self.base + i), v);
                } else {
                    forget_scalar(v);
                }
            }
            for i in N.min(self.nparams)..self.nlocals {
                std::ptr::write(p.add(self.base + i), Value::Undef);
            }
            vm.stack.set_len(self.base + self.nlocals);
        }
        let status = unsafe {
            let frame = vm.stack.as_mut_ptr().add(self.base);
            code.call(vm as *mut Vm, frame, 0)
        };
        if status == 0 {
            // The return left the result in the callee slot, and the frame is still on the stack.
            Ok(std::mem::replace(
                &mut vm.stack[self.base - 2],
                Value::Undef,
            ))
        } else {
            // The exception left the frame. Its handlers may still catch it, but the frame must
            // return like any other from here on.
            let exc = vm.pending.take().unwrap_or(Value::Undef);
            self.end(vm);
            if let Some(frame) = vm.frames.get_mut(self.depth) {
                frame.session = false;
            }
            vm.resume_after_error(exc, self.depth)
        }
    }

    fn push_frame(&mut self, vm: &mut Vm) {
        self.depth = vm.frames.len();
        vm.nested_calls += 1;
        vm.stack.push(Value::Func(self.closure.clone()));
        vm.stack.push(self.this.clone());
        self.base = vm.stack.len();
        vm.frames.push(Frame {
            closure: Rc::as_ptr(&self.closure),
            ip: 0,
            base: self.base,
            handlers: None,
            ctor: false,
            session: true,
        });
        self.alive = true;
    }

    /// Marks the frame as gone. The caller pops it or has already done so.
    fn end(&mut self, vm: &mut Vm) {
        self.alive = false;
        vm.nested_calls -= 1;
    }

    fn finish(&mut self, vm: &mut Vm) {
        if self.alive {
            self.end(vm);
            vm.frames.pop();
            vm.truncate_stack(self.base - 2);
        }
    }
}

pub struct Timer {
    pub callback: Value,
    pub args: Vec<Value>,
    pub interval: Option<Duration>,
}

pub struct Vm {
    pub stack: Vec<Value>,
    pub frames: FrameStack,
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
    /// Buffer that carries the arguments of a native call, reused from call to call.
    arg_buf: Vec<Value>,
    /// The exception that native code reported, waiting for the dispatcher to take it.
    pub(crate) pending: Option<Value>,
    pub argv: Vec<String>,
    /// Set after a native numeric call ran out of stack. Native calls stay off above this many frames, until the
    /// call that ran out returns (see `native_suspended`).
    native_off: Option<usize>,
}

/// Discards a value. Scalars own nothing, so their drop glue is skipped; others are dropped.
#[inline(always)]
fn forget_scalar(v: Value) {
    if v.owns_ref() {
        drop(v);
    } else {
        std::mem::forget(v);
    }
}

/// A constant of the running function. Operands are emitted in range by the compiler.
#[inline(always)]
fn konst(proto: &Proto, k: u32) -> &Value {
    debug_assert!((k as usize) < proto.consts.len());
    // SAFETY: the compiler only emits indices of constants it added to this prototype.
    unsafe { proto.consts.get_unchecked(k as usize) }
}

/// Overwrites a value. A slot that holds a scalar needs no drop, so it is written directly.
#[inline(always)]
fn put(slot: &mut Value, v: Value) {
    if slot.owns_ref() {
        *slot = v;
    } else {
        // SAFETY: the old value owns nothing, so skipping its drop leaks nothing.
        unsafe { std::ptr::write(slot, v) }
    }
}

/// A bitwise operator on two numbers, computed on 32-bit integers as JavaScript does. A shift counts only the low
/// five bits of its right operand.
fn bitwise(op: Op, a: f64, b: f64) -> f64 {
    let (x, y) = (to_int32(a), to_int32(b));
    let shift = (y as u32) & 31;
    f64::from(match op {
        Op::BitAnd => x & y,
        Op::BitOr => x | y,
        Op::BitXor => x ^ y,
        Op::Shl => x.wrapping_shl(shift),
        Op::Shr => x >> shift,
        _ => return f64::from((x as u32) >> shift),
    })
}

/// The array index a property name spells, such as "2" but not "02" or "-1": digits without a leading zero,
/// below the array size limit.
fn array_key_index(name: &str) -> Option<usize> {
    let digits = name.as_bytes();
    let canonical = !digits.is_empty()
        && digits.len() <= 10
        && digits.iter().all(u8::is_ascii_digit)
        && (digits[0] != b'0' || digits.len() == 1);
    if canonical {
        name.parse::<usize>().ok().filter(|&i| i < MAX_ARRAY_GROW)
    } else {
        None
    }
}

pub(crate) fn index_of(n: f64) -> Option<usize> {
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
            frames: FrameStack::new(),
            globals: HashMap::new(),
            modules: HashMap::new(),
            out: BufWriter::new(std::io::stdout()),
            ticks: VecDeque::new(),
            timers: BinaryHeap::new(),
            timer_table: HashMap::new(),
            next_timer: 1,
            rng: seed | 1,
            nested_calls: 0,
            arg_buf: Vec::new(),
            pending: None,
            argv,
            native_off: None,
        };
        builtins::install(&mut vm);
        vm
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        self.globals.insert(crate::value::intern_name(name), v);
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
            Some(f) => Path::new(&*f.closure().proto.file)
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
        let args = [
            exports,
            builtins::global::require_fn(),
            module.clone(),
            Value::str(&filename),
            Value::str(&dir),
        ];
        self.call(&func, Value::Undef, &args)?;
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
    pub fn call(&mut self, f: &Value, this: Value, args: &[Value]) -> Result<Value, Value> {
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

    /// Like `call`, but the arguments move onto the stack instead of being copied.
    pub fn call_moved<const N: usize>(
        &mut self,
        f: &Value,
        this: Value,
        args: [Value; N],
    ) -> Result<Value, Value> {
        if let Value::Func(c) = f {
            let proto: *const Proto = Rc::as_ptr(&c.proto);
            // SAFETY: `c` owns the prototype, and it is borrowed for the whole call.
            let (has_rest, nlocals) = unsafe { ((*proto).has_rest, (*proto).nlocals as usize) };
            let code = if has_rest {
                None
            } else {
                unsafe { (*proto).native() }
            };
            if let Some(code) = code {
                if self.nested_calls < MAX_NESTED_CALLS && self.frames.len() < MAX_FRAMES {
                    return self.call_compiled(c, code, nlocals, this, args);
                }
            }
        }
        match f {
            Value::Func(c) => {
                if self.nested_calls >= MAX_NESTED_CALLS {
                    return Err(self.make_error("RangeError", "Maximum call stack size exceeded"));
                }
                self.nested_calls += 1;
                let depth = self.frames.len();
                // Arguments past the declared parameters have no name to bind to, so they are
                // dropped here, unless a rest parameter collects them.
                let fixed = c.proto.nparams as usize;
                let keep = if c.proto.has_rest { N } else { N.min(fixed) };
                self.stack.push(Value::Func(c.clone()));
                self.stack.push(this);
                for (i, v) in args.into_iter().enumerate() {
                    if i < keep {
                        self.stack.push(v);
                    }
                }
                let result = match self.enter(c.clone(), keep, false) {
                    Ok(()) => self.run_entered(depth),
                    Err(e) => Err(e),
                };
                self.nested_calls -= 1;
                result
            }
            _ => self.call(f, this, &args),
        }
    }

    /// Calls a closure whose prototype has native code and no rest parameter. Frames are set up
    /// directly, without the general path: arguments past the parameters are dropped, and missing
    /// ones are padded with `undefined`.
    fn call_compiled<const N: usize>(
        &mut self,
        c: &Rc<Closure>,
        code: &Native,
        nlocals: usize,
        this: Value,
        args: [Value; N],
    ) -> Result<Value, Value> {
        self.nested_calls += 1;
        let depth = self.frames.len();
        let base = self.stack.len() + 2;
        let fixed = c.proto.nparams as usize;
        self.stack.push(Value::Func(c.clone()));
        self.stack.push(this);
        for (i, v) in args.into_iter().enumerate() {
            if i < fixed {
                self.stack.push(v);
            }
        }
        while self.stack.len() < base + nlocals {
            self.stack.push(Value::Undef);
        }
        self.frames.push(Frame {
            closure: Rc::as_ptr(c),
            ip: 0,
            base,
            handlers: None,
            ctor: false,
            session: false,
        });
        let result = match self.run_native(code, 0) {
            Ok(()) => Ok(self.pop()),
            Err(e) => self.resume_after_error(e, depth),
        };
        self.nested_calls -= 1;
        result
    }

    /// Finishes a frame that raised `exc` in native code: unwinds to a handler above `depth` and
    /// runs on from there, or returns the exception when no handler catches it.
    fn resume_after_error(&mut self, exc: Value, depth: usize) -> Result<Value, Value> {
        self.unwind(exc, depth)?;
        self.execute(depth)?;
        Ok(self.pop())
    }

    fn call_closure(
        &mut self,
        c: Rc<Closure>,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Value> {
        let depth = self.frames.len();
        self.stack.push(Value::Func(c.clone()));
        self.stack.push(this);
        self.stack.extend_from_slice(args);
        self.enter(c, args.len(), false)?;
        self.run_entered(depth)
    }

    /// Runs the frame that `enter` pushed above `depth` to completion, and returns its result.
    fn run_entered(&mut self, depth: usize) -> Result<Value, Value> {
        let proto: *const Proto = Rc::as_ptr(&self.frames.last().unwrap().closure().proto);
        // SAFETY: the frame holds the closure that owns the prototype.
        if let Some(code) = unsafe { (*proto).native() } {
            match self.run_native(code, 0) {
                Ok(()) => return Ok(self.pop()),
                Err(e) => self.unwind(e, depth)?,
            }
        }
        self.execute(depth)?;
        Ok(self.pop())
    }

    /// Pushes a frame for a closure. Arguments are already on the stack above the callee and `this` slots.
    fn enter(&mut self, closure: Rc<Closure>, argc: usize, ctor: bool) -> Result<(), Value> {
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
            self.truncate_stack(base + fixed);
            while self.stack.len() < base + fixed {
                self.stack.push(Value::Undef);
            }
            self.stack.push(Value::arr(extra));
        } else {
            self.truncate_stack(base + fixed);
            while self.stack.len() < base + fixed {
                self.stack.push(Value::Undef);
            }
        }
        let target = base + proto.nlocals as usize;
        while self.stack.len() < target {
            self.stack.push(Value::Undef);
        }
        self.frames.push(Frame {
            closure: Rc::as_ptr(&closure),
            ip: 0,
            base,
            handlers: None,
            ctor,
            session: false,
        });
        Ok(())
    }

    /// Calls or constructs the value under the arguments. The stack holds [callee, this, args...].
    fn invoke(&mut self, callee: Value, argc: usize, is_new: bool) -> Result<(), Value> {
        match callee {
            Value::Func(c) => {
                if is_new {
                    let n = self.stack.len();
                    self.stack[n - argc - 1] = Value::obj();
                }
                self.enter(c, argc, is_new)
            }
            Value::Native(def) => {
                // The arguments move into a buffer that is reused from call to call.
                let mut args = std::mem::take(&mut self.arg_buf);
                args.clear();
                let start = self.stack.len() - argc;
                args.extend(self.stack.drain(start..));
                let this = self.pop();
                self.stack.pop();
                let this = if is_new { Value::Undef } else { this };
                let result = (def.func)(self, this, &args);
                args.clear();
                self.arg_buf = args;
                self.stack.push(result?);
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
            if let Some((catch_ip, height)) = f.handlers.as_mut().and_then(|h| h.pop()) {
                f.ip = catch_ip;
                self.truncate_stack(height);
                self.stack.push(exc);
                return Ok(());
            }
            let f = self.frames.pop().unwrap();
            self.truncate_stack(f.base - 2);
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
        (f.closure().proto.clone(), f.base, f.ip)
    }

    /// A stack slot of the running frame. `enter` sizes the stack to cover every slot the
    /// compiler can name, so the index is in range.
    #[inline(always)]
    fn local(&self, idx: usize) -> &Value {
        debug_assert!(idx < self.stack.len());
        // SAFETY: see above; checked in debug builds.
        unsafe { self.stack.get_unchecked(idx) }
    }

    #[inline(always)]
    fn local_mut(&mut self, idx: usize) -> &mut Value {
        debug_assert!(idx < self.stack.len());
        // SAFETY: see `local`.
        unsafe { self.stack.get_unchecked_mut(idx) }
    }

    /// `++` / `--` on a plain numeric local, in place.
    #[inline(always)]
    fn step_local(&mut self, idx: usize, delta: f64) {
        match self.local_mut(idx) {
            Value::Num(n) => *n += delta,
            other => {
                let n = to_num(other) + delta;
                put(other, Value::Num(n));
            }
        }
    }

    /// Drops every stack value above `len`. Scalars are skipped without calling drop glue.
    #[inline(always)]
    fn truncate_stack(&mut self, len: usize) {
        if self.stack.len() <= len {
            return;
        }
        for v in &mut self.stack[len..] {
            if v.owns_ref() {
                // SAFETY: each value is dropped once here, and `set_len` below forgets the slots.
                unsafe { std::ptr::drop_in_place(v) }
            }
        }
        // SAFETY: everything above `len` has been dropped or was a scalar.
        unsafe { self.stack.set_len(len) }
    }

    /// Pops the running frame, leaving its result on the caller's stack. A session frame stays
    /// instead, with its `this`: the result goes to the callee slot, where `Session::call` takes it,
    /// and the next call reuses the frame.
    #[inline(always)]
    pub(crate) fn do_return(&mut self) {
        let top = self.frames.last().unwrap();
        let (base, session, ctor) = (top.base, top.session, top.ctor);
        if ctor {
            return self.do_return_ctor(base);
        }
        if !session {
            self.frames.discard_last();
        }
        let len = self.stack.len();
        // SAFETY: the result is the value at `len - 1`. The callee slot (`base - 2`) and the slots
        // above `this` up to the result are dropped where they own a reference, except that a session
        // keeps `this`. The result then moves into the callee slot, and the length ends after it.
        unsafe {
            let p = self.stack.as_mut_ptr();
            if (*p.add(base - 2)).owns_ref() {
                std::ptr::drop_in_place(p.add(base - 2));
            }
            let from = if session { base } else { base - 1 };
            for i in from..(len - 1) {
                let slot = p.add(i);
                if (*slot).owns_ref() {
                    std::ptr::drop_in_place(slot);
                }
            }
            std::ptr::copy_nonoverlapping(p.add(len - 1), p.add(base - 2), 1);
            self.stack.set_len(if session { base } else { base - 1 });
        }
    }

    /// `do_return` for a constructor, which returns the object it was given unless the constructor
    /// returned an object itself.
    #[inline(never)]
    fn do_return_ctor(&mut self, base: usize) {
        let len = self.stack.len();
        self.frames.pop();
        // SAFETY: as in `do_return`, with the result read out first so that it can be replaced.
        unsafe {
            let p = self.stack.as_mut_ptr();
            let v = std::ptr::read(p.add(len - 1));
            let v = if !v.is_object() {
                let obj = (*p.add(base - 1)).clone();
                drop(v);
                obj
            } else {
                v
            };
            for i in (base - 2)..(len - 1) {
                let slot = p.add(i);
                if (*slot).owns_ref() {
                    std::ptr::drop_in_place(slot);
                }
            }
            std::ptr::write(p.add(base - 2), v);
            self.stack.set_len(base - 1);
        }
    }

    /// Runs the frame that is on top, and the frames it calls, until the frame at `base_depth`
    /// returns. Each frame runs as native code when it has been compiled.
    ///
    /// Returns Err with the exception still on the frame stack. The caller unwinds from there.
    fn run(&mut self, base_depth: usize) -> Result<(), Value> {
        while self.frames.len() > base_depth {
            let (proto, ip) = {
                let f = self.frames.last().unwrap();
                (f.closure().proto.clone(), f.ip)
            };
            match proto.native() {
                Some(code) => self.run_native(code, ip)?,
                None => return self.run_interp(base_depth),
            }
        }
        Ok(())
    }

    /// Runs the innermost frame as native code from instruction `ip`. Returns Ok when the frame
    /// returned, which pops it. Returns Err when an exception left it, and it stays on the stack.
    fn run_native(&mut self, code: &Native, ip: usize) -> Result<(), Value> {
        let base = self.frames.last().unwrap().base;
        let need = base + code.frame_len();
        if need > self.stack.capacity() {
            self.stack.reserve(need - self.stack.len());
        }
        // SAFETY: the stack has room for the whole frame (reserved above), and it holds the
        // operands that native code expects at `ip`. Helpers update the frame pointer they return.
        let status = unsafe {
            let frame = self.stack.as_mut_ptr().add(base);
            code.call(self, frame, ip)
        };
        if status == 0 {
            Ok(())
        } else {
            Err(self.pending.take().unwrap_or(Value::Undef))
        }
    }

    /// `arr.push(v)` with one argument, done in place. The argument moves into the array, and the
    /// result replaces the callee. Returns false, and changes nothing, for any other call.
    #[inline(always)]
    pub(crate) fn push_in_place(&mut self, idx: usize, argc: usize) -> bool {
        // The callee, `this` and the argument are the top three slots, so the slice has exactly those.
        let [callee, this, arg] = &mut self.stack[idx..] else {
            return false;
        };
        let is_push = matches!(callee, Value::Native(def)
            if std::ptr::eq(*def, builtins::array::push_def()));
        if !is_push || argc != 1 || !matches!(this, Value::Arr(_)) {
            return false;
        }
        let Value::Arr(items) = std::mem::replace(this, Value::Undef) else {
            unreachable!("`this` was checked to be an array")
        };
        let v = std::mem::replace(arg, Value::Undef);
        let len = {
            let mut vec = items.borrow_mut();
            vec.push(v);
            vec.len()
        };
        drop(items);
        // SAFETY: the callee is a built-in, which owns nothing, so the result overwrites it without a
        // drop. The slots above the result are dead.
        unsafe {
            std::ptr::write(callee, Value::Num(len as f64));
            self.stack.set_len(idx + 1);
        }
        true
    }

    /// Pushes the frame of a plain call with `argc` arguments, for native code that runs the callee
    /// itself (see `rt::prepare_call`). Returns the callee's code, which runs from its first
    /// instruction. Returns `None`, and pushes nothing, unless the callee is compiled code that takes
    /// exactly those arguments.
    #[inline(always)]
    pub(crate) fn push_callee(&mut self, argc: usize) -> Option<*const Native> {
        let idx = self.stack.len() - argc - 2;
        let closure: *const Closure = match &self.stack[idx] {
            Value::Func(c) if c.proto.nparams as usize == argc && !c.proto.has_rest => {
                Rc::as_ptr(c)
            }
            _ => return None,
        };
        if self.frames.len() >= MAX_FRAMES {
            return None;
        }
        // SAFETY: the callee slot holds the closure, which owns its prototype, for as long as the
        // frame runs.
        let proto: *const Proto = unsafe { Rc::as_ptr(&(*closure).proto) };
        let code = unsafe { (*proto).native() }?;
        let base = idx + 2;
        self.frames.push(Frame {
            closure,
            ip: 0,
            base,
            handlers: None,
            ctor: false,
            session: false,
        });
        let target = base + unsafe { (*proto).nlocals as usize };
        while self.stack.len() < target {
            self.stack.push(Value::Undef);
        }
        let need = base + code.frame_len();
        if need > self.stack.capacity() {
            self.stack.reserve(need - self.stack.len());
        }
        Some(code as *const Native)
    }

    /// Whether native numeric calls are suspended. They are while the frames above the one that ran out of native
    /// stack run, so the calls made from there do not each recurse natively to the limit again.
    pub(crate) fn native_suspended(&mut self) -> bool {
        match self.native_off {
            Some(depth) if self.frames.len() > depth => true,
            _ => {
                self.native_off = None;
                false
            }
        }
    }

    /// Suspends native numeric calls above the frame that is running now (see `native_suspended`).
    pub(crate) fn suspend_native(&mut self) {
        self.native_off = Some(self.frames.len());
    }

    pub(crate) fn call_op(&mut self, argc: usize, is_new: bool, spread: bool) -> Result<(), Value> {
        let argc = if spread {
            let list = self.pop();
            let items: Vec<Value> = match &list {
                Value::Arr(a) => a.borrow().clone(),
                _ => Vec::new(),
            };
            let n = items.len();
            self.stack.extend(items);
            n
        } else {
            argc
        };
        let idx = self.stack.len() - argc - 2;
        // Fast path: a plain call of compiled code that takes exactly these arguments.
        if !is_new {
            let fast = match &self.stack[idx] {
                Value::Func(c) if c.proto.nparams as usize == argc && !c.proto.has_rest => {
                    Some(Rc::as_ptr(c))
                }
                _ => None,
            };
            if let Some(closure) = fast {
                // The callee slot keeps the closure alive while the frame runs.
                // SAFETY: `closure` is the closure in the callee slot, which is still there.
                let proto: *const Proto = unsafe { Rc::as_ptr(&(*closure).proto) };
                if let Some(code) = unsafe { (*proto).native() } {
                    if self.frames.len() < MAX_FRAMES {
                        let base = idx + 2;
                        let target = base + unsafe { (*proto).nlocals as usize };
                        self.frames.push(Frame {
                            closure,
                            ip: 0,
                            base,
                            handlers: None,
                            ctor: false,
                            session: false,
                        });
                        while self.stack.len() < target {
                            self.stack.push(Value::Undef);
                        }
                        return self.run_native(code, 0);
                    }
                }
            }
        }
        if !is_new {
            if let Value::Native(def) = &self.stack[idx] {
                let def = *def;
                return self.call_native(def, idx, argc);
            }
        }
        let callee = self.stack[idx].clone();
        let depth = self.frames.len();
        self.invoke(callee, argc, is_new)?;
        if self.frames.len() > depth {
            self.run(depth)
        } else {
            Ok(())
        }
    }

    /// Calls a built-in whose callee is at `idx`, with `this` above it and `argc` arguments above
    /// that. The arguments move into the call instead of being copied.
    fn call_native(
        &mut self,
        def: &'static NativeDef,
        idx: usize,
        argc: usize,
    ) -> Result<(), Value> {
        let this = std::mem::replace(&mut self.stack[idx + 1], Value::Undef);
        let mut args = std::mem::take(&mut self.arg_buf);
        args.clear();
        args.reserve(argc);
        // SAFETY: the `argc` values above `idx + 1` are initialized and move into `args`. Then the
        // stack is cut to `idx` without dropping anything: the callee is a built-in, and `this`
        // was replaced by `Undef`, so no slot in between owns a reference.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.stack.as_ptr().add(idx + 2),
                args.as_mut_ptr(),
                argc,
            );
            args.set_len(argc);
            self.stack.set_len(idx);
        }
        let result = (def.func)(self, this, &args);
        args.clear();
        self.arg_buf = args;
        self.stack.push(result?);
        Ok(())
    }

    /// The interpreter. It runs until the frame at `base_depth` returns, or an exception escapes.
    /// It is the fallback for functions without native code.
    fn run_interp(&mut self, base_depth: usize) -> Result<(), Value> {
        let (mut proto, mut base, mut ip) = self.frame_state();
        loop {
            debug_assert!(ip < proto.code.len());
            // SAFETY: every function ends in `Return`, and each jump target is an instruction
            // of the same function, so `ip` stays in range.
            let op = unsafe { *proto.code.get_unchecked(ip) };
            ip += 1;
            match op {
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
                Op::CallSpread | Op::NewSpread => {
                    self.sync_ip(ip);
                    let list = self.pop();
                    let items: Vec<Value> = match &list {
                        Value::Arr(a) => a.borrow().clone(),
                        _ => Vec::new(),
                    };
                    let argc = items.len();
                    self.stack.extend(items);
                    let callee = self.stack[self.stack.len() - argc - 2].clone();
                    self.invoke(callee, argc, op == Op::NewSpread)?;
                    (proto, base, ip) = self.frame_state();
                }
                Op::Return => {
                    self.do_return();
                    if self.frames.len() == base_depth {
                        return Ok(());
                    }
                    (proto, base, ip) = self.frame_state();
                }
                _ => ip = self.exec_op(op, &proto, base, ip)?,
            }
        }
    }

    /// Runs one instruction that neither calls nor returns. `ip` is the instruction after it.
    /// Returns the instruction to run next, which is `ip` unless a jump was taken.
    /// Native code runs the same code through `jit::rt`, so every instruction has one definition.
    /// Inlined into the interpreter loop, where a call per instruction would cost most of the time.
    #[inline(always)]
    pub(crate) fn exec_op(
        &mut self,
        op: Op,
        proto: &Proto,
        base: usize,
        ip: usize,
    ) -> Result<usize, Value> {
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
                let this = if proto.is_arrow {
                    self.frames.last().unwrap().closure().this.clone()
                } else {
                    self.stack[base - 1].clone()
                };
                self.stack.push(this);
            }
            Op::Pop => {
                if let Some(v) = self.stack.pop() {
                    forget_scalar(v);
                }
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
                let v = self.local(base + i as usize).clone();
                self.stack.push(v);
            }
            Op::SetLocal(i) => {
                let v = self.top().clone();
                put(self.local_mut(base + i as usize), v);
            }
            Op::PopToLocal(i) => {
                let v = self.pop();
                put(self.local_mut(base + i as usize), v);
            }
            Op::IncLocal(i) => self.step_local(base + i as usize, 1.0),
            Op::DecLocal(i) => self.step_local(base + i as usize, -1.0),
            Op::NewCell(i) => {
                self.stack[base + i as usize] = Value::Cell(Rc::new(RefCell::new(Value::Undef)));
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
            Op::AddToLocal(i) => {
                let rhs = self.pop();
                let idx = base + i as usize;
                if joins_too_long(self.local(idx), &rhs) {
                    return Err(self.make_error("RangeError", "Invalid string length"));
                }
                // Moving the value out first lets a string be extended in place.
                let old = std::mem::replace(self.local_mut(idx), Value::Undef);
                let result = match (&old, &rhs) {
                    (Value::Num(x), Value::Num(y)) => {
                        let sum = Value::Num(x + y);
                        forget_scalar(old);
                        forget_scalar(rhs);
                        sum
                    }
                    _ => concat_owned(old, rhs),
                };
                put(self.local_mut(idx), result);
            }
            Op::ConcatLocal(i) => {
                let rhs = self.pop();
                let old = self.pop();
                if joins_too_long(&old, &rhs) {
                    return Err(self.make_error("RangeError", "Invalid string length"));
                }
                let idx = base + i as usize;
                let result = match (&old, &rhs) {
                    (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
                    _ => {
                        // Drop the variable's own reference first, so `old` can be extended in place.
                        let released = match &mut self.stack[idx] {
                            Value::Cell(c) => std::mem::replace(&mut *c.borrow_mut(), Value::Undef),
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
            Op::GetLocalProp(s, n) => {
                let name: &str = &proto.names[n as usize];
                let idx = base + s as usize;
                let direct = match self.local(idx) {
                    Value::Obj(o) => Some(obj_get(o, name).unwrap_or(Value::Undef)),
                    _ => None,
                };
                let v = match direct {
                    Some(v) => v,
                    None => {
                        self.sync_ip(ip);
                        let obj = self.stack[idx].clone();
                        self.get_prop(&obj, name)?
                    }
                };
                self.stack.push(v);
            }
            Op::AddLK(s, k) => {
                let r = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => Value::Num(x + y),
                    (a, b) => add(a, b),
                };
                self.stack.push(r);
            }
            Op::SubLK(s, k) => {
                let r = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => x - y,
                    (a, b) => to_num(a) - to_num(b),
                };
                self.stack.push(Value::Num(r));
            }
            Op::MulLK(s, k) => {
                let r = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => x * y,
                    (a, b) => to_num(a) * to_num(b),
                };
                self.stack.push(Value::Num(r));
            }
            Op::ModLK(s, k) => {
                let r = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => js_mod(*x, *y),
                    (a, b) => js_mod(to_num(a), to_num(b)),
                };
                self.stack.push(Value::Num(r));
            }

            Op::GetUpval(i) => {
                let v = self.frames.last().unwrap().closure().upvals[i as usize]
                    .borrow()
                    .clone();
                self.stack.push(v);
            }
            Op::SetUpval(i) => {
                let v = self.top().clone();
                *self.frames.last().unwrap().closure().upvals[i as usize].borrow_mut() = v;
            }

            Op::GetGlobal(i) => {
                self.sync_ip(ip);
                let name: &str = &proto.names[i as usize];
                let found = self.globals.get(name).cloned();
                match found {
                    Some(v) => self.stack.push(v),
                    None => {
                        return Err(
                            self.make_error("ReferenceError", &format!("{} is not defined", name))
                        )
                    }
                }
            }
            Op::GetGlobalOrUndef(i) => {
                let v = self
                    .globals
                    .get(&*proto.names[i as usize])
                    .cloned()
                    .unwrap_or(Value::Undef);
                self.stack.push(v);
            }
            Op::SetGlobal(i) => {
                let name = proto.names[i as usize].clone();
                let v = self.top().clone();
                self.globals.insert(name, v);
            }
            Op::ConstAssign(_) => {
                self.sync_ip(ip);
                return Err(self.make_error("TypeError", "Assignment to constant variable."));
            }

            Op::GetProp(i) => {
                self.sync_ip(ip);
                let obj = self.pop();
                let v = self.get_prop(&obj, &proto.names[i as usize])?;
                self.stack.push(v);
            }
            Op::SetProp(i) => {
                self.sync_ip(ip);
                let v = self.pop();
                let obj = self.pop();
                self.set_prop(&obj, &proto.names[i as usize], v.clone())?;
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
                    if joins_too_long(&a, &b) {
                        return Err(self.make_error("RangeError", "Invalid string length"));
                    }
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
            Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr | Op::UShr => {
                let (a, b) = self.pop_pair();
                self.stack
                    .push(Value::Num(bitwise(op, to_num(&a), to_num(&b))));
            }
            Op::BitNot => {
                let a = self.pop();
                self.stack
                    .push(Value::Num(f64::from(!to_int32(to_num(&a)))));
            }
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
                    let r = matches!(compare(&a, &b), Some(Ordering::Greater | Ordering::Equal));
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

            Op::Jump(t) => return Ok(t as usize),
            Op::JumpIfFalse(t) => {
                if !truthy(&self.pop()) {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfTrue(t) => {
                if truthy(&self.pop()) {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotLt(t) => {
                let less = match self.num_pair() {
                    Some((x, y)) => {
                        self.stack.truncate(self.stack.len() - 2);
                        x < y
                    }
                    // `pop_pair` already removed both operands, so nothing is left to truncate.
                    None => {
                        let (a, b) = self.pop_pair();
                        compare(&a, &b) == Some(Ordering::Less)
                    }
                };
                if !less {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotLe(t) => {
                let (a, b) = self.pop_pair();
                if !matches!(compare(&a, &b), Some(Ordering::Less | Ordering::Equal)) {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotGt(t) => {
                let (a, b) = self.pop_pair();
                if compare(&a, &b) != Some(Ordering::Greater) {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotGe(t) => {
                let (a, b) = self.pop_pair();
                if !matches!(compare(&a, &b), Some(Ordering::Greater | Ordering::Equal)) {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfLtLK(s, k, t) => {
                let less = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => *x < *y,
                    (a, b) => compare(a, b) == Some(Ordering::Less),
                };
                if less {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfLtLL(a, b, t) => {
                let less = match (self.local(base + a as usize), self.local(base + b as usize)) {
                    (Value::Num(x), Value::Num(y)) => *x < *y,
                    (x, y) => compare(x, y) == Some(Ordering::Less),
                };
                if less {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotLtLK(s, k, t) => {
                let less = match (self.local(base + s as usize), konst(proto, k)) {
                    (Value::Num(x), Value::Num(y)) => *x < *y,
                    (a, b) => compare(a, b) == Some(Ordering::Less),
                };
                if !less {
                    return Ok(t as usize);
                }
            }
            Op::JumpIfNotLtLL(a, b, t) => {
                let less = match (self.local(base + a as usize), self.local(base + b as usize)) {
                    (Value::Num(x), Value::Num(y)) => *x < *y,
                    (x, y) => compare(x, y) == Some(Ordering::Less),
                };
                if !less {
                    return Ok(t as usize);
                }
            }
            Op::AndJump(t) => {
                if truthy(self.top()) {
                    self.pop();
                } else {
                    return Ok(t as usize);
                }
            }
            Op::OrJump(t) => {
                if truthy(self.top()) {
                    return Ok(t as usize);
                } else {
                    self.pop();
                }
            }
            Op::NullishJump(t) => {
                if self.top().is_nullish() {
                    self.pop();
                } else {
                    return Ok(t as usize);
                }
            }

            Op::Closure(i) => {
                let child = proto.protos[i as usize].clone();
                let frame = self.frames.last().unwrap();
                let mut upvals = Vec::with_capacity(child.upvals.len());
                for d in &child.upvals {
                    if d.from_local {
                        let cell = match &self.stack[base + d.index as usize] {
                            Value::Cell(c) => c.clone(),
                            other => Rc::new(RefCell::new(other.clone())),
                        };
                        upvals.push(cell);
                    } else {
                        upvals.push(frame.closure().upvals[d.index as usize].clone());
                    }
                }
                let this = if child.is_arrow {
                    if proto.is_arrow {
                        frame.closure().this.clone()
                    } else {
                        self.stack[base - 1].clone()
                    }
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
            Op::ArrayPush => {
                let v = self.pop();
                if let Some(Value::Arr(a)) = self.stack.last() {
                    a.borrow_mut().push(v);
                }
            }
            Op::NewObject(n) => self.stack.push(Value::obj_with(n as usize)),
            Op::InitProp(i) => {
                let v = self.pop();
                if let Some(Value::Obj(o)) = self.stack.last() {
                    obj_put(o, &proto.names[i as usize], v);
                }
            }
            Op::ToKeys => {
                let v = self.pop();
                let keys: Vec<Value> = match &v {
                    Value::Obj(o) => {
                        let props = o.borrow();
                        let order = key_order(&props);
                        order.into_iter().map(|i| Value::str(&props[i].0)).collect()
                    }
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
                    .get_or_insert_with(Default::default)
                    .push((t as usize, height));
            }
            Op::PopHandler => {
                if let Some(h) = self.frames.last_mut().unwrap().handlers.as_mut() {
                    h.pop();
                }
            }
            Op::Call(_) | Op::New(_) | Op::CallSpread | Op::NewSpread | Op::Return => {
                unreachable!("calls and returns are run by the dispatcher, not by exec_op")
            }
        }
        Ok(ip)
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
            Value::Func(c) => match name {
                "name" => Value::str(&c.proto.name),
                // The parameters before a rest parameter, which is what `length` counts.
                "length" => {
                    Value::Num(c.proto.nparams.saturating_sub(u32::from(c.proto.has_rest)) as f64)
                }
                _ => Value::Undef,
            },
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
        if let (Value::Arr(a), Value::Str(k)) = (obj, key) {
            if let Some(i) = array_key_index(k) {
                return Ok(a.borrow().get(i).cloned().unwrap_or(Value::Undef));
            }
        }
        match (obj, key) {
            (Value::Arr(a), Value::Num(n)) => {
                let a = a.borrow();
                Ok(index_of(*n)
                    .and_then(|i| a.get(i).cloned())
                    .unwrap_or(Value::Undef))
            }
            (Value::Str(s), Value::Num(n)) => Ok(index_of(*n)
                .and_then(|i| string::char_at(s, i))
                .map(crate::value::char_value)
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
        if let (Value::Arr(_), Value::Str(k)) = (obj, key) {
            if let Some(i) = array_key_index(k) {
                return self.set_index(obj, &Value::Num(i as f64), v);
            }
        }
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
                    .closure()
                    .proto
                    .lines
                    .get(f.ip.saturating_sub(1))
                    .copied()
                    .unwrap_or(0);
                format!("{}:{}", f.closure().proto.file, line)
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
            self.call(&callback, Value::Undef, &args)?;
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
            self.call(&callback, Value::Undef, &args)?;
            self.run_ticks()?;
        }
        Ok(())
    }
}
