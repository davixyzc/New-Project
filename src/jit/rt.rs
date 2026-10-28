//! The functions native code calls. Each runs one instruction (or one call or return) through
//! the interpreter's own code in `Vm`, so an instruction behaves the same whichever path runs it.

use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::ptr;
use std::rc::Rc;

use crate::builtins;
use crate::bytecode::Proto;
use crate::value::{
    concat_owned, find_setter, joins_too_long, lookup_hinted, obj_init_hinted, obj_put_hinted,
    ArrRef, JsStr, Lookup, PropHint, Value,
};
use crate::vm::Vm;

use super::{call_numbers, numbers_arity, MAX_NUMBER_ARGS};

/// Status codes, returned in `rax`.
pub const STATUS_DONE: u64 = 0;
pub const STATUS_THREW: u64 = 1;
pub const STATUS_JUMP: u64 = 2;

/// Flags for `call`.
pub const CALL_NEW: u32 = 1;
pub const CALL_SPREAD: u32 = 2;

/// What a helper returns: a status in `rax` and the frame's address in `rdx`. The stack may have
/// grown while the helper ran, so native code takes the frame address from `base` again.
#[repr(C)]
pub struct HelperRet {
    pub status: u64,
    pub base: *mut Value,
}

/// Brings the VM's view of the running frame up to date before a helper runs. It saves the
/// instruction pointer and sets the stack length to the `depth` operands in use. Returns the
/// frame's prototype and base.
///
/// # Safety
/// `depth` must fit in the capacity that `run_native` reserved for the frame.
unsafe fn enter(vm: &mut Vm, next_ip: usize, depth: usize) -> (*const Proto, usize) {
    let frame = vm.frames.last_mut().unwrap();
    frame.ip = next_ip;
    let proto = Rc::as_ptr(&frame.closure().proto);
    let base = frame.base;
    let len = base + (*proto).nlocals as usize + depth;
    // SAFETY: native code only leaves initialized values below `len`, and the capacity is reserved.
    vm.stack.set_len(len);
    (proto, base)
}

/// Turns an instruction's result into the helper's return value. `result` is the instruction
/// that runs next, so anything other than `next` is a jump the native code must take. An
/// exception is kept in `vm.pending` for the dispatcher to take.
///
/// # Safety
/// Same as `enter`: the frame must still be on the stack.
unsafe fn finish(vm: &mut Vm, result: Result<usize, Value>, next: usize) -> HelperRet {
    let status = match result {
        Ok(target) if target == next => STATUS_DONE,
        Ok(_) => STATUS_JUMP,
        Err(e) => {
            vm.pending = Some(e);
            STATUS_THREW
        }
    };
    let base = vm.frames.last().map_or(0, |f| f.base);
    HelperRet {
        status,
        base: vm.stack.as_mut_ptr().add(base),
    }
}

/// `a + b` where one of them is a string and both are primitives: `a` is replaced by the concatenation, and `b` is
/// dead afterwards. Anything else writes nothing and returns false. An object converts through user code, so the
/// instruction's own path runs it.
///
/// # Safety
/// `a` and `b` must point at valid values that the instruction pops.
pub unsafe extern "C" fn concat_top(a: *mut Value, b: *const Value) -> bool {
    let primitive = |v: &Value| {
        matches!(
            v,
            Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Undef | Value::Null
        )
    };
    let (x, y) = (&*a, &*b);
    let textual = matches!(x, Value::Str(_)) || matches!(y, Value::Str(_));
    if !textual || !primitive(x) || !primitive(y) || joins_too_long(x, y) {
        return false;
    }
    let left = ptr::read(a);
    let right = ptr::read(b);
    ptr::write(a, concat_owned(left, right));
    true
}

/// `x % y` for numbers: the remainder with the sign of `x`, which is JavaScript's `%`. Native code
/// calls it when its inline path does not cover the operands.
pub extern "C" fn js_mod(x: f64, y: f64) -> f64 {
    crate::value::js_mod(x, y)
}

/// Runs the instruction at `ip`, which native code does not handle itself.
///
/// # Safety
/// `vm` must be the running VM, and the operands of `ip` must be on the stack (see `enter`).
pub unsafe extern "C" fn op(vm: *mut Vm, ip: usize, depth: usize) -> HelperRet {
    let vm = &mut *vm;
    let next = ip + 1;
    let (proto, base) = enter(vm, next, depth);
    let proto = &*proto;
    let result = vm.exec_op(proto.code[ip], proto, base, next);
    finish(vm, result, next)
}

/// Drops the value at `p`, which native code releases when its reference is the last one to the object (see
/// `codegen::native_return`). Dropping it frees the object.
///
/// # Safety
/// `p` must point at a live value, which the caller gives up and never reads again.
pub unsafe extern "C" fn drop_slot(p: *mut Value) {
    ptr::drop_in_place(p);
}

/// What `call_plain` returns. `code` is the address of the callee's code, which native code runs
/// next. Or it is the status of a call that has already run to its end (`STATUS_DONE` or
/// `STATUS_THREW`), as the other helpers return it. `base` is the caller's frame, which may have
/// moved.
#[repr(C)]
pub struct PlainCall {
    pub code: u64,
    pub base: *mut Value,
}

/// Runs a plain call (no `new`, no spread) for native code. When the callee is compiled code that
/// takes exactly `argc` arguments, its frame is pushed and its code address is returned, and native
/// code runs it. Any other call runs to its end here.
///
/// # Safety
/// Same as `call`.
pub unsafe extern "C" fn call_plain(
    vm: *mut Vm,
    ip: usize,
    depth: usize,
    argc: usize,
) -> PlainCall {
    let vm = &mut *vm;
    let (_, base) = enter(vm, ip + 1, depth);
    // The callee is one of a push, a number function or a compiled closure, and each test is a
    // single comparison of its tag, so the push, the most common call in array code, goes first.
    let idx = vm.stack.len() - argc - 2;
    let code = if vm.push_in_place(idx, argc) || call_plain_numbers(vm, argc) {
        STATUS_DONE
    } else {
        match vm.push_callee(argc) {
            Some(code) => (*code).entry() as u64,
            None => {
                let result = vm.call_op(argc, false, false).map(|()| ip + 1);
                finish(vm, result, ip + 1).status
            }
        }
    };
    PlainCall {
        code,
        base: vm.stack.as_mut_ptr().add(base),
    }
}

/// Runs the call of a function that native code compiled for numbers (see `call_numbers`), when the
/// arguments it uses are numbers. Returns false, and leaves the stack as it is, when the call must run
/// in the interpreter.
fn call_plain_numbers(vm: &mut Vm, argc: usize) -> bool {
    if vm.native_suspended() {
        return false;
    }
    let idx = vm.stack.len() - argc - 2;
    // The callee is borrowed rather than cloned: most calls are not numeric, and they pay no reference count.
    let (closure, n) = match &vm.stack[idx] {
        Value::Func(c) => match numbers_arity(c) {
            Some(n) => (c, n),
            None => return false,
        },
        _ => return false,
    };
    if argc < n || n > MAX_NUMBER_ARGS {
        return false;
    }
    let mut args = [0.0; MAX_NUMBER_ARGS];
    for (j, arg) in args.iter_mut().take(n).enumerate() {
        match &vm.stack[idx + 2 + j] {
            Value::Num(x) => *arg = *x,
            _ => return false,
        }
    }
    let Some(out) = call_numbers(closure, &args[..n]) else {
        // The native stack ran out, so the interpreter makes this call. The calls it makes from there run in the
        // interpreter too, until this call returns, instead of recursing natively to the limit each time.
        vm.suspend_native();
        return false;
    };
    // The result replaces the callee, and the `this` and the arguments above it are dropped.
    vm.stack.truncate(idx);
    vm.stack.push(out);
    true
}

/// Runs the call or construct at `ip` with `argc` arguments. The callee has finished on return.
///
/// # Safety
/// Same as `op`.
pub unsafe extern "C" fn call(
    vm: *mut Vm,
    ip: usize,
    depth: usize,
    argc: usize,
    flags: usize,
) -> HelperRet {
    let vm = &mut *vm;
    enter(vm, ip + 1, depth);
    let is_new = flags & (CALL_NEW as usize) != 0;
    let spread = flags & (CALL_SPREAD as usize) != 0;
    let result = vm.call_op(argc, is_new, spread).map(|()| ip + 1);
    finish(vm, result, ip + 1)
}

// The value helpers below run without the VM's bookkeeping, because they neither fail nor
// change the stack's length. They only move or copy values between slots of the frame.

/// `*dst = src.clone()`, dropping the value `dst` held.
///
/// # Safety
/// Both pointers must point at valid values.
pub unsafe extern "C" fn assign_clone(src: *const Value, dst: *mut Value) {
    *dst = (*src).clone();
}

/// Drops the value in a stack slot that is being popped.
///
/// # Safety
/// `p` must point at a valid value, which is not used again.
pub unsafe extern "C" fn drop_value(p: *mut Value) {
    ptr::drop_in_place(p);
}

/// Runs `arr.push(v)` for native code, when the callee at `callee` is the built-in `push` and `this`,
/// the slot above it, is an array. The argument, the slot above `this`, moves into the array, and the
/// result replaces the callee. Returns false, and changes nothing, when `this` is not an array.
///
/// # Safety
/// `callee` must point at three valid values, the callee being the built-in `push`. The argument slot
/// is dead afterwards.
pub unsafe extern "C" fn push_call(callee: *mut Value) -> bool {
    let (this, arg) = (callee.add(1), callee.add(2));
    let Value::Arr(items) = &*this else {
        return false;
    };
    let v = ptr::read(arg);
    let len = {
        let mut vec = items.borrow_mut();
        vec.push(v);
        vec.len()
    };
    // `this` held one reference to the array, which is released here, as the slot is dead after.
    let Value::Arr(items) = ptr::read(this) else {
        unreachable!("`this` was checked to be an array")
    };
    drop(items);
    ptr::write(callee, Value::Num(len as f64));
    true
}

/// Runs `s.charCodeAt(i)` for native code, when the callee at `callee` is the built-in `charCodeAt`, `this`, the
/// slot above it, is a string and the argument is a number. The receiver is released and the result replaces the
/// callee. Returns false, and changes nothing, otherwise.
///
/// # Safety
/// `callee` must point at three valid values. The receiver and argument slots are dead afterwards.
pub unsafe extern "C" fn char_code_call(callee: *mut Value) -> bool {
    let (this, arg) = (callee.add(1), callee.add(2));
    let code = match (&*this, &*arg) {
        (Value::Str(s), Value::Num(i)) => builtins::string::char_code(s, *i),
        _ => return false,
    };
    // The receiver is released here, as the slot is dead afterwards. A number owns nothing.
    ptr::drop_in_place(this);
    ptr::write(callee, Value::Num(code));
    true
}

/// Runs `String(n)` for native code, when the callee at `callee` is the built-in `String` and the argument is a
/// number. The receiver of a plain call is `undefined`, and it is released anyway. The result replaces the callee.
/// Returns false, and changes nothing, otherwise.
///
/// # Safety
/// `callee` must point at three valid values. The receiver and argument slots are dead afterwards.
pub unsafe extern "C" fn string_call(callee: *mut Value) -> bool {
    let (this, arg) = (callee.add(1), callee.add(2));
    let Value::Num(n) = &*arg else {
        return false;
    };
    let text = crate::value::num_to_string(*n);
    ptr::drop_in_place(this);
    ptr::write(callee, Value::from_string(text));
    true
}

/// `s.length` for native code, when the value at `top` is a string: the string is replaced by its length in
/// characters. Anything else writes nothing and returns false.
///
/// # Safety
/// `top` must point at a valid value that the instruction pops.
pub unsafe extern "C" fn str_length_top(top: *mut Value) -> bool {
    let Value::Str(s) = &*top else {
        return false;
    };
    let len = s.char_count() as f64;
    ptr::drop_in_place(top);
    ptr::write(top, Value::Num(len));
    true
}

/// Moves the value at `src` into `dst`, dropping what `dst` held. `src` is dead afterwards.
///
/// # Safety
/// Both pointers must point at valid values.
pub unsafe extern "C" fn move_into(src: *mut Value, dst: *mut Value) {
    let v = ptr::read(src);
    *dst = v;
}

/// `*dst = the value of upvalue idx` of the running closure.
///
/// # Safety
/// `vm` must be the running VM, with a frame on it. `dst` must be a dead slot.
pub unsafe extern "C" fn upval_into(vm: *mut Vm, idx: usize, dst: *mut Value) {
    let v = match &(*vm).frames.last().unwrap().closure().upvals[idx] {
        Value::Cell(cell) => cell.borrow().clone(),
        value => value.clone(),
    };
    ptr::write(dst, v);
}

/// `*dst = a closure made by `Op::Closure(idx)``, as `op` makes it. It cannot fail, so the frame keeps the instruction
/// pointer it has.
///
/// # Safety
/// Same as `op`: the frame must be on the stack, and `depth` must fit in the capacity reserved for it. `dst` must be
/// a dead slot.
pub unsafe extern "C" fn make_closure(vm: *mut Vm, depth: usize, idx: usize, dst: *mut Value) {
    let vm = &mut *vm;
    let next = vm.frames.last().unwrap().ip;
    enter(vm, next, depth);
    let v = vm.make_closure(idx);
    ptr::write(dst, v);
}

/// The call of a factory (see `Proto::factory`), which makes the closure that the function returns, over its arguments,
/// without a frame. The callee holds the factory, and is replaced by the closure. The `argc` arguments at `args` are
/// released once their values have gone to the closure. A call that does not fit, with another number of arguments or with
/// a `this` that is not `undefined`, is declined with status 1, before anything has changed.
///
/// # Safety
/// `callee` must hold a function value whose prototype is a factory, and `args` the `argc` values of the call, as the
/// operands of a call are.
pub unsafe extern "C" fn call_factory(
    callee: *mut Value,
    this: *const Value,
    args: *mut Value,
    argc: usize,
) -> u64 {
    let Value::Func(f) = &*callee else {
        return 1;
    };
    if argc != f.proto.nparams as usize || !matches!(*this, Value::Undef) {
        return 1;
    }
    let child = f.proto.protos[f.proto.factory.get() as usize - 1].clone();
    // Each captured parameter moves into the closure, which leaves its argument undefined. `factory_child` admits a factory
    // only when its captures name distinct parameters, so no argument is moved twice.
    let upvals: Vec<Value> = child
        .upvals
        .iter()
        .map(|u| ptr::replace(args.add(u.index as usize), Value::Undef))
        .collect();
    let made = Value::Func(Rc::new(crate::value::Closure {
        proto: child,
        upvals,
        this: ManuallyDrop::new(Value::Undef),
        class: Default::default(),
    }));
    for j in 0..argc {
        // A value that holds no reference, such as a moved-out `undefined` or a number, needs no drop glue call.
        let arg = args.add(j);
        if (*arg).owns_ref() {
            ptr::drop_in_place(arg);
        }
        ptr::write(arg, Value::Undef);
    }
    *callee = made;
    0
}

/// Boxes the parameter in slot `idx` of the running frame, as `op` does for `Op::BoxParam`.
///
/// # Safety
/// Same as `make_closure`.
pub unsafe extern "C" fn box_param(vm: *mut Vm, depth: usize, idx: usize) {
    let vm = &mut *vm;
    let next = vm.frames.last().unwrap().ip;
    enter(vm, next, depth);
    vm.box_param(idx);
}

/// `*dst = the value of the local at src`, which may be a boxed (captured) variable.
///
/// # Safety
/// `src` must point at a valid value, and `dst` at a dead slot.
pub unsafe extern "C" fn boxed_into(src: *const Value, dst: *mut Value) {
    let v = match &*src {
        Value::Cell(c) => c.borrow().clone(),
        other => other.clone(),
    };
    ptr::write(dst, v);
}

/// `*dst = src.name`, when `src` is an object. For any other value this writes nothing and
/// returns false, so the caller can run the instruction through its helper, which reports
/// the error.
///
/// # Safety
/// `src` must point at a valid value, `name` at a key that outlives the code, `hint` at the cell for that name, and
/// `dst` at a dead slot.
pub unsafe extern "C" fn get_prop_into(
    src: *const Value,
    name: *const Rc<str>,
    dst: *mut Value,
    hint: *const PropHint,
) -> bool {
    match &*src {
        Value::Obj(o) => match lookup_hinted(o, &*name, &*hint) {
            Lookup::Data(v) => {
                ptr::write(dst, v);
                true
            }
            Lookup::Missing => {
                ptr::write(dst, Value::Undef);
                true
            }
            // A getter is run by the interpreter, which the caller falls back to.
            Lookup::Getter(_) => false,
        },
        Value::Arr(a) => {
            ptr::write(dst, array_prop(a, &*name, &(*hint).index));
            true
        }
        Value::Str(s) => {
            ptr::write(dst, string_prop(s, &*name, &(*hint).index));
            true
        }
        _ => false,
    }
}

/// Replaces the object or array in `slot` by its property `name`. For any other value this
/// writes nothing and returns false, as `get_prop_into` does.
///
/// # Safety
/// `slot` must point at a valid value that is popped by the instruction, and `name` and `hint` are as above.
pub unsafe extern "C" fn get_prop_top(
    slot: *mut Value,
    name: *const Rc<str>,
    hint: *const PropHint,
) -> bool {
    let v = match &*slot {
        Value::Obj(o) => match lookup_hinted(o, &*name, &*hint) {
            Lookup::Data(v) => v,
            Lookup::Missing => Value::Undef,
            Lookup::Getter(_) => return false,
        },
        Value::Arr(a) => array_prop(a, &*name, &(*hint).index),
        Value::Str(s) => string_prop(s, &*name, &(*hint).index),
        _ => return false,
    };
    *slot = v;
    true
}

/// `x === y`, or `x !== y` when `negate` is set, for values of any kind. It replaces `x` by the boolean, and drops
/// `y`, which the comparison consumes. Comparisons of two numbers never reach it.
///
/// # Safety
/// `x` and `y` must point at valid values that the instruction pops.
pub unsafe extern "C" fn strict_eq_into(x: *mut Value, y: *mut Value, negate: u64) {
    let equal = crate::value::strict_eq(&*x, &*y) != (negate != 0);
    ptr::drop_in_place(x);
    ptr::drop_in_place(y);
    ptr::write(x, Value::Bool(equal));
}

/// Reads the global `name` into `dst`. A name that is not defined writes nothing and returns false, so the
/// instruction's own path reports the error.
///
/// # Safety
/// `vm` must be the running VM, `name` as for `get_prop_top`, `dst` a dead slot, and `hint` the `global_hints`
/// cell of the same name in the same prototype.
pub unsafe extern "C" fn get_global_into(
    vm: *mut Vm,
    name: *const Rc<str>,
    dst: *mut Value,
    hint: *const Cell<u32>,
) -> bool {
    match (*vm).globals.get_hinted(&*name, &*hint) {
        Some(v) => {
            ptr::write(dst, v.clone());
            true
        }
        None => false,
    }
}

/// `obj.name = v` for an object: the object in `obj` is replaced by `v`, which is moved out of `val`, and the
/// property keeps a copy. Anything else writes nothing and returns false, so the instruction's own path runs it.
///
/// # Safety
/// `obj` and `val` must point at valid values that the instruction pops, and `name` and `hint` are as for
/// `get_prop_top`.
pub unsafe extern "C" fn set_prop_top(
    obj: *mut Value,
    name: *const Rc<str>,
    val: *mut Value,
    hint: *const PropHint,
) -> bool {
    let Value::Obj(o) = &*obj else {
        return false;
    };
    // An assignment that a setter on the prototype chain catches runs in the interpreter.
    if o.borrow().proto.is_some() && find_setter(o, &**name).is_some() {
        return false;
    }
    obj_put_hinted(o, &*name, (*val).clone(), &*hint);
    *obj = ptr::read(val);
    true
}

/// `a % b` for two numbers: `a` is replaced by the remainder. Anything else writes nothing and returns false.
///
/// # Safety
/// `a` and `b` must point at valid values that the instruction pops.
pub unsafe extern "C" fn mod_top(a: *mut Value, b: *const Value) -> bool {
    let (Value::Num(x), Value::Num(y)) = (&*a, &*b) else {
        return false;
    };
    let rem = crate::value::js_mod(*x, *y);
    // `a` holds a number, which owns nothing, so it is overwritten without a drop.
    ptr::write(a, Value::Num(rem));
    true
}

/// `obj[key]` for an array and a number in range: the array in `obj` is replaced by the element. Anything else
/// writes nothing and returns false, so the instruction's own path runs it.
///
/// # Safety
/// `obj` and `key` must point at valid values that the instruction pops.
pub unsafe extern "C" fn get_index_top(obj: *mut Value, key: *const Value) -> bool {
    if let (Value::Str(s), Value::Num(n)) = (&*obj, &*key) {
        let v = crate::vm::index_of(*n)
            .and_then(|i| builtins::string::char_at(s, i))
            .map_or(Value::Undef, crate::value::char_value);
        *obj = v;
        return true;
    }
    let (Value::Arr(a), Value::Num(n)) = (&*obj, &*key) else {
        return false;
    };
    let Some(i) = crate::vm::index_of(*n) else {
        return false;
    };
    let Ok(items) = a.try_borrow() else {
        return false;
    };
    let Some(element) = items.get(i).cloned() else {
        return false;
    };
    drop(items);
    *obj = element;
    true
}

/// `obj[key] = v` for an array and a number of an existing element: the element takes a copy of `v`, and the
/// array in `obj` is replaced by `v`, which is moved out of `val`. Growing the array is left to the instruction's
/// own path, as is anything that is not an array and a number, which writes nothing and returns false.
///
/// # Safety
/// `obj`, `key` and `val` must point at valid values that the instruction pops. `val` is dead afterwards.
pub unsafe extern "C" fn set_index_top(
    obj: *mut Value,
    key: *const Value,
    val: *mut Value,
) -> bool {
    let (Value::Arr(a), Value::Num(n)) = (&*obj, &*key) else {
        return false;
    };
    let Some(i) = crate::vm::index_of(*n) else {
        return false;
    };
    let Ok(mut items) = a.try_borrow_mut() else {
        return false;
    };
    if i >= items.len() {
        return false;
    }
    let old = std::mem::replace(&mut items[i], (*val).clone());
    drop(items);
    drop(old);
    *obj = ptr::read(val);
    true
}

/// `local[key]` for a number `key`, on an array or a string in `obj`: the key is replaced by the element, and an
/// index past the end gives `undefined`. Anything else writes nothing and returns false, so the instruction's own
/// path runs it.
///
/// # Safety
/// `obj` must point at a valid value that the instruction does not pop, and `key` at one that it pops.
pub unsafe extern "C" fn get_index_local(obj: *const Value, key: *mut Value) -> bool {
    let Value::Num(n) = &*key else {
        return false;
    };
    let element = match &*obj {
        Value::Arr(a) => {
            let Ok(items) = a.try_borrow() else {
                return false;
            };
            crate::vm::index_of(*n).and_then(|i| items.get(i).cloned())
        }
        Value::Str(s) => crate::vm::index_of(*n)
            .and_then(|i| builtins::string::char_at(s, i))
            .map(crate::value::char_value),
        _ => return false,
    };
    // `key` holds a number, which owns nothing, so it is overwritten without a drop.
    ptr::write(key, element.unwrap_or(Value::Undef));
    true
}

/// `local[k1][k2]` for numbers, on an array of arrays in `obj`: `k1` is replaced by the element. A row that is not
/// an array, which includes one past the end of the outer array, writes nothing and returns false, so the
/// instruction's own path runs it and reports the error.
///
/// # Safety
/// `obj` must point at a valid value that the instruction does not pop, and `k1` and `k2` at values that it pops.
pub unsafe extern "C" fn get_index2_local(
    obj: *const Value,
    k1: *mut Value,
    k2: *const Value,
) -> bool {
    let (Value::Arr(a), Value::Num(n1), Value::Num(n2)) = (&*obj, &*k1, &*k2) else {
        return false;
    };
    let Ok(items) = a.try_borrow() else {
        return false;
    };
    let Some(Value::Arr(row)) = crate::vm::index_of(*n1).and_then(|i| items.get(i)) else {
        return false;
    };
    let Ok(cells) = row.try_borrow() else {
        return false;
    };
    let element = crate::vm::index_of(*n2).and_then(|i| cells.get(i).cloned());
    drop(cells);
    drop(items);
    // `k1` holds a number, which owns nothing, so it is overwritten without a drop.
    ptr::write(k1, element.unwrap_or(Value::Undef));
    true
}

/// `local[key] = v` for a number `key`, on an array in `obj`, where the element exists: the element takes a copy of
/// `v`, and `key` is replaced by `v`, which is moved out of `val`. Growing the array, and anything else, writes
/// nothing and returns false, so the instruction's own path runs it.
///
/// # Safety
/// `obj` must point at a valid value that the instruction does not pop, and `key` and `val` at valid values that it
/// pops. `val` is dead afterwards when this returns true.
pub unsafe extern "C" fn set_index_local(
    obj: *const Value,
    key: *mut Value,
    val: *const Value,
) -> bool {
    let (Value::Arr(a), Value::Num(n)) = (&*obj, &*key) else {
        return false;
    };
    let Some(i) = crate::vm::index_of(*n) else {
        return false;
    };
    let Ok(mut items) = a.try_borrow_mut() else {
        return false;
    };
    if i >= items.len() {
        return false;
    }
    let old = std::mem::replace(&mut items[i], (*val).clone());
    drop(items);
    drop(old);
    ptr::write(key, ptr::read(val));
    true
}

/// `local[k1][k2] = v` for numbers, on an array of arrays in `obj`, where the row and the element exist: the element
/// takes a copy of `v`, and `k1` is replaced by `v`, which is moved out of `val`. Anything else writes nothing and
/// returns false, so the instruction's own path runs it.
///
/// # Safety
/// `obj` must point at a valid value that the instruction does not pop, and `k1`, `k2` and `val` at valid values
/// that it pops. `val` is dead afterwards when this returns true.
pub unsafe extern "C" fn set_index2_local(
    obj: *const Value,
    k1: *mut Value,
    k2: *const Value,
    val: *const Value,
) -> bool {
    let (Value::Arr(a), Value::Num(n1), Value::Num(n2)) = (&*obj, &*k1, &*k2) else {
        return false;
    };
    let (Some(i1), Some(i2)) = (crate::vm::index_of(*n1), crate::vm::index_of(*n2)) else {
        return false;
    };
    // The row is cloned out, so that no borrow of the outer array is held while the row is written.
    let row = {
        let Ok(items) = a.try_borrow() else {
            return false;
        };
        match items.get(i1) {
            Some(Value::Arr(row)) => row.clone(),
            _ => return false,
        }
    };
    let Ok(mut cells) = row.try_borrow_mut() else {
        return false;
    };
    if i2 >= cells.len() {
        return false;
    }
    let old = std::mem::replace(&mut cells[i2], (*val).clone());
    drop(cells);
    drop(row);
    drop(old);
    ptr::write(k1, ptr::read(val));
    true
}

/// The property `name` of a string: its length, or one of its methods.
fn string_prop(s: &JsStr, name: &str, hint: &Cell<u32>) -> Value {
    if name == "length" {
        Value::Num(s.char_count() as f64)
    } else {
        builtins::string::method_hinted(name, hint)
            .map(Value::Native)
            .unwrap_or(Value::Undef)
    }
}

/// The property `name` of an array: its length, or one of its methods.
fn array_prop(a: &ArrRef, name: &str, hint: &Cell<u32>) -> Value {
    if name == "length" {
        Value::Num(a.borrow().len() as f64)
    } else {
        builtins::array::method_hinted(name, hint)
            .or_else(|| builtins::global::object_method(name))
            .map(Value::Native)
            .unwrap_or(Value::Undef)
    }
}

/// Sets property `name` of the object at `obj` to the value moved out of `val`. A value that
/// is not an object is dropped, as the interpreter does.
///
/// # Safety
/// `obj` and `val` must point at valid values, and `val` is dead afterwards. `name` and `hint` are as above.
pub unsafe extern "C" fn init_prop(
    obj: *const Value,
    name: *const Rc<str>,
    val: *mut Value,
    hint: *const PropHint,
) {
    let v = ptr::read(val);
    if let Value::Obj(o) = &*obj {
        obj_init_hinted(o, &*name, v, &*hint);
    }
}

/// `*dst = {}` with room for `capacity` properties.
///
/// # Safety
/// `dst` must point at a dead slot.
pub unsafe extern "C" fn new_object(capacity: usize, dst: *mut Value) {
    ptr::write(dst, Value::obj_with(capacity));
}

/// Pops the frame of the function, leaving its result for the caller. The caller's frame, which
/// native code that called this one continues in, is returned as `base`.
///
/// # Safety
/// Same as `op`.
pub unsafe extern "C" fn ret(vm: *mut Vm, ip: usize, depth: usize) -> HelperRet {
    let vm = &mut *vm;
    enter(vm, ip + 1, depth);
    vm.do_return();
    let base = vm.frames.last().map_or(0, |f| f.base);
    HelperRet {
        status: STATUS_DONE,
        base: vm.stack.as_mut_ptr().add(base),
    }
}
