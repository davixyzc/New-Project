//! Native code for functions. The first time a function runs, its bytecode is translated to
//! x86-64 machine code that works directly on the interpreter's value stack (see `codegen`).
//! Instructions that are not plain numbers, locals or jumps call back into the VM (see `rt`),
//! so every instruction has one definition. The interpreter stays as the fallback. It runs
//! on platforms without native code, and everywhere when `FLY_JIT=0` is set.
//!
//! Code that works on numbers only can also run without the VM: a function whose arguments are
//! numbers is called as plain machine code (see `region` and `call_numbers`).

mod codegen;
mod mem;
mod region;
mod rt;
mod types;
mod x64;

use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::OnceLock;

use crate::bytecode::Proto;
use crate::value::{Closure, Value};
use crate::vm::Vm;
use mem::ExecMem;

/// Native code needs x86-64 and an `mmap` that can make pages executable.
const SUPPORTED: bool = cfg!(all(
    target_arch = "x86_64",
    any(target_os = "linux", target_os = "macos")
));

/// The most parameters a function that runs on numbers takes.
pub(crate) const MAX_NUMBER_ARGS: usize = region::MAX_ARGS;

/// What the wrapper of a function region returns: its result in xmm0, and in rax 0, or 1 when the native
/// stack ran out. The C calling convention returns a struct of a double and an integer in those registers.
#[repr(C)]
struct Ret {
    value: f64,
    status: u64,
}

/// A wrapper of a function region, which takes the parameters as doubles.
type Wrapper = unsafe extern "C" fn(*const f64) -> Ret;

/// A batch entry of a function region, which runs the function over the elements of a batch (see `Batch`).
/// It reads the elements from its first argument, and writes results to its second.
type BatchFn = unsafe extern "C" fn(*const Value, *mut Value, *mut Batch) -> u64;

/// The progress of a batch of numeric calls, which native code updates as it goes (see
/// `region::emit_batch`). The batch runs the elements from `start` to `end`. `done` counts the elements
/// that ran, `kept` the elements that a filter kept, and `acc` is the accumulator of a reduce. Native code
/// finds the fields by `offset_of!`.
#[repr(C)]
struct Batch {
    start: u64,
    end: u64,
    done: u64,
    kept: u64,
    acc: f64,
}

impl Batch {
    fn new(items: &[Value], start: usize, acc: f64) -> Batch {
        Batch {
            start: start as u64,
            end: items.len() as u64,
            done: 0,
            kept: 0,
            acc,
        }
    }
}

/// What a function that runs on numbers returns: a number, or a boolean. Both are plain data, so a
/// caller that loops over them drops nothing.
#[derive(Clone, Copy)]
pub(crate) enum Number {
    Num(f64),
    Bool(bool),
}

impl Number {
    pub(crate) fn value(self) -> Value {
        match self {
            Number::Num(n) => Value::Num(n),
            Number::Bool(b) => Value::Bool(b),
        }
    }
}

/// Where an `Rc`'s value sits, counted from the pointer the `Rc` holds: after the strong and weak counts. Native code
/// adds it to that pointer to reach the closure or prototype it holds (see `codegen::closure_call`).
pub(crate) const RC_VALUE: i32 = 16;

/// Whether `Rc` lays out its allocation the way native code reads it: a bare pointer, the strong count first, and the
/// value at `RC_VALUE`. Native calls are left to the helpers when this does not hold.
pub(crate) fn rc_layout_ok() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if std::mem::size_of::<std::rc::Rc<u64>>() != std::mem::size_of::<usize>() {
            return false;
        }
        let rc = std::rc::Rc::new(7u64);
        // SAFETY: an `Rc` is a bare pointer of the size checked above, so its bits are that pointer.
        let bits: usize = unsafe { std::mem::transmute_copy(&rc) };
        let value = std::rc::Rc::as_ptr(&rc) as usize;
        // SAFETY: `bits` points at the allocation, which starts with the strong count and lives as long as `rc`.
        let strong = unsafe { *(bits as *const usize) };
        value.wrapping_sub(bits) == RC_VALUE as usize && strong == 1
    })
}

/// Whether functions are compiled. `FLY_JIT=0` turns it off.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| SUPPORTED && std::env::var_os("FLY_JIT").is_none_or(|v| v != "0"))
}

/// Where a function's native code is kept. It is filled on first use, and holds `None` if
/// compilation was not possible.
pub type Slot = OnceCell<Option<Box<Native>>>;

/// A compiled function.
pub struct Native {
    mem: ExecMem,
    /// The offset of each instruction's code, from the start of `mem`. The last one is the end.
    offsets: Vec<usize>,
    /// Stack slots a frame needs: its locals and its deepest operand stack.
    frame_len: usize,
    /// The function's entry for numbers, when it has one.
    spec: Option<Spec>,
    /// The function's entry for a method, when it reads its receiver.
    method: Option<Method>,
    /// The function's entry for a closure that reads a captured number (see `ClosureEntry`).
    closure: Option<ClosureEntry>,
}

/// The wrapper of a method, which takes the parameters as doubles and the receiver. It returns 0 when the method ran, and 1
/// when it declined before it changed anything (see `region::method_admitted`).
type MethodWrapper = unsafe extern "C" fn(*const f64, *const Value) -> u64;

/// The entry of a method: its wrapper, and how many parameters the wrapper takes.
struct Method {
    wrapper: MethodWrapper,
    nparams: u32,
}

/// The wrapper of a closure that reads a captured number, which takes the parameters as doubles and the address of the
/// callee's value, and returns its result (see `Ret`).
type ClosureWrapper = unsafe extern "C" fn(*const f64, *const Value) -> Ret;

/// The entry of a closure: its wrapper, how many parameters the wrapper takes, and whether it returns a boolean.
struct ClosureEntry {
    wrapper: ClosureWrapper,
    nparams: u32,
    returns_bool: bool,
}

/// The entry of a function region: a wrapper that takes the parameters as doubles.
struct Spec {
    /// The wrapper, which runs the function's code.
    wrapper: Wrapper,
    /// The batch entries of `array::map`, `filter` and `reduce`, where the function has them.
    map: Option<BatchFn>,
    filter: Option<BatchFn>,
    reduce: Option<BatchFn>,
    /// How many parameters the wrapper takes.
    nparams: u32,
    /// The upvalue that the function calls itself through, if it does (see `region::Function`).
    self_upval: Option<u32>,
    /// Whether the function returns a boolean, which the wrapper gives as 0.0 or 1.0.
    returns_bool: bool,
}

impl Spec {
    /// Runs the function on `args`, which are its parameters. `None` when the native stack ran out.
    #[inline]
    fn call(&self, args: &[f64]) -> Option<Number> {
        debug_assert_eq!(args.len(), self.nparams as usize);
        // SAFETY: the wrapper reads `nparams` doubles from `args`, and the caller passes exactly that many.
        let ret = unsafe { (self.wrapper)(args.as_ptr()) };
        (ret.status == 0).then(|| {
            if self.returns_bool {
                Number::Bool(ret.value != 0.0)
            } else {
                Number::Num(ret.value)
            }
        })
    }
}

impl Native {
    /// The stack slots a frame of this function needs.
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// The numeric entry as native code calls it: its wrapper's address, its number of parameters and whether it
    /// returns a boolean. `None` when the function has no such entry, or calls itself through an upvalue, whose check
    /// the helpers make (see `call_numbers`).
    pub(crate) fn direct_numbers(&self) -> Option<(usize, u32, bool)> {
        let spec = self.spec.as_ref()?;
        spec.self_upval.is_none().then_some((
            spec.wrapper as usize,
            spec.nparams,
            spec.returns_bool,
        ))
    }

    /// The entry of a closure as native code calls it: its wrapper's address, its number of parameters, and whether it
    /// returns a boolean. `None` when the function has no such entry.
    pub(crate) fn direct_closure(&self) -> Option<(usize, u32, bool)> {
        let closure = self.closure.as_ref()?;
        Some((
            closure.wrapper as usize,
            closure.nparams,
            closure.returns_bool,
        ))
    }

    /// The entry of a method as native code calls it: its wrapper's address, and its number of parameters. `None` when the
    /// function has no such entry.
    pub(crate) fn direct_method(&self) -> Option<(usize, u32)> {
        let method = self.method.as_ref()?;
        Some((method.wrapper as usize, method.nparams))
    }

    /// The address of the function's code. Native code calls it with `rdx` set to 0, which starts
    /// at the first instruction.
    pub fn entry(&self) -> *const u8 {
        self.mem.ptr()
    }

    /// Runs the function on the numbers in `args`, its parameters. Returns its result, or `None` when
    /// the native stack ran out, and the call must run in the interpreter instead.
    fn call_numbers(&self, args: &[f64]) -> Option<Value> {
        self.spec.as_ref()?.call(args).map(Number::value)
    }

    /// Runs the function from instruction `ip`. Returns 0 when the frame returned, and 1 when
    /// an exception is waiting in the VM's `pending` slot.
    ///
    /// # Safety
    /// `vm` must point to the running VM. `frame` must point at the frame's first slot, with
    /// room for `frame_len` slots. The stack must hold the operands the code expects at `ip`.
    pub unsafe fn call(&self, vm: *mut Vm, frame: *mut Value, ip: usize) -> u64 {
        type Entry = unsafe extern "C" fn(*mut Vm, *mut Value, *const u8) -> u64;
        let start = self.mem.ptr();
        let entry: Entry = std::mem::transmute(start);
        entry(vm, frame, start.wrapping_add(self.offsets[ip]))
    }
}

/// How many parameters a call of `closure` passes when it runs on numbers, or `None` when the function
/// has no entry for numbers.
pub(crate) fn numbers_arity(closure: &Closure) -> Option<usize> {
    let spec = closure.proto.native()?.spec.as_ref()?;
    Some(spec.nparams as usize)
}

/// A function that runs on numbers, held by a builtin that calls it many times in a row. It needs no
/// check per call, which is why a function that calls itself through an upvalue has none.
pub(crate) struct Numeric<'a> {
    spec: &'a Spec,
}

impl Numeric<'_> {
    /// Runs the function on `args`, its parameters as numbers, and returns its result. `None` when `args` are not
    /// its parameters, or the native stack ran out, and the call must run in the interpreter.
    pub(crate) fn call(&self, args: &[f64]) -> Option<Value> {
        if args.len() != self.spec.nparams as usize {
            return None;
        }
        self.spec.call(args).map(Number::value)
    }

    /// `call` for a comparator: two numbers in, the result as a number out. The sort loop gets plain numbers, so
    /// it builds no value per comparison.
    pub(crate) fn compare(&self, a: f64, b: f64) -> Option<f64> {
        if self.spec.nparams != 2 {
            return None;
        }
        Some(match self.spec.call(&[a, b])? {
            Number::Num(n) => n,
            Number::Bool(flag) => f64::from(u8::from(flag)),
        })
    }

    /// Maps the elements of `items` from `start` on through the function, appending the results to `out`.
    /// Returns how many elements ran. A batch stops at the first element that is not a number, and the
    /// interpreter runs the rest, starting with that element.
    pub(crate) fn map(&self, items: &[Value], start: usize, out: &mut Vec<Value>) -> usize {
        let Some(batch) = self.spec.map else {
            return 0;
        };
        out.reserve(items.len() - start);
        let base = out.len();
        let mut progress = Batch::new(items, start, 0.0);
        // SAFETY: the batch reads the elements from `start` on, which exist, and writes one result per
        // element into the room that `reserve` made.
        unsafe {
            batch(
                items.as_ptr().add(start),
                out.as_mut_ptr().add(base),
                &mut progress,
            );
            out.set_len(base + progress.done as usize);
        }
        progress.done as usize
    }

    /// Keeps the elements from `start` on that the function finds true, appending them to `out`. Returns
    /// how many elements ran (see `map`).
    pub(crate) fn filter(&self, items: &[Value], start: usize, out: &mut Vec<Value>) -> usize {
        let Some(batch) = self.spec.filter else {
            return 0;
        };
        out.reserve(items.len() - start);
        let base = out.len();
        let mut progress = Batch::new(items, start, 0.0);
        // SAFETY: as for `map`, and a filter writes at most one element per element it runs.
        unsafe {
            batch(
                items.as_ptr().add(start),
                out.as_mut_ptr().add(base),
                &mut progress,
            );
            out.set_len(base + progress.kept as usize);
        }
        progress.done as usize
    }

    /// Folds the elements from `start` on into `acc` through the function. Returns the new accumulator, and
    /// how many elements ran (see `map`).
    pub(crate) fn reduce(&self, items: &[Value], start: usize, acc: f64) -> (f64, usize) {
        let Some(batch) = self.spec.reduce else {
            return (acc, 0);
        };
        let mut progress = Batch::new(items, start, acc);
        // SAFETY: as for `map`. A reduce writes no results, so it is given no output.
        unsafe {
            batch(
                items.as_ptr().add(start),
                std::ptr::null_mut(),
                &mut progress,
            )
        };
        (progress.acc, progress.done as usize)
    }
}

/// The entry of `closure` for numbers, when it has one that needs no check per call.
pub(crate) fn numeric_entry(closure: &Closure) -> Option<Numeric<'_>> {
    let spec = closure.proto.native()?.spec.as_ref()?;
    spec.self_upval.is_none().then_some(Numeric { spec })
}

/// Runs `closure` on `args`, its parameters as numbers, when it has an entry for numbers and calls
/// through its self-reference, if it has one, reach the function itself. `None` when the interpreter
/// must make the call: the native stack ran out, or the function is not the one its code expects.
pub(crate) fn call_numbers(closure: &Rc<Closure>, args: &[f64]) -> Option<Value> {
    let native = closure.proto.native()?;
    let spec = native.spec.as_ref()?;
    if args.len() != spec.nparams as usize {
        return None;
    }
    if let Some(upval) = spec.self_upval {
        let is_self = match closure.upvals.get(upval as usize) {
            Some(Value::Cell(cell)) => {
                matches!(&*cell.borrow(), Value::Func(f) if Rc::ptr_eq(f, closure))
            }
            Some(Value::Func(f)) => Rc::ptr_eq(f, closure),
            _ => false,
        };
        if !is_self {
            return None;
        }
    }
    native.call_numbers(args)
}

/// Compiles a function to native code. `None` when native code is disabled or unavailable.
pub fn compile(proto: &Proto) -> Option<Box<Native>> {
    if !enabled() {
        return None;
    }
    let compiled = codegen::compile(proto)?;
    let mem = ExecMem::new(&compiled.code)?;
    // SAFETY (for the transmutes below): each offset is an entry in `mem`, which is not moved by `Native` and lives as
    // long as the entries do. Each entry is called with the signature of its type.
    let at = |offset: usize| unsafe { mem.ptr().add(offset) };
    let (mut spec, mut method, mut closure) = (None, None, None);
    if let Some((entries, function)) = compiled.function {
        if function.receiver {
            method = Some(Method {
                wrapper: unsafe {
                    std::mem::transmute::<*const u8, MethodWrapper>(at(entries.wrapper))
                },
                nparams: function.nparams,
            });
        } else if function.upvals {
            closure = Some(ClosureEntry {
                wrapper: unsafe {
                    std::mem::transmute::<*const u8, ClosureWrapper>(at(entries.wrapper))
                },
                nparams: function.nparams,
                returns_bool: function.returns_bool,
            });
        } else {
            spec = Some(Spec {
                wrapper: unsafe { std::mem::transmute::<*const u8, Wrapper>(at(entries.wrapper)) },
                map: entries
                    .map
                    .map(|o| unsafe { std::mem::transmute::<*const u8, BatchFn>(at(o)) }),
                filter: entries
                    .filter
                    .map(|o| unsafe { std::mem::transmute::<*const u8, BatchFn>(at(o)) }),
                reduce: entries
                    .reduce
                    .map(|o| unsafe { std::mem::transmute::<*const u8, BatchFn>(at(o)) }),
                nparams: function.nparams,
                self_upval: function.self_upval,
                returns_bool: function.returns_bool,
            });
        }
    }
    Some(Box::new(Native {
        mem,
        offsets: compiled.offsets,
        frame_len: compiled.frame_len,
        spec,
        method,
        closure,
    }))
}
