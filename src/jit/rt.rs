//! The functions native code calls. Each runs one instruction (or one call or return) through
//! the interpreter's own code in `Vm`, so an instruction behaves the same whichever path runs it.

use std::ptr;
use std::rc::Rc;

use crate::builtins;
use crate::bytecode::Proto;
use crate::value::{obj_get_key, obj_put, ArrRef, Value};
use crate::vm::Vm;

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
    let code = match vm.push_callee(argc) {
        Some(code) => (*code).entry() as u64,
        None => {
            let idx = vm.stack.len() - argc - 2;
            if vm.push_in_place(idx, argc) {
                STATUS_DONE
            } else {
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

/// `*dst = src.clone()`, where `dst` is a dead slot that holds nothing to drop.
///
/// # Safety
/// Both pointers must point at valid values, or at a dead slot for `dst`.
pub unsafe extern "C" fn clone_into(src: *const Value, dst: *mut Value) {
    ptr::write(dst, (*src).clone());
}

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
    let v = (*vm).frames.last().unwrap().closure().upvals[idx]
        .borrow()
        .clone();
    ptr::write(dst, v);
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
/// `src` must point at a valid value, `name` at a key that outlives the code, and `dst` at a
/// dead slot.
pub unsafe extern "C" fn get_prop_into(
    src: *const Value,
    name: *const Rc<str>,
    dst: *mut Value,
) -> bool {
    match &*src {
        Value::Obj(o) => {
            let v = obj_get_key(o, &*name).unwrap_or(Value::Undef);
            ptr::write(dst, v);
            true
        }
        Value::Arr(a) => {
            ptr::write(dst, array_prop(a, &*name));
            true
        }
        _ => false,
    }
}

/// Replaces the object or array in `slot` by its property `name`. For any other value this
/// writes nothing and returns false, as `get_prop_into` does.
///
/// # Safety
/// `slot` must point at a valid value that is popped by the instruction, and `name` is as above.
pub unsafe extern "C" fn get_prop_top(slot: *mut Value, name: *const Rc<str>) -> bool {
    let v = match &*slot {
        Value::Obj(o) => obj_get_key(o, &*name).unwrap_or(Value::Undef),
        Value::Arr(a) => array_prop(a, &*name),
        _ => return false,
    };
    *slot = v;
    true
}

/// The property `name` of an array: its length, or one of its methods.
fn array_prop(a: &ArrRef, name: &str) -> Value {
    if name == "length" {
        Value::Num(a.borrow().len() as f64)
    } else {
        builtins::array::method(name)
            .map(Value::Native)
            .unwrap_or(Value::Undef)
    }
}

/// Sets property `name` of the object at `obj` to the value moved out of `val`. A value that
/// is not an object is dropped, as the interpreter does.
///
/// # Safety
/// `obj` and `val` must point at valid values, and `val` is dead afterwards. `name` is as above.
pub unsafe extern "C" fn init_prop(obj: *const Value, name: *const Rc<str>, val: *mut Value) {
    let v = ptr::read(val);
    if let Value::Obj(o) = &*obj {
        obj_put(o, &*name, v);
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
