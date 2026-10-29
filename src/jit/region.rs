//! Code that works on numbers, with its locals in registers.
//!
//! The generic code keeps each local in its frame slot and loads and stores it on every step. Two
//! kinds of region avoid that for code that cannot run user code: numbers, locals, arithmetic,
//! comparisons and jumps. A loop region can also read and write the number properties of an object
//! in a local, which it reaches in place (see `Mark::View`), take an object out of an array (see
//! `Mark::Owned`), and call the built-in `sqrt` on `Math`. Each of these checks what it relies on,
//! and a check that fails leaves the loop at that instruction, which the generic code then runs.
//!
//! A loop region loads the loop's locals into xmm registers once, runs the loop on them, and writes
//! them back when it leaves. A function region is a whole function that the Rust side calls with
//! numbers. It keeps its locals in its native frame, and calls itself directly, without frames in
//! the VM. The function returns through `Native::call_numbers`, and a native stack that runs out
//! sends the call back to the interpreter, which is safe because the code has no side effects.
//!
//! A local that a loop reads and that may not hold a number is checked on entry, and the region
//! returns at once when it is not a number, so the generic code runs the loop instead.

use super::codegen::{
    emit_mod_const, find_global, find_prop, int_divisor, local_uses, magic, successors, PropSite,
    OWNING, TAG_ARR, TAG_BOOL, TAG_CELL, TAG_FUNC, TAG_NATIVE, TAG_NUM, TAG_OBJ,
};
use super::rt;
use super::types::Numbers;
use super::x64::*;
use super::Batch;
use crate::bytecode::{Op, Proto};
use crate::value::{
    arr_layout, cell_value_offset, entry_layout, epoch_address, prop_layout, PropHint, Value,
};
use std::mem::offset_of;

/// The xmm registers that hold locals of a loop, from xmm2 up. xmm0 and xmm1 are scratch.
const LOCAL_REGS: std::ops::Range<u8> = 2..8;
/// The xmm register of the operand at depth 0 in a loop. Operand `k` is in `TEMP_BASE + k`, up to xmm15.
const TEMP_BASE: u8 = 8;
/// The deepest operand stack a loop handles, which leaves the registers above it for constants.
const MAX_DEPTH: i32 = 7;
/// The xmm register of the operand at depth 0 in a function. The locals of a function are in its frame, so
/// the registers from xmm2 up are free for operands, and xmm0 and xmm1 stay scratch.
const FN_TEMP_BASE: u8 = 2;
/// The deepest operand stack a function handles. Operand `k` is in `FN_TEMP_BASE + k`, up to xmm15.
const FN_MAX_DEPTH: i32 = 13;
/// Arguments a function region takes, in xmm0 and up.
pub(super) const MAX_ARGS: usize = 6;
/// The bytes of a loop region's frame, below the saved `r14`, `r15` and `r13`: room to save the xmm registers a call
/// clobbers, the tag cells, the row caches, and the property caches with the epoch that follows them, and a size that
/// keeps rsp 16-byte aligned at those calls.
const FRAME: i32 = 312;
/// Where the tag of an element operand is kept, one byte per depth. `call_mod` saves its registers below this.
const TAG_CELLS: i32 = 136;
/// Where the row caches of a loop start, above the tag cells. Each read of a row takes a key and a row, 8 bytes each
/// (see `row_cached`).
const ROW_CACHE_BASE: i32 = 144;
/// The most reads of rows that a loop keeps a row cache for.
const MAX_ROW_CACHES: usize = 4;
/// Where the argument of an inlined body is kept, as a value: its tag here and its payload 8 bytes up (see `arg_payload`).
/// It lies above the row caches and below the property caches, which do not overlap it.
const ARG_SLOT: i32 = ROW_CACHE_BASE + 16 * MAX_ROW_CACHES as i32;
/// Where the property caches of a loop start, above the argument. Each one takes the address of a property's value, 8
/// bytes (see `prop_address`). The count of structural changes that they were last checked at follows them.
const PROP_CACHE_BASE: i32 = ARG_SLOT + 16;
/// The most properties that a loop keeps the address of.
const MAX_PROP_CACHES: usize = 8;
// The property caches and the epoch after them must lie in the frame, which is below the saved registers. The argument
// and the caches must not overlap.
const _: () = assert!(PROP_CACHE_BASE + 8 * (MAX_PROP_CACHES as i32 + 1) <= FRAME - 8);
const _: () = assert!(ARG_SLOT + 16 <= PROP_CACHE_BASE);
/// The key of an empty row cache. A key is a 32-bit index (see `row_cached`), so no key takes it.
const NO_KEY: u64 = 1 << 40;
/// The largest magnitude an integer copy of a local may have. It stays exact, and fits an imm32.
const INT_LIMIT: i32 = i32::MAX;
/// The stack that a function region may use, below the stack its wrapper was called on.
const STACK_BUDGET: i32 = 1 << 20;
/// What a loop region returns when a guard fails, which means the loop did not run.
const NOT_ENTERED: u64 = u64::MAX;

/// Where a local lives while a region runs.
#[derive(Clone, Copy)]
enum Home {
    /// In an xmm register, for the whole loop. It is written back to the frame when the loop leaves.
    Reg(u8),
    /// In the frame, at this address.
    Mem(Mem),
}

/// An operand that is read where it is, rather than copied into its depth's register first (see `Emitter::operand`).
#[derive(Clone, Copy)]
enum Src {
    /// A register that holds a local, or a constant of the pool.
    Reg(u8),
    /// The frame slot of a local.
    Mem(Mem),
}

/// The depths that can hold a pending operand. An operand depth is never near this many.
const PENDING: usize = 32;

/// What a function region needs to know about its function.
pub(super) struct Function {
    pub(super) nparams: u32,
    /// The upvalue that the function calls itself through, if it does. Native code calls the
    /// function's own code directly, which is only right if that upvalue holds the function.
    pub(super) self_upval: Option<u32>,
    /// Whether the function returns a boolean, which native code returns as 0.0 or 1.0.
    pub(super) returns_bool: bool,
    /// Whether the function reads `this`. Its entry then takes the receiver, and it returns `undefined` (see `method_admitted`).
    pub(super) receiver: bool,
    /// Whether the function reads a captured number, which its entry reads from the closure it is called with.
    pub(super) upvals: bool,
}

/// What the planner of a function region learns from the instructions it steps through (see `function_step`).
#[derive(Default)]
struct FnFacts {
    /// Whether the function reads a captured number (see `Function::upvals`).
    upvals: bool,
    /// The upvalue that the function calls itself through, if it does.
    self_upval: Option<u32>,
    /// Whether the function returns a boolean, once it returns a number or a boolean.
    ret_bool: Option<bool>,
    /// Whether the function returns `undefined`, which only a method does.
    ret_undefined: bool,
    /// Whether the function reads `this`.
    receiver: bool,
}

/// A region: a loop, or a whole function.
pub(super) struct Region {
    /// The first instruction of the region, which a loop's back jump goes to.
    header: usize,
    /// The last instruction of the region. For a loop, a jump back to `header`.
    last: usize,
    /// Where control enters the region: `header`, and any instruction in a loop that a jump from
    /// outside reaches. The generic code passes the one it reached in rdx.
    pub(super) entries: Vec<usize>,
    /// The instructions outside a loop that its jumps reach. Leaving at one returns its index.
    pub(super) exits: Vec<usize>,
    /// The locals, each with its home.
    locals: Vec<(u32, Home)>,
    /// The constants the loop uses as operands, by their bits, each in the register that holds it.
    consts: Vec<(u64, u8)>,
    /// The locals that a guard checks on entry: the loop reads them before any number is stored.
    guards: Vec<u32>,
    /// A local that the loop only steps by one, and that a `%` by an integer reads. Its integer copy is
    /// kept in r13, which makes that `%` skip the conversion and the exactness check of its number.
    int_local: Option<u32>,
    /// The divisor of the remainder that native code keeps in r15 for the integer copy, which steps
    /// up by one, and is taken by this one divisor (see `step_counter`).
    rem: Option<u32>,
    /// The locals that hold arrays. A loop reads and writes their elements in place, so they have no register,
    /// and the loop checks on entry that they hold an array.
    arrays: Vec<u32>,
    /// The reads of rows by a whole key (the `GetIndex2Local` at each one), which keep their last row in the frame (see
    /// `row_cached`).
    row_caches: Vec<usize>,
    /// The properties that a loop reads or writes through an object local, which keep the address of their value in the
    /// frame (see `prop_address`).
    prop_caches: Vec<(u32, u32)>,
    /// Whether the loop calls anything that may run script code, which can write any property to any value (see `plan`).
    calls: bool,
    /// The locals that hold objects, which a loop reads in place (see `Mark::View`). They have no register.
    pub(super) objects: Vec<u32>,
    /// The object locals that a path reads from the entry before it writes them, which the loop checks on entry.
    object_guards: Vec<u32>,
    /// The instructions that read an element into an object local (see `Mark::Owned`).
    object_reads: Vec<usize>,
    /// The global `Math` and the property `sqrt` that the loop reads through it, as name indices. The entry checks that
    /// `Math.sqrt` is the built-in, which nothing in the loop can change (see `math_reads`).
    math: Option<(u32, u32)>,
    /// The kind of each operand before each instruction of a loop, from `header` on (see `operand_marks`).
    marks: Vec<Vec<Mark>>,
    /// The locals that hold whole numbers, by local (see `plan`).
    whole: Vec<bool>,
    /// The whole locals that the loop uses and that a path carries in from before it, which the entry checks find whole.
    whole_guards: Vec<u32>,
    /// The instructions of a loop that read or write an element. A side exit resumes the generic code at one of
    /// them, with its operands written back, so the generic code dispatches them as exits.
    pub(super) resumes: Vec<usize>,
    /// Set for a function region.
    pub(super) function: Option<Function>,
    /// The method call that the loop runs in place, with its body, when `with_inline` finds one.
    pub(super) inline: Option<Box<Inline>>,
    /// The statements that run once before an inner loop, instead of on each of its trips (see `with_hoists`).
    pub(super) hoists: Vec<Hoist>,
}

/// A method that a loop region runs in place of its call (see `with_inline`): the call, the function that the callee
/// must be, that function's own region, and its depths.
pub(super) struct Inline {
    call: usize,
    proto: std::rc::Rc<Proto>,
    lp: Region,
    depth: Vec<i32>,
}

/// Where the body of an inlined method finds its receiver and its operands, and where it goes when it returns and when
/// it declines. A decline leaves at the call's exit, as a call that the entry declines does (see `call_method`).
#[derive(Clone, Copy)]
struct InlineCtx {
    receiver: Mem,
    reg_base: u8,
    exit: Label,
    cont: Label,
}

fn arg_payload() -> Mem {
    Mem {
        base: RSP,
        disp: ARG_SLOT + 8,
    }
}

/// A statement of a loop that computes the same number on every trip of an inner loop. The region runs it once, before
/// the jump that enters the inner loop, and skips it inside the loop (see `with_hoists`).
#[derive(Clone, Copy)]
pub(super) struct Hoist {
    /// The jump that enters the inner loop, which the statement runs before.
    pub(super) entry: usize,
    /// The first instruction of the statement.
    pub(super) start: usize,
    /// The last instruction of the statement: the `PopToLocal` that writes its local.
    pub(super) end: usize,
}

/// The locals that `op` reads. A `PopToLocal` or `SetLocal` only writes its local.
fn local_read(op: Op) -> Vec<u32> {
    match op {
        Op::PopToLocal(_) | Op::SetLocal(_) => Vec::new(),
        _ => local_uses(op),
    }
}

/// The local that `op` writes, if it writes one.
fn local_write(op: Op) -> Option<u32> {
    match op {
        Op::PopToLocal(i)
        | Op::SetLocal(i)
        | Op::IncLocal(i)
        | Op::DecLocal(i)
        | Op::AddToLocal(i) => Some(i),
        _ => None,
    }
}

/// Whether `op` computes a number from constants and locals, with no effect except the value it leaves. These are the
/// operations that a statement may hold (see `statement_end`).
fn pure_number(op: Op) -> bool {
    matches!(
        op,
        Op::Const(_)
            | Op::True
            | Op::False
            | Op::GetLocal(_)
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Mod
            | Op::Neg
            | Op::AddLK(..)
            | Op::SubLK(..)
            | Op::MulLK(..)
            | Op::ModLK(..)
    )
}

/// The `PopToLocal` that ends the statement which starts at `s`, inside a loop from `h` to `t` whose operand depth at the
/// header is `base`. The instructions from `s` to it are number operations that leave only the value the `PopToLocal`
/// writes, and no jump lands on a later one of them.
fn statement_end(
    proto: &Proto,
    depth: &[i32],
    s: usize,
    t: usize,
    base: i32,
    jumped: &[bool],
) -> Option<usize> {
    if depth[s] != base {
        return None;
    }
    for e in s..=t {
        if e > s && jumped[e] {
            return None;
        }
        match proto.code[e] {
            Op::PopToLocal(_) => return (depth[e] == base + 1).then_some(e),
            op if pure_number(op) => {}
            _ => return None,
        }
    }
    None
}

/// Whether a read of `x` inside the loop `h..=t` can run from its header without passing the statement at `s`.
fn reads_before(proto: &Proto, depth: &[i32], h: usize, t: usize, s: usize, x: u32) -> bool {
    let code = &proto.code;
    let mut seen = vec![false; code.len()];
    let mut stack = vec![h];
    while let Some(k) = stack.pop() {
        if !(h..=t).contains(&k) || k == s || seen[k] || depth[k] < 0 {
            continue;
        }
        seen[k] = true;
        if local_read(code[k]).contains(&x) {
            return true;
        }
        stack.extend(successors(code[k], k, depth[k]).into_iter().map(|(n, _)| n));
    }
    false
}

/// Whether the statement `s..=e` may run once before the inner loop `h..=t`. Nothing in the loop writes a local that the
/// statement reads, nothing but the statement writes its target, the target is read nowhere outside the loop, and every
/// read of it inside the loop comes after the statement on every path from the header.
fn hoistable(
    proto: &Proto,
    depth: &[i32],
    lp: &Region,
    (h, t): (usize, usize),
    (s, e): (usize, usize),
) -> bool {
    let code = &proto.code;
    let Op::PopToLocal(x) = code[e] else {
        return false;
    };
    let reads: Vec<u32> = (s..e).flat_map(|k| local_read(code[k])).collect();
    if [x]
        .iter()
        .chain(&reads)
        .any(|&i| lp.arrays.contains(&i) || lp.objects.contains(&i))
    {
        return false;
    }
    for k in h..=t {
        if let Some(w) = local_write(code[k]) {
            if reads.contains(&w) || (w == x && !(s..=e).contains(&k)) {
                return false;
            }
        }
    }
    let read_outside = (0..code.len())
        .filter(|&k| !(h..=t).contains(&k) && depth[k] >= 0)
        .any(|k| local_read(code[k]).contains(&x));
    !read_outside && !reads_before(proto, depth, h, t, s, x)
}

/// The jump that enters the inner loop `h..=t` of `lp`, when that jump is the only way in. A `for` or `while` loop is
/// entered by the jump just before its header, which goes to its test. Another jump into the loop, or a region entry in
/// it, would skip the statements that run before that jump.
fn loop_entry(proto: &Proto, depth: &[i32], lp: &Region, (h, t): (usize, usize)) -> Option<usize> {
    let code = &proto.code;
    let entry = h.checked_sub(1).filter(|&e| e >= lp.header)?;
    let inside = h..=t;
    let Op::Jump(test) = code[entry] else {
        return None;
    };
    if !inside.contains(&(test as usize)) || depth[h] < 0 || depth[entry] != depth[h] {
        return None;
    }
    if lp.entries.iter().any(|e| inside.contains(e)) {
        return None;
    }
    let other = (lp.header..=lp.last).any(|k| {
        k != entry
            && !inside.contains(&k)
            && depth[k] >= 0
            && successors(code[k], k, depth[k])
                .iter()
                .any(|&(n, _)| inside.contains(&n))
    });
    (!other).then_some(entry)
}

/// Returns `lp`, with the statements that run once before each inner loop of it, instead of on every trip. A statement
/// qualifies when `hoistable` says so, and it belongs to the innermost loop that holds it, which is the one it is hoisted
/// before. The loop must be entered only by the jump that `loop_entry` finds.
pub(super) fn with_hoists(proto: &Proto, depth: &[i32], mut lp: Region) -> Region {
    let code = &proto.code;
    let jumped = jump_targets(proto);
    // The loops inside the region: a back jump from `t` to `h`, where `h` is after the header.
    let mut loops: Vec<(usize, usize)> = Vec::new();
    for t in lp.header..=lp.last {
        if depth[t] < 0 {
            continue;
        }
        for (h, _) in successors(code[t], t, depth[t]) {
            if h > lp.header && h <= t {
                loops.push((h, t));
            }
        }
    }
    loops.sort_unstable();
    loops.dedup();
    let mut hoists = Vec::new();
    for &(h, t) in &loops {
        let Some(entry) = loop_entry(proto, depth, &lp, (h, t)) else {
            continue;
        };
        let mut s = h;
        while s <= t {
            let inner = loops.iter().any(|&(h2, t2)| h < h2 && h2 <= s && s <= t2);
            let end = if inner {
                None
            } else {
                statement_end(proto, depth, s, t, depth[h], &jumped)
            };
            match end {
                Some(e) if hoistable(proto, depth, &lp, (h, t), (s, e)) => {
                    hoists.push(Hoist {
                        entry,
                        start: s,
                        end: e,
                    });
                    s = e + 1;
                }
                _ => s += 1,
            }
        }
    }
    lp.hoists = hoists;
    lp
}

/// Returns `lp`, with the method call that its loop inlines, when there is one. The call is the first in the loop whose
/// callee is a method, and the method is the function that the loop's object literal makes for that name: the `Closure`
/// that is followed by `InitProp` or `SetProp` of the name. The body of that function runs in place of the call, behind a check that
/// the callee's prototype is that function (see `Emitter::inline_body`). Any other callee makes the call as before.
pub(super) fn with_inline(proto: &Proto, depth: &[i32], mut lp: Region) -> Region {
    let found = (lp.header..=lp.last).find_map(|ip| {
        if depth[ip] < 3 || proto.code[ip] != Op::Call(1) {
            return None;
        }
        match lp.marks[ip - lp.header][depth[ip] as usize - 3] {
            Mark::Method(_, name, _) => Some((ip, name)),
            _ => None,
        }
    });
    let Some((call, name)) = found else {
        return lp;
    };
    let mut sites = proto.code.windows(2).filter_map(|w| match w {
        [Op::Closure(i), Op::InitProp(n) | Op::SetProp(n)] if *n == name => Some(*i),
        _ => None,
    });
    let (Some(i), None) = (sites.next(), sites.next()) else {
        return lp;
    };
    let Some(child) = proto.protos.get(i as usize).cloned() else {
        return lp;
    };
    // An arrow function's `this` is not the receiver of the call, so it is never inlined.
    if child.nparams != 1 || child.has_rest || child.is_arrow {
        return lp;
    }
    let Some((child_depth, deepest)) = super::codegen::depths(&child) else {
        return lp;
    };
    let types = super::types::number_slots(&child, &child_depth, deepest, 1);
    let Some(mut body) = plan_function(&child, &child_depth, &types) else {
        return lp;
    };
    if !body.function.as_ref().is_some_and(|f| f.receiver) {
        return lp;
    }
    // The argument is read from the loop's frame, not from the register that the call takes it from.
    for (local, home) in body.locals.iter_mut() {
        if *local == 0 {
            *home = Home::Mem(arg_payload());
        }
    }
    // The body's operands start at the callee's depth, and must stay below the constants of the loop.
    if TEMP_BASE as i32 + depth[call] - 3 + deepest > 16 - lp.consts.len() as i32 {
        return lp;
    }
    lp.inline = Some(Box::new(Inline {
        call,
        proto: child,
        lp: body,
        depth: child_depth,
    }));
    lp
}

/// What an operand is, in the stack that `plan_function` follows.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    /// A number, in the operand's register.
    Num,
    /// A number that is whole, so its register holds an integral value. An index or a `%` takes it without checking
    /// that it is whole. A number out of the 32-bit range, or NaN, which an overflow can make, converts to the most
    /// negative int, which those uses refuse.
    Int,
    /// A boolean, from a comparison or `!`, held as 0.0 or 1.0. Only a jump or a return takes it.
    Bool,
    /// The function, pushed by `GetUpval`. Only a call takes it, and it has no register.
    Callee,
    /// The `this` of a plain call, which is `undefined`. Only a call takes it, and it has no register.
    This,
    /// An element of an array, which is a number or a boolean. Its register holds the number, or 0.0 or 1.0 for a
    /// boolean, and its tag is kept in the operand's tag cell, which a side exit writes back.
    Elem,
    /// The `push` of the array in local `s`, which only a call takes. It has no register, and the probe that
    /// `Op::GetLocalProp` runs there checked that the array has room.
    Push(u32),
    /// The array in local `s`, which only the call of a `push` takes as its receiver. It has no register.
    Array(u32),
    /// The object in local `s`, read in place: the operand has no register and no slot. The local must not be
    /// written while the operand is on the stack, since the operand reads the local when it is used.
    View(u32),
    /// An object that the operand owns a reference to, kept in the operand's slot of the value stack. Whatever takes it
    /// releases the reference.
    Owned,
    /// The built-in `sqrt`, which a property read of `sqrt` from an object gives. It has no register and no slot. The
    /// read checks that the property is the built-in (see `Op::GetProp`), and a side exit writes it back.
    Sqrt,
    /// The built-in `Math` global, which a loop reads through its name (see `Region::math`). It has no register and no
    /// slot, and nothing on the stack may need it at a side exit.
    Math,
    /// The `this` of a method, which a function region reads in place from a copy in its frame (see `emit_function`).
    /// Only a property read or write takes it, and it has no register or slot to write back.
    Receiver,
    /// The property `name` of the object in local `s`, which a call takes as its callee (see `method_read`). It has no
    /// register or slot: the call looks the property up, and a side exit resumes at the read, at instruction `read`.
    Method(u32, u32, u32),
}

/// Plans a loop region for the loop whose back jumps go to `header`. `None` if the loop has an
/// instruction a region cannot run, uses more locals than there are registers, or is entered with
/// operands on the stack.
pub(super) fn plan(proto: &Proto, depth: &[i32], types: &Numbers, header: usize) -> Option<Region> {
    let code = &proto.code;
    let last = (header..code.len()).rev().find(|&ip| {
        depth[ip] >= 0
            && successors(code[ip], ip, depth[ip])
                .iter()
                .any(|&(next, _)| next == header)
    })?;
    let inside = |ip: usize| (header..=last).contains(&ip);
    // A local that an index operation, a `push` or a `length` read names is an array. The loop reads its elements, or its
    // length, in place, and the entry checks that it is one.
    let mut arrays = vec![false; proto.nlocals as usize];
    for ip in header..=last {
        if depth[ip] >= 0 {
            let named = index_base(code[ip])
                .or_else(|| push_base(proto, code[ip]))
                .or_else(|| length_base(proto, code[ip]));
            if let Some(a) = named {
                arrays[a as usize] = true;
            }
        }
    }
    // A local that a property read names is an object, unless it is an array, whose length the loop reads in place. An
    // object local is only read and written as one, so a number operation that uses it is not planned.
    let mut objects = vec![false; proto.nlocals as usize];
    for ip in header..=last {
        if depth[ip] >= 0 {
            if let Some(s) = object_base(proto, code[ip]) {
                objects[s as usize] = true;
            }
        }
    }
    for i in 0..proto.nlocals as usize {
        if objects[i] && arrays[i] {
            return None;
        }
    }
    for ip in header..=last {
        if depth[ip] >= 0
            && local_uses(code[ip]).iter().any(|&i| objects[i as usize])
            && !matches!(code[ip], Op::GetLocal(_) | Op::PopToLocal(_))
        {
            return None;
        }
    }
    // An element that is stored in an object local is read as an object (see `Mark::Owned`).
    let object_reads: Vec<usize> = (header..last)
        .filter(|&ip| {
            depth[ip] >= 0
                && depth[ip + 1] >= 0
                && matches!(code[ip], Op::GetIndexLocal(_))
                && matches!(code[ip + 1], Op::PopToLocal(i) if objects[i as usize])
        })
        .collect();
    let mut resumes = Vec::new();
    let mut entries = vec![header];
    let mut exits = Vec::new();
    // The loops that enclose each instruction inside the region: a back jump and its target bound one of them. A use
    // counts sixteen times for each loop around it, since the loop's instructions run once per iteration.
    let mut nest = vec![0u32; code.len()];
    for ip in header..=last {
        if depth[ip] < 0 {
            continue;
        }
        for (t, _) in successors(code[ip], ip, depth[ip]) {
            if (header..=ip).contains(&t) {
                for k in t..=ip {
                    nest[k] += 1;
                }
            }
        }
    }
    let mut uses = vec![0u64; proto.nlocals as usize];
    let mut other_writes = vec![false; proto.nlocals as usize];
    let mut mod_uses = vec![0u32; proto.nlocals as usize];
    // The divisor of the first `local % c` on each local, and whether a later one differs or the local is
    // decremented, which keeps a remainder from stepping with it.
    let mut mod_divisor = vec![0u32; proto.nlocals as usize];
    let mut mod_mixed = vec![false; proto.nlocals as usize];
    let mut decremented = vec![false; proto.nlocals as usize];
    let mut reads = Vec::new();
    let mut deepest = 0;
    // The constants the region uses, by their bits, with the weight of their uses.
    let mut wanted: Vec<(u64, u64)> = Vec::new();
    for ip in 0..code.len() {
        let d = depth[ip];
        if d < 0 {
            continue;
        }
        let op = code[ip];
        if !inside(ip) {
            // A jump from outside the loop makes its target an entry.
            for (next, _) in successors(op, ip, d) {
                if inside(next) && !entries.contains(&next) {
                    entries.push(next);
                }
            }
            continue;
        }
        if !allowed(proto, op) {
            return None;
        }
        // A side exit of an operation that reads or writes an element, a property or an object resumes at it.
        let object_side = object_base(proto, op).is_some()
            || matches!(op, Op::GetProp(_) | Op::SetProp(_) | Op::GetGlobal(_))
            || matches!(op, Op::PopToLocal(i) if objects[i as usize])
            || (!object_reads.is_empty() && matches!(op, Op::Pop | Op::Dup | Op::Call(1)));
        if index_base(op).is_some() || push_base(proto, op).is_some() || object_side {
            resumes.push(ip);
        }
        if is_test(op) && !consumed(code, ip) {
            return None;
        }
        if d > MAX_DEPTH {
            return None;
        }
        deepest = deepest.max(d + 1);
        if let Some(bits) = constant_of(proto, op) {
            let weight = 16u64.saturating_pow(nest[ip]);
            match wanted.iter_mut().find(|(b, _)| *b == bits) {
                Some((_, w)) => *w = w.saturating_add(weight),
                None => wanted.push((bits, weight)),
            }
        }
        for (next, next_depth) in successors(op, ip, d) {
            if !inside(next) {
                if next_depth != 0 {
                    return None;
                }
                if !exits.contains(&next) {
                    exits.push(next);
                }
            }
        }
        let weight = 16u64.saturating_pow(nest[ip]);
        for i in local_uses(op) {
            uses[i as usize] = uses[i as usize].saturating_add(weight);
        }
        match op {
            Op::SetLocal(i) | Op::PopToLocal(i) | Op::AddToLocal(i) => {
                other_writes[i as usize] = true
            }
            Op::DecLocal(i) => decremented[i as usize] = true,
            Op::ModLK(s, k) => {
                if let Some(c) = int_divisor(number(proto, k)) {
                    mod_uses[s as usize] += 1;
                    let seen = &mut mod_divisor[s as usize];
                    if *seen == 0 {
                        *seen = c;
                    } else if *seen != c {
                        mod_mixed[s as usize] = true;
                    }
                }
            }
            _ => {}
        }
        reads.extend(
            reads_of(op)
                .into_iter()
                .filter(|&i| !arrays[i as usize] && !objects[i as usize])
                .map(|i| (ip, i)),
        );
    }
    if entries.iter().any(|&e| depth[e] != 0) {
        return None;
    }
    // The integer copy needs its steps to leave the operands empty, since they leave the region.
    let int_local = (0..proto.nlocals)
        .filter(|&i| mod_uses[i as usize] > 0 && !other_writes[i as usize])
        .max_by_key(|&i| mod_uses[i as usize])
        .filter(|&i| {
            (header..=last).all(|ip| {
                !matches!(code[ip], Op::IncLocal(j) | Op::DecLocal(j) if j == i)
                    || depth[ip + 1] == 0
            })
        });
    let rem = int_local.and_then(|i| {
        let u = i as usize;
        (!mod_mixed[u] && !decremented[u]).then_some(mod_divisor[u])
    });
    if let Some(i) = int_local {
        for ip in header..=last {
            if depth[ip] >= 0 && matches!(code[ip], Op::IncLocal(j) | Op::DecLocal(j) if j == i) {
                // A step that leaves the integer copy's range leaves the region after the step.
                if !exits.contains(&(ip + 1)) {
                    exits.push(ip + 1);
                }
            }
        }
    }
    let mut order: Vec<u32> = (0..proto.nlocals)
        .filter(|&i| uses[i as usize] > 0 && !arrays[i as usize] && !objects[i as usize])
        .collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(uses[i as usize]), i));
    // The most used locals take the registers, and the others stay in their frame slots.
    let locals: Vec<(u32, Home)> = order
        .iter()
        .enumerate()
        .map(|(k, &i)| match LOCAL_REGS.clone().nth(k) {
            Some(x) => (i, Home::Reg(x)),
            None => (i, Home::Mem(local_mem(i).at(8))),
        })
        .collect();
    // The integer copy lives in a register, so its local must have one.
    let int_local = int_local.filter(|&i| {
        locals
            .iter()
            .any(|&(l, h)| l == i && matches!(h, Home::Reg(_)))
    });
    let rem = if int_local.is_some() { rem } else { None };
    // Constants take registers from the top down, which must not reach the operand registers. The most used ones
    // take them, and the others load into a scratch register where they are used.
    let free = 16usize.saturating_sub(TEMP_BASE as usize + deepest as usize);
    wanted.sort_by_key(|&(_, weight)| std::cmp::Reverse(weight));
    let consts: Vec<(u64, u8)> = wanted
        .iter()
        .take(free)
        .enumerate()
        .map(|(n, &(bits, _))| (bits, (15 - n) as u8))
        .collect();
    // A read is only safe where the analysis knows the local is a number. Elsewhere the entry must
    // check it.
    let mut guards: Vec<u32> = Vec::new();
    for (ip, i) in reads {
        if !known(types, ip, i) && !guards.contains(&i) {
            guards.push(i);
        }
    }
    // A local that a path can carry from before the loop to a read or to an exit where it is still used is
    // written back from its register as it is, so it must hold a number on entry as well.
    let live = live_locals(code, depth, proto.nlocals as usize);
    for &(i, _) in &locals {
        if !known(types, header, i)
            && !guards.contains(&i)
            && carries_entry_value(code, depth, &entries, &inside, &live, i)
        {
            guards.push(i);
        }
    }
    let object_guards: Vec<u32> = (0..proto.nlocals)
        .filter(|&i| {
            objects[i as usize]
                && entries
                    .iter()
                    .any(|&e| reads_before_write(proto, depth, header, last, e, i))
        })
        .collect();
    let math = math_reads(proto, depth, header, last)?;
    // A local holds whole numbers while every store to it stores one, as the marks of the stack show. The marks depend on
    // the locals, so the set only narrows until the marks agree with it.
    let mut whole: Vec<bool> = (0..proto.nlocals)
        .map(|i| !arrays[i as usize] && !objects[i as usize])
        .collect();
    let marks = loop {
        let kinds = Kinds {
            arrays: &arrays,
            objects: &objects,
            object_reads: &object_reads,
            math_global: math.map(|m| m.0),
            math_sqrt: math.map(|m| m.1),
            whole: &whole,
        };
        let marks = operand_marks(proto, depth, header, last, &entries, &kinds)?;
        let mut narrowed = false;
        for ip in header..=last {
            if depth[ip] < 0 {
                continue;
            }
            let top = marks[ip - header].last().copied();
            let store = match code[ip] {
                // A store keeps its local whole when the value on top is whole.
                Op::SetLocal(i) | Op::PopToLocal(i) | Op::AddToLocal(i) => {
                    Some((i, top == Some(Mark::Int)))
                }
                // A string, or a cell, is not a whole number.
                Op::ConcatLocal(i) | Op::NewCell(i) => Some((i, false)),
                _ => None,
            };
            if let Some((i, keeps)) = store {
                if !keeps && whole[i as usize] {
                    whole[i as usize] = false;
                    narrowed = true;
                }
            }
        }
        if !narrowed {
            break marks;
        }
    };
    // A whole local that the loop uses, and that a path carries in from before the loop, is checked on entry from every
    // entry. A local the loop does not use keeps whatever it holds, so it needs no check: an array that it holds would
    // only send the loop to the generic code.
    let whole_guards: Vec<u32> = (0..proto.nlocals)
        .filter(|&i| {
            whole[i as usize]
                && locals.iter().any(|&(l, _)| l == i)
                && entries.iter().any(|&e| live[i as usize][e])
        })
        .collect();
    // A store of an element leaves the loop at the store when the element is a boolean, so the store is a resume point.
    for ip in header..=last {
        if matches!(code[ip], Op::SetLocal(i) | Op::PopToLocal(i) if !objects[i as usize])
            && marks[ip - header].last() == Some(&Mark::Elem)
            && !resumes.contains(&ip)
        {
            resumes.push(ip);
        }
    }
    // A read of a row by a whole key keeps the last row it read in the frame (see `row_cached`). A loop that stores into the
    // array, or rebinds its local, keeps none, which is stricter than needed: no store replaces a row. Each read has its own
    // cache, so that two reads of one array, one by a key that changes and one by a key that stays, do not evict each other.
    let mut row_caches: Vec<usize> = Vec::new();
    for ip in header..=last {
        if let Op::GetIndex2Local(a) = code[ip] {
            let replaced = (header..=last).any(|j| {
                matches!(code[j], Op::SetIndexLocal(b) | Op::SetLocal(b) | Op::PopToLocal(b) if b == a)
            });
            if depth[ip] >= 0 && !replaced && row_caches.len() < MAX_ROW_CACHES {
                row_caches.push(ip);
            }
        }
    }
    // A property that the loop reads or writes through an object local keeps the address of its value, so that a later
    // access to it skips the lookup (see `prop_address`). Accesses to one local and name share a cache.
    let mut keyed_at: Vec<(usize, (u32, u32))> = Vec::new();
    for ip in header..=last {
        let stack = &marks[ip - header];
        let keyed = match code[ip] {
            Op::GetLocalProp(s, n) if objects[s as usize] => Some((s, n)),
            Op::GetProp(n) => match stack.last() {
                Some(&Mark::View(s)) => Some((s, n)),
                _ => None,
            },
            Op::SetProp(n) => match stack.len().checked_sub(2).and_then(|k| stack.get(k)) {
                Some(&Mark::View(s)) => Some((s, n)),
                _ => None,
            },
            _ => None,
        };
        if let Some(key) = keyed {
            keyed_at.push((ip, key));
        }
    }
    // The loop that encloses `ip` most closely: the latest-starting one that contains it, and the shortest of those.
    let innermost = |ip: usize| {
        let mut best = (header, last);
        for q in header..=last {
            if depth[q] < 0 {
                continue;
            }
            for (t, _) in successors(code[q], q, depth[q]) {
                if t <= ip && ip <= q && (t > best.0 || (t == best.0 && q < best.1)) {
                    best = (t, q);
                }
            }
        }
        best
    };
    // An access whose local is rebound in its innermost loop, where it is the only access to the key, reads an address
    // that the rebinding clears before the next iteration: the cache would never hit, so the key keeps none.
    let useless = |ip: usize, key: (u32, u32)| {
        let (t, q) = innermost(ip);
        let rebound = (t..=q).any(|k| {
            depth[k] >= 0 && matches!(code[k], Op::PopToLocal(i) | Op::SetLocal(i) if i == key.0)
        });
        let once = keyed_at
            .iter()
            .filter(|&&(k, other)| other == key && (t..=q).contains(&k))
            .count()
            == 1;
        rebound && once
    };
    let mut prop_caches: Vec<(u32, u32)> = Vec::new();
    for &(_, key) in &keyed_at {
        if prop_caches.contains(&key) || prop_caches.len() >= MAX_PROP_CACHES {
            continue;
        }
        if keyed_at
            .iter()
            .all(|&(ip, other)| other != key || useless(ip, key))
        {
            continue;
        }
        prop_caches.push(key);
    }
    // A call that runs script code may write any property to any value. Calls of `sqrt` and of `push` are native, and
    // run no script code (see `Region::calls`).
    let calls = (header..=last).any(|ip| match code[ip] {
        Op::Call(argc) => {
            let stack = &marks[ip - header];
            let callee = stack.len().checked_sub(argc as usize + 2).map(|k| stack[k]);
            !matches!(callee, Some(Mark::Sqrt | Mark::Push(_)))
        }
        _ => false,
    });
    Some(Region {
        header,
        last,
        entries,
        exits,
        locals,
        consts,
        guards,
        int_local,
        rem,
        arrays: (0..proto.nlocals).filter(|&i| arrays[i as usize]).collect(),
        row_caches,
        prop_caches,
        calls,
        objects: (0..proto.nlocals)
            .filter(|&i| objects[i as usize])
            .collect(),
        object_guards,
        object_reads,
        math,
        marks,
        whole,
        whole_guards,
        resumes,
        function: None,
        inline: None,
        hoists: Vec::new(),
    })
}

/// Plans a function region for `proto`, whose parameters `types` knows to be numbers. `None` if the
/// function does anything but number work, returns something that is not a number, or calls anything
/// but itself, through one upvalue, with its own number of arguments.
pub(super) fn plan_function(proto: &Proto, depth: &[i32], types: &Numbers) -> Option<Region> {
    let nparams = proto.nparams;
    if proto.has_rest || nparams as usize > MAX_ARGS || depth.first() != Some(&0) {
        return None;
    }
    let code = &proto.code;
    let n = code.len();
    let mut states: Vec<Option<Vec<Mark>>> = vec![None; n];
    states[0] = Some(Vec::new());
    let mut work = vec![0usize];
    let mut facts = FnFacts::default();
    while let Some(ip) = work.pop() {
        let d = depth[ip];
        if d < 0 || d > FN_MAX_DEPTH {
            return None;
        }
        let op = code[ip];
        let before = states[ip].clone()?;
        let after = function_step(proto, ip, op, &before, types, nparams, &mut facts)?;
        for (next, _) in successors(op, ip, d) {
            if next >= n || depth[next] < 0 {
                return None;
            }
            match &states[next] {
                None => {
                    states[next] = Some(after.clone());
                    work.push(next);
                }
                Some(known_state) if *known_state != after => return None,
                Some(_) => {}
            }
        }
    }
    if !method_admitted(proto, depth, &facts) {
        return None;
    }
    // The entry of a closure has no stack budget, which a self call would need, and it returns a number.
    if facts.upvals && (facts.receiver || facts.self_upval.is_some() || facts.ret_undefined) {
        return None;
    }
    let locals = (0..proto.nlocals)
        .map(|i| (i, Home::Mem(frame_local(i))))
        .collect();
    Some(Region {
        header: 0,
        last: n - 1,
        entries: vec![0],
        exits: Vec::new(),
        locals,
        consts: Vec::new(),
        guards: Vec::new(),
        int_local: None,
        rem: None,
        arrays: Vec::new(),
        row_caches: Vec::new(),
        prop_caches: Vec::new(),
        calls: true,
        objects: Vec::new(),
        object_guards: Vec::new(),
        object_reads: Vec::new(),
        math: None,
        // The operands before each instruction, which the emitter reads for the kinds of the operands it takes.
        marks: states.into_iter().map(Option::unwrap_or_default).collect(),
        whole: vec![false; proto.nlocals as usize],
        whole_guards: Vec::new(),
        resumes: Vec::new(),
        function: Some(Function {
            nparams,
            self_upval: facts.self_upval,
            returns_bool: facts.ret_bool.unwrap_or(false),
            receiver: facts.receiver,
            upvals: facts.upvals,
        }),
        inline: None,
        hoists: Vec::new(),
    })
}

/// Whether a function whose instructions `facts` describes can run as a function region, given that the function
/// reads `this`. A function that does not read `this` returns numbers or booleans, as a numeric entry does. A method
/// instead returns `undefined`, and never a number. It must go forward only, so that each instruction runs at most
/// once. And its one write comes after every read, since a read that fails declines the call, which must not come
/// after a write (see `Mark::Receiver`).
fn method_admitted(proto: &Proto, depth: &[i32], facts: &FnFacts) -> bool {
    if !facts.receiver {
        return !facts.ret_undefined;
    }
    if !facts.ret_undefined || facts.ret_bool.is_some() {
        return false;
    }
    let (mut writes, mut reads) = (Vec::new(), Vec::new());
    for (ip, &op) in proto.code.iter().enumerate() {
        if depth[ip] < 0 {
            continue;
        }
        if successors(op, ip, depth[ip])
            .iter()
            .any(|&(next, _)| next <= ip)
        {
            return false;
        }
        match op {
            Op::SetProp(_) => writes.push(ip),
            Op::GetProp(_) => reads.push(ip),
            _ => {}
        }
    }
    match writes.as_slice() {
        [] => true,
        [write] => reads.iter().all(|&read| read < *write),
        _ => false,
    }
}

/// The operands after `op` runs on the operands `before`, or `None` when `op` is not number work.
fn function_step(
    proto: &Proto,
    ip: usize,
    op: Op,
    before: &[Mark],
    types: &Numbers,
    nparams: u32,
    facts: &mut FnFacts,
) -> Option<Vec<Mark>> {
    let FnFacts {
        self_upval,
        ret_bool: ret_kind,
        ret_undefined,
        receiver,
        upvals,
    } = facts;
    let mut s = before.to_vec();
    let top_numbers =
        |k: usize| before.len() >= k && before[before.len() - k..].iter().all(|&m| m == Mark::Num);
    let top_truthy = || matches!(before.last(), Some(Mark::Num | Mark::Bool));
    let number = |k: u32| matches!(proto.consts.get(k as usize), Some(Value::Num(_)));
    let known_at = |i: u32| known(types, ip, i);
    match op {
        Op::Const(k) if number(k) => s.push(Mark::Num),
        Op::Undef => s.push(Mark::This),
        // A read of a captured variable that `this` does not follow is a number, which the entry reads from the closure it
        // is called with (see `emit_function`). A function that calls itself reads its callee the same way, then pushes
        // `this`.
        Op::GetUpval(_) if proto.code.get(ip + 1) != Some(&Op::Undef) => {
            *upvals = true;
            s.push(Mark::Num);
        }
        Op::GetUpval(k) => {
            if *self_upval.get_or_insert(k) != k {
                return None;
            }
            s.push(Mark::Callee);
        }
        Op::Pop => {
            s.pop()?;
        }
        Op::GetLocal(i) if known_at(i) => s.push(Mark::Num),
        Op::SetLocal(_) if top_numbers(1) => {}
        Op::PopToLocal(_) if top_numbers(1) => {
            s.pop();
        }
        Op::AddToLocal(i) if known_at(i) && top_numbers(1) => {
            s.pop();
        }
        Op::IncLocal(i) | Op::DecLocal(i) if known_at(i) => {}
        Op::AddLK(i, k) | Op::SubLK(i, k) | Op::MulLK(i, k) | Op::ModLK(i, k)
            if known_at(i) && number(k) =>
        {
            s.push(Mark::Num)
        }
        Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod if top_numbers(2) => {
            s.truncate(s.len() - 1);
        }
        Op::Neg if top_numbers(1) => {}
        Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::StrictEq | Op::StrictNe
            if top_numbers(2) =>
        {
            s.truncate(s.len() - 1);
            *s.last_mut()? = Mark::Bool;
        }
        Op::Not if top_truthy() => *s.last_mut()? = Mark::Bool,
        Op::Dup if top_numbers(1) => s.push(Mark::Num),
        Op::Swap if top_numbers(2) => {}
        Op::Jump(_) => {}
        Op::JumpIfFalse(_) | Op::JumpIfTrue(_) if top_truthy() => {
            s.pop();
        }
        Op::JumpIfNotLt(_)
        | Op::JumpIfNotLe(_)
        | Op::JumpIfNotGt(_)
        | Op::JumpIfNotGe(_)
        | Op::JumpIfStrictEq(_)
        | Op::JumpIfNotStrictEq(_)
            if top_numbers(2) =>
        {
            s.truncate(s.len() - 2);
        }
        Op::JumpIfLtLK(i, k, _) | Op::JumpIfNotLtLK(i, k, _) if known_at(i) && number(k) => {}
        Op::JumpIfLtLL(p, q, _) | Op::JumpIfNotLtLL(p, q, _) if known_at(p) && known_at(q) => {}
        Op::Return if top_truthy() => {
            // Every return of a function gives the same kind of result.
            let is_bool = before.last() == Some(&Mark::Bool);
            match ret_kind {
                None => *ret_kind = Some(is_bool),
                Some(kind) if *kind != is_bool => return None,
                Some(_) => {}
            }
        }
        Op::Call(argc) => {
            let argc = argc as usize;
            let len = s.len();
            if argc != nparams as usize || len < argc + 2 {
                return None;
            }
            let (callee, this) = (s[len - argc - 2], s[len - argc - 1]);
            if callee != Mark::Callee || this != Mark::This || !top_numbers(argc) {
                return None;
            }
            s.truncate(len - argc - 2);
            s.push(Mark::Num);
        }
        // The `this` of a method, which the body reads and writes properties of (see `Mark::Receiver`).
        Op::This if !proto.is_arrow => {
            *receiver = true;
            s.push(Mark::Receiver);
        }
        Op::Dup if before.last() == Some(&Mark::Receiver) => s.push(Mark::Receiver),
        Op::GetProp(_) if before.last() == Some(&Mark::Receiver) => *s.last_mut()? = Mark::Num,
        Op::SetProp(_)
            if before.len() >= 2
                && before[before.len() - 2] == Mark::Receiver
                && before.last() == Some(&Mark::Num) =>
        {
            s.truncate(s.len() - 2);
            s.push(Mark::Num);
        }
        Op::Return if before.last() == Some(&Mark::This) => *ret_undefined = true,
        _ => return None,
    }
    Some(s)
}

/// Whether the number the test at `ip` leaves is consumed by the jump after it. A `&&` or `||` tests its operand the
/// same way, and keeps it as the value of the expression when it jumps (see `operand_marks`).
fn consumed(code: &[Op], ip: usize) -> bool {
    matches!(
        code.get(ip + 1),
        Some(Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::AndJump(_) | Op::OrJump(_))
    )
}

/// Whether local `i` is known to hold a number before instruction `ip`.
fn known(types: &Numbers, ip: usize, i: u32) -> bool {
    types
        .get(ip)
        .and_then(|s| s.get(i as usize))
        .copied()
        .unwrap_or(false)
}

/// For each local, the instructions from which a read of it may come before any store to it.
fn live_locals(code: &[Op], depth: &[i32], nlocals: usize) -> Vec<Vec<bool>> {
    let n = code.len();
    let mut live = vec![vec![false; n]; nlocals];
    for (i, row) in live.iter_mut().enumerate() {
        let mut changed = true;
        while changed {
            changed = false;
            for ip in (0..n).rev() {
                if depth[ip] < 0 || row[ip] {
                    continue;
                }
                let op = code[ip];
                let stores = matches!(op, Op::SetLocal(j) | Op::PopToLocal(j) if j as usize == i);
                let now = reads_of(op).contains(&(i as u32))
                    || (!stores
                        && successors(op, ip, depth[ip])
                            .iter()
                            .any(|&(next, _)| next < n && row[next]));
                if now {
                    row[ip] = true;
                    changed = true;
                }
            }
        }
    }
    live
}

/// Whether local `i` can reach a read inside the loop, or an exit where it is still live, with the value it
/// had on entry: some path from an entry gets there without a store to `i`.
fn carries_entry_value(
    code: &[Op],
    depth: &[i32],
    entries: &[usize],
    inside: &dyn Fn(usize) -> bool,
    live: &[Vec<bool>],
    i: u32,
) -> bool {
    let mut seen = vec![false; code.len()];
    let mut todo = entries.to_vec();
    while let Some(ip) = todo.pop() {
        if !inside(ip) {
            if live[i as usize].get(ip).copied().unwrap_or(false) {
                return true;
            }
            continue;
        }
        if std::mem::replace(&mut seen[ip], true) || depth[ip] < 0 {
            continue;
        }
        let op = code[ip];
        if reads_of(op).contains(&i) {
            return true;
        }
        if matches!(op, Op::SetLocal(j) | Op::PopToLocal(j) if j == i) {
            continue;
        }
        todo.extend(
            successors(op, ip, depth[ip])
                .into_iter()
                .map(|(next, _)| next),
        );
    }
    false
}

/// Whether a loop region can run `op`: number work, plain locals and jumps.
fn allowed(proto: &Proto, op: Op) -> bool {
    let number = |k: u32| matches!(proto.consts.get(k as usize), Some(Value::Num(_)));
    match op {
        Op::Const(k) | Op::AddLK(_, k) | Op::SubLK(_, k) | Op::MulLK(_, k) | Op::ModLK(_, k) => {
            number(k)
        }
        Op::JumpIfLtLK(_, k, _) | Op::JumpIfNotLtLK(_, k, _) => number(k),
        Op::Pop | Op::Dup | Op::Swap | Op::GetLocal(_) | Op::SetLocal(_) | Op::PopToLocal(_) => {
            true
        }
        Op::AddToLocal(_) | Op::IncLocal(_) | Op::DecLocal(_) => true,
        Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Neg => true,
        Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge
        | Op::Eq
        | Op::Ne
        | Op::StrictEq
        | Op::StrictNe
        | Op::Not => true,
        Op::Jump(_) | Op::JumpIfFalse(_) | Op::JumpIfTrue(_) => true,
        Op::JumpIfNotLt(_) | Op::JumpIfNotLe(_) | Op::JumpIfNotGt(_) | Op::JumpIfNotGe(_) => true,
        Op::JumpIfStrictEq(_) | Op::JumpIfNotStrictEq(_) => true,
        Op::JumpIfLtLL(..) | Op::JumpIfNotLtLL(..) => true,
        Op::True | Op::False | Op::AndJump(_) | Op::OrJump(_) => true,
        // The element code needs the layout of arrays, which the generic code needs too (see `arr_layout`).
        Op::GetIndexLocal(_)
        | Op::GetIndex2Local(_)
        | Op::SetIndexLocal(_)
        | Op::SetIndex2Local(_) => arr_layout().is_some(),
        // A `push` is the probe of the method and the call that takes its receiver (see `step`).
        Op::GetLocalProp(_, n) if is_push(proto, n) || is_length(proto, n) => {
            arr_layout().is_some()
        }
        Op::GetLocalProp(..) | Op::GetProp(_) | Op::SetProp(_) => prop_layout().is_some(),
        Op::GetGlobal(_) => entry_layout().is_some(),
        Op::Call(1) => arr_layout().is_some(),
        _ => false,
    }
}

/// Where the planner's knowledge of a loop's locals and element reads goes (see `step`).
struct Kinds<'a> {
    /// The locals that hold arrays (see `plan`).
    arrays: &'a [bool],
    /// The locals that hold objects (see `plan`).
    objects: &'a [bool],
    /// The instructions that read an element into an object local (see `Region::object_reads`).
    object_reads: &'a [usize],
    /// The name index of the global `Math`, when the loop reads `Math.sqrt` (see `Region::math`).
    math_global: Option<u32>,
    /// The name index of `sqrt`, when the loop reads it through `Math`.
    math_sqrt: Option<u32>,
    /// The locals that hold whole numbers (see `plan`).
    whole: &'a [bool],
}

/// Whether `name` is the property `length`, which an array answers by its length (see `Op::GetLocalProp`).
fn is_length(proto: &Proto, name: u32) -> bool {
    &*proto.names[name as usize] == "length"
}

/// Whether `name` is `sqrt`, which a loop reads from an object and calls (see `Mark::Sqrt`).
fn is_sqrt(proto: &Proto, name: u32) -> bool {
    &*proto.names[name as usize] == "sqrt"
}

/// The global `Math` and its `sqrt`, as name indices, when the loop reads `Math.sqrt`. `Some(None)` when the loop does not
/// read `Math`, and `None` when it reads `Math` without `sqrt`, or writes `sqrt`, which would change the built-in that the
/// entry checks.
fn math_reads(
    proto: &Proto,
    depth: &[i32],
    header: usize,
    last: usize,
) -> Option<Option<(u32, u32)>> {
    let (mut global, mut sqrt, mut written) = (None, None, false);
    for (&d, &op) in depth[header..=last].iter().zip(&proto.code[header..=last]) {
        if d < 0 {
            continue;
        }
        match op {
            Op::GetGlobal(k) if &*proto.names[k as usize] == "Math" => {
                if global.is_some_and(|g| g != k) {
                    return None;
                }
                global = Some(k);
            }
            Op::GetProp(n) if is_sqrt(proto, n) => {
                if sqrt.is_some_and(|s| s != n) {
                    return None;
                }
                sqrt = Some(n);
            }
            Op::SetProp(n) if is_sqrt(proto, n) => written = true,
            _ => {}
        }
    }
    match (global, sqrt) {
        (None, _) => Some(None),
        (Some(k), Some(n)) if !written => Some(Some((k, n))),
        _ => None,
    }
}

/// Whether an operand of this kind has a register (see `Mark`).
fn has_register(m: &Mark) -> bool {
    matches!(m, Mark::Num | Mark::Int | Mark::Bool | Mark::Elem)
}

/// Whether constant `k` is a whole number, which its register holds as an integral value.
fn whole_const(proto: &Proto, k: u32) -> bool {
    let c = number(proto, k);
    c.is_finite() && c.fract() == 0.0
}

/// The address of the built-in `sqrt`, which a loop compares a property with (see `Mark::Sqrt`).
fn sqrt_address() -> u64 {
    crate::builtins::global::sqrt_def() as *const _ as usize as u64
}

/// The object local that `op` reads a property of, when it is a property other than `push` and `length`.
fn object_base(proto: &Proto, op: Op) -> Option<u32> {
    match op {
        Op::GetLocalProp(s, n) if !is_push(proto, n) && !is_length(proto, n) => Some(s),
        _ => None,
    }
}

/// Whether a path inside the loop, from `start`, reads object local `i` before it writes it.
fn reads_before_write(
    proto: &Proto,
    depth: &[i32],
    header: usize,
    last: usize,
    start: usize,
    i: u32,
) -> bool {
    let code = &proto.code;
    let mut seen = vec![false; code.len()];
    let mut work = vec![start];
    while let Some(ip) = work.pop() {
        if ip < header || ip > last || seen[ip] || depth[ip] < 0 {
            continue;
        }
        seen[ip] = true;
        let op = code[ip];
        if matches!(op, Op::GetLocal(j) if j == i) || object_base(proto, op) == Some(i) {
            return true;
        }
        if matches!(op, Op::SetLocal(j) | Op::PopToLocal(j) if j == i) {
            continue;
        }
        work.extend(
            successors(op, ip, depth[ip])
                .into_iter()
                .map(|(next, _)| next),
        );
    }
    false
}

/// The array local that an index operation reads or writes, if `op` is one.
fn index_base(op: Op) -> Option<u32> {
    match op {
        Op::GetIndexLocal(a)
        | Op::GetIndex2Local(a)
        | Op::SetIndexLocal(a)
        | Op::SetIndex2Local(a) => Some(a),
        _ => None,
    }
}

/// Whether `name` is the property `push`, which a loop region runs on an array (see `Op::GetLocalProp`).
fn is_push(proto: &Proto, name: u32) -> bool {
    &*proto.names[name as usize] == "push"
}

/// The array local that the probe of a `push` reads, if `op` is one.
fn push_base(proto: &Proto, op: Op) -> Option<u32> {
    match op {
        Op::GetLocalProp(s, n) if is_push(proto, n) => Some(s),
        _ => None,
    }
}

/// The local that a `length` read names, which is an array when the loop reads its length in place (see `plan`).
fn length_base(proto: &Proto, op: Op) -> Option<u32> {
    match op {
        Op::GetLocalProp(s, n) if is_length(proto, n) => Some(s),
        _ => None,
    }
}

/// Whether a kind is a `push` or its receiver, which only the call of the `push` takes (see `step`), or a method, which
/// only its call takes too.
fn is_receiver(m: &Mark) -> bool {
    matches!(m, Mark::Push(_) | Mark::Array(_) | Mark::Method(..))
}

/// Whether the read at `ip` is the callee of a method call: a property of an object local, which the next instruction
/// reads as the receiver, as in `local.name(...)`. The call looks the property up (see `Mark::Method`).
fn method_read(proto: &Proto, objects: &[bool], ip: usize, op: Op) -> bool {
    match op {
        Op::GetLocalProp(s, n) => {
            objects[s as usize]
                && !is_push(proto, n)
                && !is_length(proto, n)
                && proto.code.get(ip + 1) == Some(&Op::GetLocal(s))
        }
        _ => false,
    }
}

/// Pops `n` operand kinds from the stack of kinds, and returns them from the top down.
fn pop_kinds(state: &mut Vec<Mark>, n: usize) -> Option<Vec<Mark>> {
    let at = state.len().checked_sub(n)?;
    if state[at..].iter().any(is_receiver) {
        return None;
    }
    Some(state.split_off(at).into_iter().rev().collect())
}

/// Steps the kinds of the operands through `op`. `false` when an operand has a kind that `op` cannot take, or a local
/// is used in a way that an array does not allow. A boolean and an element read as the number 0 or 1, so arithmetic
/// and the ordering comparisons take them. Strict equality, a local and an index take numbers only.
fn step(proto: &Proto, ip: usize, op: Op, state: &mut Vec<Mark>, kinds: &Kinds) -> bool {
    let (arrays, objects) = (kinds.arrays, kinds.objects);
    let plain = |i: u32| !arrays[i as usize];
    let number = |m: &Mark| matches!(m, Mark::Num | Mark::Int);
    // A side exit resumes the generic code with the operands of the instruction on the stack, so none of them may be a
    // receiver, which has no register to write back.
    let side_exit = matches!(
        op,
        Op::GetIndexLocal(_) | Op::GetIndex2Local(_) | Op::SetIndexLocal(_) | Op::SetIndex2Local(_)
    ) || push_base(proto, op).is_some()
        || object_base(proto, op).is_some()
        || matches!(op, Op::GetProp(_) | Op::SetProp(_) | Op::GetGlobal(_))
        || matches!(op, Op::PopToLocal(i) if objects[i as usize])
        || (matches!(op, Op::SetLocal(_) | Op::PopToLocal(_)) && state.last() == Some(&Mark::Elem));
    // The read of `sqrt` from `Math` needs no side exit, since the entry checks that it is the built-in
    // (see `Region::math`).
    let math_read =
        matches!(op, Op::GetProp(n) if is_sqrt(proto, n)) && state.last() == Some(&Mark::Math);
    // The built-in `Math` has no slot to write back, so a side exit never has it on the stack.
    if side_exit && !math_read && state.iter().any(|m| is_receiver(m) || *m == Mark::Math) {
        return false;
    }
    // A write to a local that a view of it is on the stack would change what the view reads.
    if let Op::SetLocal(i) | Op::PopToLocal(i) = op {
        if state.contains(&Mark::View(i)) {
            return false;
        }
    }
    match op {
        Op::Const(k) => state.push(if whole_const(proto, k) {
            Mark::Int
        } else {
            Mark::Num
        }),
        Op::True | Op::False => state.push(Mark::Bool),
        Op::GetGlobal(k) if kinds.math_global == Some(k) => state.push(Mark::Math),
        Op::GetGlobal(_) => state.push(Mark::Owned),
        Op::Pop => {
            if pop_kinds(state, 1).is_none() {
                return false;
            }
        }
        Op::Dup => match state.last().copied() {
            Some(top) if !is_receiver(&top) => state.push(top),
            _ => return false,
        },
        Op::Swap => {
            let n = state.len();
            if n < 2 {
                return false;
            }
            // Operands with registers trade them; operands without one trade their slots (see `Op::Swap`).
            if has_register(&state[n - 1]) != has_register(&state[n - 2]) {
                return false;
            }
            state.swap(n - 1, n - 2);
        }
        Op::GetLocalProp(s, n) if is_push(proto, n) => {
            if !arrays[s as usize] {
                return false;
            }
            state.push(Mark::Push(s));
        }
        Op::GetLocalProp(s, n) if is_length(proto, n) => {
            if !arrays[s as usize] {
                return false;
            }
            state.push(Mark::Num);
        }
        Op::GetLocalProp(s, n) if method_read(proto, objects, ip, op) => {
            state.push(Mark::Method(s, n, ip as u32))
        }
        Op::GetLocalProp(s, n) if objects[s as usize] && !is_push(proto, n) => {
            state.push(Mark::Num)
        }
        Op::GetProp(n) if is_sqrt(proto, n) && state.last() == Some(&Mark::Math) => {
            state.pop();
            state.push(Mark::Sqrt);
        }
        Op::GetProp(n) if is_sqrt(proto, n) && state.last() == Some(&Mark::Owned) => {
            state.pop();
            state.push(Mark::Sqrt);
        }
        Op::GetProp(_) => {
            if !matches!(state.last(), Some(Mark::View(_))) {
                return false;
            }
            state.pop();
            state.push(Mark::Num);
        }
        // The loop reads `sqrt` through `Math` as the built-in, so it must not write one.
        Op::SetProp(n) if kinds.math_sqrt.is_some() && is_sqrt(proto, n) => return false,
        Op::SetProp(_) => {
            let n = state.len();
            if n < 2
                || !matches!(state[n - 1], Mark::Num | Mark::Int)
                || !matches!(state[n - 2], Mark::View(_))
            {
                return false;
            }
            state.truncate(n - 2);
            state.push(Mark::Num);
        }
        Op::PopToLocal(i) if objects[i as usize] => match state.last() {
            Some(Mark::View(_) | Mark::Owned) => {
                state.pop();
            }
            _ => return false,
        },
        Op::GetLocal(i) if objects[i as usize] => state.push(Mark::View(i)),
        Op::GetLocal(i) if arrays[i as usize] => state.push(Mark::Array(i)),
        Op::GetLocal(i) => {
            if !plain(i) {
                return false;
            }
            state.push(if kinds.whole[i as usize] {
                Mark::Int
            } else {
                Mark::Num
            });
        }
        // A method call on the object that the callee was read from. The call looks the callee up, and a side exit
        // resumes at the read, so no operand below the callee may need a side exit.
        Op::Call(1)
            if matches!(
                state.as_slice(),
                [.., Mark::Method(s, _, _), Mark::View(v), Mark::Num | Mark::Int] if s == v
            ) =>
        {
            let n = state.len();
            if state[..n - 3]
                .iter()
                .any(|m| is_receiver(m) || *m == Mark::Math)
            {
                return false;
            }
            state.truncate(n - 3);
            state.push(Mark::This);
        }
        // `sqrt` of a number, called on an object: the callee is the built-in, the object is released, and the result is a
        // number.
        Op::Call(1)
            if state.len() >= 3
                && state[state.len() - 3] == Mark::Sqrt
                && matches!(
                    state[state.len() - 2],
                    Mark::View(_) | Mark::Owned | Mark::Math
                )
                && matches!(state[state.len() - 1], Mark::Num | Mark::Int) =>
        {
            let n = state.len();
            state.truncate(n - 3);
            state.push(Mark::Num);
        }
        // The `push` of an array, whose value is a number or a boolean, with the array as its receiver. The result is
        // the new length.
        Op::Call(1) => {
            let n = state.len();
            let pushes = n >= 3
                && matches!(
                    (state[n - 3], state[n - 2], state[n - 1]),
                    (Mark::Push(s), Mark::Array(a), Mark::Num | Mark::Int | Mark::Bool) if s == a
                );
            if !pushes {
                return false;
            }
            state.truncate(n - 3);
            state.push(Mark::Num);
        }
        Op::SetLocal(i) | Op::PopToLocal(i) => match pop_kinds(state, 1) {
            Some(v) if plain(i) && number(&v[0]) => {}
            // An element is a number unless it is a boolean, and a store of one leaves the loop when it is a boolean (see
            // `Emitter::op`), so the local only ever takes a number.
            Some(v) if plain(i) && v[0] == Mark::Elem => {}
            _ => return false,
        },
        Op::AddToLocal(i) => match pop_kinds(state, 1) {
            Some(_) if plain(i) => {}
            _ => return false,
        },
        Op::IncLocal(i)
        | Op::DecLocal(i)
        | Op::JumpIfLtLK(i, _, _)
        | Op::JumpIfNotLtLK(i, _, _) => {
            if !plain(i) {
                return false;
            }
        }
        Op::AddLK(i, k) | Op::SubLK(i, k) | Op::MulLK(i, k) | Op::ModLK(i, k) => {
            if !plain(i) {
                return false;
            }
            let whole = kinds.whole[i as usize] && whole_const(proto, k);
            state.push(if whole { Mark::Int } else { Mark::Num });
        }
        Op::JumpIfLtLL(p, q, _) | Op::JumpIfNotLtLL(p, q, _) => {
            if !plain(p) || !plain(q) {
                return false;
            }
        }
        Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod => {
            let Some(operands) = pop_kinds(state, 2) else {
                return false;
            };
            // A sum, difference, product or remainder of whole numbers is whole. A quotient is not.
            let whole = op != Op::Div && operands.iter().all(|m| *m == Mark::Int);
            state.push(if whole { Mark::Int } else { Mark::Num });
        }
        Op::Neg => {
            let Some(operand) = pop_kinds(state, 1) else {
                return false;
            };
            state.push(if operand[0] == Mark::Int {
                Mark::Int
            } else {
                Mark::Num
            });
        }
        Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::Not => {
            let pops = if matches!(op, Op::Not) { 1 } else { 2 };
            if pop_kinds(state, pops).is_none() {
                return false;
            }
            state.push(Mark::Bool);
        }
        Op::StrictEq | Op::StrictNe => match pop_kinds(state, 2) {
            Some(kinds) if kinds.iter().all(number) => state.push(Mark::Bool),
            _ => return false,
        },
        Op::Jump(_) => {}
        Op::JumpIfFalse(_) | Op::JumpIfTrue(_) | Op::AndJump(_) | Op::OrJump(_) => {
            if pop_kinds(state, 1).is_none() {
                return false;
            }
        }
        Op::JumpIfNotLt(_) | Op::JumpIfNotLe(_) | Op::JumpIfNotGt(_) | Op::JumpIfNotGe(_) => {
            if pop_kinds(state, 2).is_none() {
                return false;
            }
        }
        // Only numbers are compared, since a boolean is held as 0.0 or 1.0 and would equal the number 1.
        Op::JumpIfStrictEq(_) | Op::JumpIfNotStrictEq(_) => match pop_kinds(state, 2) {
            Some(kinds) if kinds.iter().all(number) => {}
            _ => return false,
        },
        Op::GetIndexLocal(_) if kinds.object_reads.contains(&ip) => match state.last() {
            Some(Mark::Num | Mark::Int) => {
                state.pop();
                state.push(Mark::Owned);
            }
            _ => return false,
        },
        Op::GetIndexLocal(a) | Op::GetIndex2Local(a) => {
            let keys = if matches!(op, Op::GetIndex2Local(_)) {
                2
            } else {
                1
            };
            match pop_kinds(state, keys) {
                Some(index) if !plain(a) && index.iter().all(number) => state.push(Mark::Elem),
                _ => return false,
            }
        }
        Op::SetIndexLocal(a) | Op::SetIndex2Local(a) => {
            let keys = if matches!(op, Op::SetIndex2Local(_)) {
                2
            } else {
                1
            };
            match pop_kinds(state, keys + 1) {
                // The value is on top, and the operation leaves it on the stack.
                Some(popped)
                    if !plain(a)
                        && popped[1..].iter().all(number)
                        && matches!(popped[0], Mark::Num | Mark::Int | Mark::Bool) =>
                {
                    state.push(popped[0]);
                }
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

/// The kinds of the operands before each instruction of a loop, indexed from `header`, found by following every path
/// from the entries. `None` when an instruction uses an operand in a way its kind does not allow, or when two paths
/// reach an instruction with different kinds.
fn operand_marks(
    proto: &Proto,
    depth: &[i32],
    header: usize,
    last: usize,
    entries: &[usize],
    kinds: &Kinds,
) -> Option<Vec<Vec<Mark>>> {
    let code = &proto.code;
    let inside = |ip: usize| (header..=last).contains(&ip);
    let mut before: Vec<Option<Vec<Mark>>> = vec![None; last - header + 1];
    let mut work = Vec::new();
    for &e in entries {
        before[e - header] = Some(Vec::new());
        work.push(e);
    }
    while let Some(ip) = work.pop() {
        let entered = before[ip - header].clone()?;
        let mut state = entered.clone();
        if !step(proto, ip, code[ip], &mut state, kinds) {
            return None;
        }
        for (next, next_depth) in successors(code[ip], ip, depth[ip]) {
            if !inside(next) {
                continue;
            }
            // A taken `&&` or `||` keeps the operand it tested, which its successor's depth shows.
            let incoming =
                if next_depth == depth[ip] && matches!(code[ip], Op::AndJump(_) | Op::OrJump(_)) {
                    &entered
                } else {
                    &state
                };
            match &before[next - header] {
                None => {
                    before[next - header] = Some(incoming.clone());
                    work.push(next);
                }
                Some(known) if known != incoming => return None,
                Some(_) => {}
            }
        }
    }
    // An instruction that no path reaches never runs, so its kinds are only a placeholder.
    Some(
        before
            .into_iter()
            .enumerate()
            .map(|(k, kinds)| {
                kinds.unwrap_or_else(|| vec![Mark::Num; depth[header + k].max(0) as usize])
            })
            .collect(),
    )
}

/// The number an instruction uses as an operand, when it has one that is kept in a register.
fn constant_of(proto: &Proto, op: Op) -> Option<u64> {
    let k = match op {
        Op::IncLocal(_) | Op::DecLocal(_) => return Some(1f64.to_bits()),
        Op::True => return Some(1f64.to_bits()),
        Op::False => return Some(0f64.to_bits()),
        Op::Const(k) | Op::AddLK(_, k) | Op::SubLK(_, k) | Op::MulLK(_, k) => k,
        Op::JumpIfLtLK(_, k, _) | Op::JumpIfNotLtLK(_, k, _) => k,
        _ => return None,
    };
    Some(number(proto, k).to_bits())
}

/// The comparisons and `!`, which leave a boolean on the stack.
fn is_test(op: Op) -> bool {
    matches!(
        op,
        Op::Lt | Op::Le | Op::Gt | Op::Ge | Op::Eq | Op::Ne | Op::StrictEq | Op::StrictNe | Op::Not
    )
}

/// The locals an instruction reads, which must hold numbers when it runs.
fn reads_of(op: Op) -> Vec<u32> {
    match op {
        Op::GetLocal(i) | Op::AddToLocal(i) | Op::IncLocal(i) | Op::DecLocal(i) => vec![i],
        Op::AddLK(s, _) | Op::SubLK(s, _) | Op::MulLK(s, _) | Op::ModLK(s, _) => vec![s],
        Op::JumpIfLtLK(s, _, _) | Op::JumpIfNotLtLK(s, _, _) => vec![s],
        Op::JumpIfLtLL(p, q, _) | Op::JumpIfNotLtLL(p, q, _) => vec![p, q],
        _ => Vec::new(),
    }
}

/// The number of a numeric constant. The planners admit only numeric constants, so the fallback is
/// never used.
fn number(proto: &Proto, k: u32) -> f64 {
    match proto.consts.get(k as usize) {
        Some(Value::Num(x)) => *x,
        _ => f64::NAN,
    }
}

/// The payload slot of local `i` in a loop's frame, which `r12` points at.
fn local_mem(i: u32) -> Mem {
    Mem {
        base: R12,
        disp: 16 * i as i32,
    }
}

/// The slot of local `i` in a function's native frame, which `rbp` points at.
fn frame_local(i: u32) -> Mem {
    Mem {
        base: RBP,
        disp: -8 * (i as i32 + 1),
    }
}

/// The address of the callee's value that a closure's entry is called with, in its native frame, below the receiver's copy.
fn callee_mem(nlocals: i32) -> Mem {
    Mem {
        base: RBP,
        disp: -8 * (nlocals + 4),
    }
}

/// The copy of a method's receiver in its native frame, below the locals: the tag of the value, and then its payload.
fn receiver_mem(nlocals: i32) -> Mem {
    Mem {
        base: RBP,
        disp: -8 * (nlocals + 3),
    }
}

/// The stack slot that a call of `js_mod` saves register `k` in.
fn js_spill(k: usize) -> Mem {
    Mem {
        base: RSP,
        disp: 8 * k as i32,
    }
}

/// The frame slot at `offset` (0 for the key, 8 for the row) of the row cache of the loop's array `j` (see `row_cached`).
fn row_cache_mem(j: usize, offset: i32) -> Mem {
    Mem {
        base: RSP,
        disp: ROW_CACHE_BASE + 16 * j as i32 + offset,
    }
}

/// The frame slot of the address that the property cache `j` of a loop holds (see `prop_address`).
fn prop_cache_mem(j: usize) -> Mem {
    Mem {
        base: RSP,
        disp: PROP_CACHE_BASE + 8 * j as i32,
    }
}

/// The frame slot of the count of structural changes that the property caches of a loop were last checked at (see
/// `check_structure`).
fn prop_epoch_mem() -> Mem {
    Mem {
        base: RSP,
        disp: PROP_CACHE_BASE + 8 * MAX_PROP_CACHES as i32,
    }
}

/// The object local that a mark views, if it views one (see `Mark::View`).
fn view_local(m: Mark) -> Option<u32> {
    match m {
        Mark::View(s) => Some(s),
        _ => None,
    }
}

/// Emits a loop region. `entry` is the label the generic code calls, with rdx set to the instruction
/// it reached the loop at. The region returns the index of the instruction it left at, or
/// `NOT_ENTERED` when a guard failed.
pub(super) fn emit(a: &mut Asm, lp: &Region, entry: Label, proto: &Proto, depth: &[i32]) {
    let at: Vec<Label> = (lp.header..=lp.last).map(|_| a.label()).collect();
    let exits: Vec<Label> = lp.exits.iter().map(|_| a.label()).collect();
    let not_entered = a.label();
    let mut e = Emitter {
        a,
        proto,
        depth,
        lp,
        at,
        exits,
        not_entered,
        slow: Vec::new(),
        body: None,
        nlocals: 0,
        sides: Vec::new(),
        prop_cache: None,
        inline: None,
        pending: [None; PENDING],
        jumped_to: jump_targets(proto),
    };
    e.a.bind(entry);
    e.a.push_reg(R13);
    e.a.push_reg(R15);
    // r14 is saved with the others. The frame is 8 bytes shorter, so the locals keep their places and the stack stays
    // aligned for the calls.
    e.a.push_reg(R14);
    e.a.sub_rsp32(FRAME - 8);
    for &i in &lp.guards {
        e.a.cmp8_imm(local_mem(i), TAG_NUM);
        e.a.jcc(CC_NE, not_entered);
    }
    // A whole local holds an integral number on entry, or a value that the uses of whole numbers refuse, which truncates to
    // the most negative int: one out of the 32-bit range, or NaN. Any other value leaves the loop to the generic code.
    for &i in &lp.whole_guards {
        e.a.cmp8_imm(local_mem(i), TAG_NUM);
        e.a.jcc(CC_NE, not_entered);
        e.a.movsd_load(0, local_mem(i).at(8));
        let checked = e.a.label();
        e.a.cvttsd2si(RAX, 0);
        e.a.cmp32_imm32(RAX, i32::MIN);
        e.a.jcc(CC_E, checked);
        e.a.cvtsi2sd(1, RAX);
        e.a.ucomisd(1, 0);
        e.a.jcc(CC_NE, not_entered);
        e.a.jcc(CC_P, not_entered);
        e.a.bind(checked);
    }
    for &a in &lp.arrays {
        e.a.cmp8_imm(local_mem(a), TAG_ARR);
        e.a.jcc(CC_NE, not_entered);
    }
    for &o in &lp.object_guards {
        e.a.cmp8_imm(local_mem(o), TAG_OBJ);
        e.a.jcc(CC_NE, not_entered);
    }
    if let Some((k, n)) = lp.math {
        // The loop takes `Math.sqrt` to be the built-in, which the entry checks once, since nothing in the loop changes
        // the global or the property (see `math_reads`). The checks clobber rdx, which the dispatch below needs, so r9
        // keeps it.
        e.a.mov_rr(R9, RDX);
        let name = &proto.names[k as usize] as *const std::rc::Rc<str> as usize;
        let hint = &proto.global_hints[k as usize] as *const std::cell::Cell<u32> as usize;
        let layout = entry_layout().expect("the planner admits a global only with a known layout");
        find_global(&mut *e.a, name, hint, layout, not_entered);
        let math = Mem {
            base: RDX,
            disp: layout.value,
        };
        let site = PropSite {
            hint: &proto.hints[n as usize] as *const PropHint as usize,
            layout: prop_layout().expect("the planner admits a property only with a known layout"),
        };
        find_prop(&mut *e.a, math, site, not_entered);
        e.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_NATIVE);
        e.a.jcc(CC_NE, not_entered);
        e.a.mov_load(RCX, Mem { base: RDX, disp: 8 });
        e.a.mov_imm64(RAX, sqrt_address());
        e.a.cmp_rr64(RCX, RAX);
        e.a.jcc(CC_NE, not_entered);
        e.a.mov_rr(RDX, R9);
    }
    for &(i, home) in &lp.locals {
        if let Home::Reg(x) = home {
            e.a.movsd_load(x, local_mem(i).at(8));
        }
    }
    if let Some(i) = lp.int_local {
        // The integer copy is taken when the number is an integer, which is not -0, within range.
        let x = match e.home(i) {
            Home::Reg(x) => x,
            Home::Mem(_) => unreachable!("a loop keeps its locals in registers"),
        };
        let ok = e.a.label();
        e.a.cvttsd2si(RAX, x);
        e.a.movsxd(R13, RAX);
        e.a.cvtsi2sd(0, RAX);
        e.a.ucomisd(0, x);
        e.a.jcc(CC_NE, not_entered);
        e.a.jcc(CC_P, not_entered);
        e.a.test64(R13, R13);
        e.a.jcc(CC_NE, ok);
        e.a.movmskpd(RAX, x);
        e.a.mov_imm32(RCX, 1);
        e.a.test32(RAX, RCX);
        e.a.jcc(CC_NE, not_entered);
        e.a.bind(ok);
        e.a.cmp64_imm32(R13, INT_LIMIT);
        e.a.jcc(CC_G, not_entered);
        e.a.cmp64_imm32(R13, -INT_LIMIT);
        e.a.jcc(CC_L, not_entered);
        if let Some(c) = lp.rem {
            // The remainder is only kept for a copy that is not negative, so it starts from the copy here.
            e.a.test64(R13, R13);
            e.a.jcc(CC_L, not_entered);
            let (m, s) = magic(c);
            e.a.mov_rr(R8, R13);
            e.a.mov_imm64(R10, m);
            e.a.imul64(R8, R10);
            e.a.shr64_imm(R8, s);
            e.a.imul64_imm(R8, R8, c as i32);
            e.a.mov_rr(R15, R13);
            e.a.sub64(R15, R8);
        }
    }
    // Each row cache starts empty, since the rows of its array may have changed since the last entry.
    for j in 0..lp.row_caches.len() {
        e.a.mov_imm64(RAX, NO_KEY);
        e.a.mov_store(row_cache_mem(j, 0), RAX);
    }
    // Each property cache starts empty, since the object of its local may have changed since the last entry.
    for j in 0..lp.prop_caches.len() {
        e.a.mov_imm64(RAX, 0);
        e.a.mov_store(prop_cache_mem(j), RAX);
    }
    if !lp.prop_caches.is_empty() {
        e.epoch_into(RAX);
        e.a.mov_store(prop_epoch_mem(), RAX);
    }
    for &(bits, x) in &lp.consts {
        e.a.mov_imm64(RAX, bits);
        e.a.movq_to_xmm(x, RAX);
    }
    for &t in &lp.entries {
        e.a.cmp64_imm32(RDX, t as i32);
        let target = e.target(t as u32);
        e.a.jcc(CC_E, target);
    }
    e.a.ud2();
    e.body_ops();
    e.leave_loop();
}

/// The entries of a function region: its wrapper, which Rust calls, and the batch entries of the numeric
/// array methods that the function can serve (see `Numeric` in `jit`).
pub(super) struct Entries<T> {
    pub(super) wrapper: T,
    pub(super) map: Option<T>,
    pub(super) filter: Option<T>,
    pub(super) reduce: Option<T>,
}

impl Entries<Label> {
    /// The offsets of the entries in the assembled code.
    pub(super) fn offsets(&self, a: &Asm) -> Entries<usize> {
        Entries {
            wrapper: a.offset(self.wrapper),
            map: self.map.map(|l| a.offset(l)),
            filter: self.filter.map(|l| a.offset(l)),
            reduce: self.reduce.map(|l| a.offset(l)),
        }
    }
}

/// Whether `op` may run between a `GetProp` and the `SetProp` that reuses its property address: it runs straight
/// through, runs no script code and writes no property. The local that holds the object (`view_slot`) must not be
/// written.
fn window_op(op: Op, view_slot: Option<u32>) -> bool {
    match op {
        Op::Const(_)
        | Op::GetLocal(_)
        | Op::Dup
        | Op::Swap
        | Op::Pop
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Neg
        | Op::Mod
        | Op::AddLK(..)
        | Op::SubLK(..)
        | Op::MulLK(..)
        | Op::ModLK(..) => true,
        Op::SetLocal(i)
        | Op::PopToLocal(i)
        | Op::AddToLocal(i)
        | Op::IncLocal(i)
        | Op::DecLocal(i) => view_slot != Some(i),
        _ => false,
    }
}

/// Pushes the frame of an entry of a function region. It saves the registers that the entry's caller
/// expects preserved, and points r13 at the lowest stack pointer that a call may reach. The padding
/// aligns the call to the body.
fn enter_frame(a: &mut Asm) {
    for r in [RBP, R13, RBX, R12, R14, R15] {
        a.push_reg(r);
    }
    a.lea(
        R13,
        Mem {
            base: RSP,
            disp: -STACK_BUDGET,
        },
    );
    a.sub_rsp(8);
}

/// Pops the frame of `enter_frame`, and returns with the status in rax.
fn leave_frame(a: &mut Asm) {
    a.add_rsp(8);
    unwind_frame(a);
}

/// Pops the registers that `enter_frame` saved, and returns. The overflow path first puts rsp where
/// those registers are, which is `STACK_BUDGET` above r13.
fn unwind_frame(a: &mut Asm) {
    for r in [R15, R14, R12, RBX, R13, RBP] {
        a.pop_reg(r);
    }
    a.ret();
}

/// Adds one to the number at `m`.
fn bump(a: &mut Asm, m: Mem) {
    a.mov_load(RAX, m);
    a.add64_imm8(RAX, 1);
    a.mov_store(m, RAX);
}

/// The numeric array method that a batch entry serves.
#[derive(Clone, Copy)]
enum Mode {
    Map,
    Filter,
    Reduce,
}

/// Emits a batch entry: it runs the function on the elements of a batch from its start on, for as long as
/// they are numbers, and keeps the batch's progress in memory as it goes (see `Batch`). It is called as
/// `fn(first: *const Value, out: *mut Value, batch: *mut Batch) -> u64`. The results of a map, and the kept
/// elements of a filter, are written at `out`. The entry returns 0, or 1 when the stack ran out, and in
/// either case the progress is valid up to the element that did not run.
fn emit_batch(a: &mut Asm, body: Label, mode: Mode, nparams: u32, returns_bool: bool) -> Label {
    let entry = a.label();
    let (top, skip, finished) = (a.label(), a.label(), a.label());
    let (start, end, done, kept, acc) = (
        offset_of!(Batch, start) as i32,
        offset_of!(Batch, end) as i32,
        offset_of!(Batch, done) as i32,
        offset_of!(Batch, kept) as i32,
        offset_of!(Batch, acc) as i32,
    );
    // r12 is where the next result goes, rbx the element at the current index, r14 that index, and
    // r15 the batch. None of them is written by the body, which keeps to the other registers.
    let batch = |disp: i32| Mem { base: R15, disp };
    a.bind(entry);
    enter_frame(a);
    a.mov_rr(RBX, RDI);
    a.mov_rr(R12, RSI);
    a.mov_rr(R15, RDX);
    a.mov_load(R14, batch(start));
    a.bind(top);
    a.mov_load(RAX, batch(end));
    a.cmp_rr64(R14, RAX);
    a.jcc(CC_AE, finished);
    a.movzx_load8(RAX, Mem { base: RBX, disp: 0 });
    a.cmp64_imm8(RAX, TAG_NUM);
    a.jcc(CC_NE, finished);
    match mode {
        Mode::Reduce => {
            a.movsd_load(0, batch(acc));
            a.movsd_load(1, Mem { base: RBX, disp: 8 });
            if nparams >= 3 {
                a.cvtsi2sd64(2, R14);
            }
        }
        Mode::Map | Mode::Filter => {
            a.movsd_load(0, Mem { base: RBX, disp: 8 });
            if nparams >= 2 {
                a.cvtsi2sd64(1, R14);
            }
        }
    }
    a.call_label(body);
    match mode {
        Mode::Map if returns_bool => {
            a.mov8_imm(Mem { base: R12, disp: 0 }, TAG_BOOL);
            a.cvttsd2si(RAX, 0);
            a.mov_store8(Mem { base: R12, disp: 1 }, RAX);
            a.add64_imm8(R12, 16);
        }
        Mode::Map => {
            a.mov8_imm(Mem { base: R12, disp: 0 }, TAG_NUM);
            a.movsd_store(Mem { base: R12, disp: 8 }, 0);
            a.add64_imm8(R12, 16);
        }
        Mode::Filter => {
            // The element is kept when the result is true: not zero, and not NaN, which is false.
            // A boolean result is 0 or 1, so the same test serves it.
            a.xorpd(1, 1);
            a.ucomisd(0, 1);
            a.jcc(CC_P, skip);
            a.jcc(CC_E, skip);
            a.mov_load(RAX, Mem { base: RBX, disp: 0 });
            a.mov_load(RCX, Mem { base: RBX, disp: 8 });
            a.mov_store(Mem { base: R12, disp: 0 }, RAX);
            a.mov_store(Mem { base: R12, disp: 8 }, RCX);
            a.add64_imm8(R12, 16);
            bump(a, batch(kept));
        }
        Mode::Reduce => a.movsd_store(batch(acc), 0),
    }
    a.bind(skip);
    a.add64_imm8(RBX, 16);
    a.add64_imm8(R14, 1);
    bump(a, batch(done));
    a.jmp(top);
    a.bind(finished);
    a.mov_imm32(RAX, 0);
    leave_frame(a);
    entry
}

/// Emits a function region, and returns the labels of its entries. Rust calls the wrapper as
/// `fn(args: *const f64) -> Ret`: the result comes back in xmm0, and rax holds 0, or 1 when the native
/// stack ran out. Native calls within the function go to its body, with the arguments in xmm0 and up
/// and the result in xmm0.
pub(super) fn emit_function(
    a: &mut Asm,
    lp: &Region,
    proto: &Proto,
    depth: &[i32],
) -> Entries<Label> {
    let function = lp.function.as_ref().expect("a function region");
    let nlocals = proto.nlocals as i32;
    let frame = (8 * (nlocals + 8) + 104 + 15) / 16 * 16;
    let (wrapper, body, overflow) = (a.label(), a.label(), a.label());
    if function.receiver || function.upvals {
        // A method or a closure needs no wrapper. Nothing it calls can call it back, so it needs neither the stack budget
        // nor the registers that a wrapper saves: its body saves rbp, and nothing else. The caller passes the arguments in
        // rdi, and the receiver, or the callee's value, in rsi.
        a.bind(wrapper);
        a.push_reg(RBP);
        a.mov_rr(RBP, RSP);
        a.sub_rsp32(frame);
        if function.receiver {
            // The receiver's value is copied into the frame, which takes no reference of its own: the caller keeps the
            // receiver alive for the call.
            let receiver = receiver_mem(nlocals);
            a.mov_load(RAX, Mem { base: RSI, disp: 0 });
            a.mov_store(receiver, RAX);
            a.mov_load(RAX, Mem { base: RSI, disp: 8 });
            a.mov_store(receiver.at(8), RAX);
        } else {
            // The callee's address is kept, which the captured numbers are read through.
            a.mov_store(callee_mem(nlocals), RSI);
        }
        for j in 0..function.nparams {
            a.movsd_load(
                j as u8,
                Mem {
                    base: RDI,
                    disp: 8 * j as i32,
                },
            );
            a.movsd_store(frame_local(j), j as u8);
        }
    } else {
        // The wrapper and the batch entries share one frame (see `enter_frame`). The body keeps to the
        // other registers, and restores rbp itself.
        a.bind(wrapper);
        enter_frame(a);
        for j in 0..function.nparams {
            a.movsd_load(
                j as u8,
                Mem {
                    base: RDI,
                    disp: 8 * j as i32,
                },
            );
        }
        a.call_label(body);
        // A method's body gives its status in rax, since it can decline. Any other function returns 0 there.
        if !function.receiver {
            a.mov_imm32(RAX, 0);
        }
        leave_frame(a);
        // A body that finds the stack too low returns through the entry that called it, with its frame
        // still in place, so the entry returns 1.
        a.bind(overflow);
        a.lea(
            RSP,
            Mem {
                base: R13,
                disp: STACK_BUDGET,
            },
        );
        a.mov_imm32(RAX, 1);
        unwind_frame(a);

        a.bind(body);
        a.cmp_rr64(RSP, R13);
        a.jcc(CC_B, overflow);
        a.push_reg(RBP);
        a.mov_rr(RBP, RSP);
        a.sub_rsp32(frame);
        for i in 0..function.nparams {
            a.movsd_store(frame_local(i), i as u8);
        }
    }
    let at: Vec<Label> = (lp.header..=lp.last).map(|_| a.label()).collect();
    let not_entered = a.label();
    let mut e = Emitter {
        a,
        proto,
        depth,
        lp,
        at,
        exits: Vec::new(),
        not_entered,
        slow: Vec::new(),
        body: Some(if function.receiver || function.upvals {
            wrapper
        } else {
            body
        }),
        nlocals,
        sides: Vec::new(),
        prop_cache: None,
        inline: None,
        pending: [None; PENDING],
        jumped_to: jump_targets(proto),
    };
    e.body_ops();
    let a = e.a;
    let (nparams, returns_bool) = (function.nparams, function.returns_bool);
    // The batch entries call the body without a receiver, so a method has none.
    let batches = !(function.receiver || function.upvals);
    let map = if batches && nparams <= 2 {
        Some(emit_batch(a, body, Mode::Map, nparams, returns_bool))
    } else {
        None
    };
    let filter = if batches && nparams <= 2 {
        Some(emit_batch(a, body, Mode::Filter, nparams, returns_bool))
    } else {
        None
    };
    // A reduce that returns booleans would hand a boolean to the next call, so it runs in the interpreter.
    let reduce = if batches && nparams <= 3 && !returns_bool {
        Some(emit_batch(a, body, Mode::Reduce, nparams, false))
    } else {
        None
    };
    Entries {
        wrapper,
        map,
        filter,
        reduce,
    }
}

/// Whether `op` reads its operands where they are and writes nothing but its own result, so that the pending operands
/// stay pending across it (see `Emitter::operand`). Every other operator first copies them into their registers.
fn reads_in_place(op: Op) -> bool {
    matches!(
        op,
        Op::Const(_)
            | Op::True
            | Op::False
            | Op::GetLocal(_)
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Lt
            | Op::Le
            | Op::Gt
            | Op::Ge
            | Op::Eq
            | Op::StrictEq
            | Op::Ne
            | Op::StrictNe
            | Op::AddToLocal(_)
            | Op::GetIndex2Local(_)
            | Op::SetIndexLocal(_)
            | Op::JumpIfFalse(_)
            | Op::JumpIfTrue(_)
            | Op::JumpIfNotLt(_)
            | Op::JumpIfNotLe(_)
            | Op::JumpIfNotGt(_)
            | Op::JumpIfNotGe(_)
            | Op::JumpIfStrictEq(_)
            | Op::JumpIfNotStrictEq(_)
            | Op::JumpIfLtLK(..)
            | Op::JumpIfNotLtLK(..)
            | Op::JumpIfLtLL(..)
            | Op::JumpIfNotLtLL(..)
    )
}

/// Which instructions a jump reaches other than by falling through from the one before. A jump leaves its operands in
/// their registers, so the instruction that it reaches expects them there (see `Emitter::bind_at`).
fn jump_targets(proto: &Proto) -> Vec<bool> {
    let mut targets = vec![false; proto.code.len()];
    for (j, &op) in proto.code.iter().enumerate() {
        for (t, _) in successors(op, j, 0) {
            if j + 1 != t {
                if let Some(target) = targets.get_mut(t) {
                    *target = true;
                }
            }
        }
    }
    targets
}

struct Emitter<'a, 'b> {
    a: &'b mut Asm,
    proto: &'a Proto,
    depth: &'a [i32],
    lp: &'a Region,
    /// The label of each instruction of the region.
    at: Vec<Label>,
    /// The stub that leaves a loop at each exit, in `lp.exits` order.
    exits: Vec<Label>,
    not_entered: Label,
    /// Out-of-line `js_mod` calls for a constant divisor: where each starts, where it rejoins, the
    /// register its result goes to, the register that holds the dividend, and the divisor.
    slow: Vec<(Label, Label, u8, u8, f64)>,
    /// For a function region, the body that calls enter. A loop region has none.
    body: Option<Label>,
    /// The locals of a function region, whose operand spill slots sit below them.
    nlocals: i32,
    /// The side exits of the element operations: the stub, the instruction, the operand depth before it, and the pending
    /// operands that the stub copies into their registers before it spills (see `side_exit`).
    sides: Vec<(Label, usize, i32, Vec<(i32, Src)>)>,
    /// The last `GetProp` that kept its property's address in r14: its instruction, its name, and its object's view.
    prop_cache: Option<(usize, u32, Mark)>,
    /// The body of the method that the loop runs in place, with its own receiver and operands (see `inline_body`).
    inline: Option<InlineCtx>,
    /// The operands that are read where they are, by depth, rather than copied into their registers yet (see
    /// `Emitter::operand`).
    pending: [Option<Src>; PENDING],
    /// Whether a jump reaches each instruction other than by falling through from the one before (see `jump_targets`).
    jumped_to: Vec<bool>,
}

impl Emitter<'_, '_> {
    /// Emits the code of every instruction in the region, then the out-of-line paths.
    fn body_ops(&mut self) {
        let lp = self.lp;
        for ip in lp.header..=lp.last {
            let at = self.at[ip - lp.header];
            // A hoisted statement starts at its own depth, so an operand still pending there is dead: the statement would
            // overwrite it. It is dropped, not written out at the label that a jump into the statement reaches.
            if lp.hoists.iter().any(|h| h.start == ip) {
                let base = self.depth[ip].max(0) as usize;
                self.pending[base..].iter_mut().for_each(|p| *p = None);
            }
            self.bind_at(at, ip);
            // A statement that runs once before an inner loop is emitted at the jump that enters it, after that jump's
            // label, so every way into the loop runs it. The back jump goes to the loop's body, which comes after the
            // jump, so it does not run the statement again (see `with_hoists`).
            for hoist in lp.hoists.iter().filter(|h| h.entry == ip) {
                self.materialize(PENDING as i32);
                for k in hoist.start..=hoist.end {
                    let (op, d) = (self.proto.code[k], self.depth[k]);
                    self.op(op, d, k);
                }
            }
            // The hoisted statements are not emitted where they stand, though their labels are still bound.
            if lp.hoists.iter().any(|h| (h.start..=h.end).contains(&ip)) {
                continue;
            }
            let d = self.depth[ip];
            if d < 0 {
                self.a.ud2();
            } else {
                let op = self.proto.code[ip];
                self.op(op, d, ip);
            }
        }
        // A loop's last instruction may fall through to the one after the loop, which is an exit.
        // The out-of-line paths below must not be reached that way.
        if self.body.is_none() && self.depth[lp.last] >= 0 {
            let (last, next) = (lp.last, lp.last + 1);
            let op = self.proto.code[last];
            if successors(op, last, self.depth[last])
                .iter()
                .any(|&(n, _)| n == next)
            {
                self.materialize(PENDING as i32);
                let target = self.target(next as u32);
                self.a.jmp(target);
            }
        }
        for (entry, join, dst, src, c) in std::mem::take(&mut self.slow) {
            self.a.bind(entry);
            self.a.movapd_rr(0, src);
            self.a.mov_imm64(RCX, c.to_bits());
            self.a.movq_to_xmm(1, RCX);
            self.call_mod(dst);
            self.a.jmp(join);
        }
        for (stub, ip, d, pending) in std::mem::take(&mut self.sides) {
            self.a.bind(stub);
            if self.body.is_some() {
                // A function declines before it writes anything, so the call runs in the interpreter, and the frame is
                // all there is to leave. The wrapper passes the status on.
                self.a.mov_imm32(RAX, 1);
                self.a.mov_rr(RSP, RBP);
                self.a.pop_reg(RBP);
                self.a.ret();
                continue;
            }
            for (k, src) in pending {
                self.load_src(k, src);
            }
            self.spill(ip, d);
            self.writeback();
            self.a.add_rsp32(FRAME - 8);
            self.a.pop_reg(R14);
            self.a.pop_reg(R15);
            self.a.pop_reg(R13);
            self.a.mov_imm32(RAX, ip as u32);
            self.a.ret();
        }
    }

    /// Emits the stubs that leave a loop, and the return for a guard that failed.
    fn leave_loop(&mut self) {
        let lp = self.lp;
        for (k, &t) in lp.exits.iter().enumerate() {
            let stub = self.exits[k];
            self.a.bind(stub);
            self.writeback();
            self.a.add_rsp32(FRAME - 8);
            self.a.pop_reg(R14);
            self.a.pop_reg(R15);
            self.a.pop_reg(R13);
            self.a.mov_imm32(RAX, t as u32);
            self.a.ret();
        }
        let not_entered = self.not_entered;
        self.a.bind(not_entered);
        self.a.add_rsp32(FRAME - 8);
        self.a.pop_reg(R14);
        self.a.pop_reg(R15);
        self.a.pop_reg(R13);
        self.a.mov_imm64(RAX, NOT_ENTERED);
        self.a.ret();
    }

    /// Emits `op` at instruction `ip`, which runs with `d` operands on the stack.
    fn op(&mut self, op: Op, d: i32, ip: usize) {
        let in_place = match op {
            // A read that binds its element to a local, and a read of an object's element, write locals: they copy first.
            Op::GetIndexLocal(_) => !self.binds_element(ip) && !self.lp.object_reads.contains(&ip),
            _ => reads_in_place(op),
        };
        if !in_place {
            self.materialize(d);
        }
        let (x, y) = (self.reg(d - 1), self.reg(d - 2));
        match op {
            Op::Const(k) => self.push_number(number(self.proto, k), d),
            Op::True => self.push_number(1.0, d),
            Op::False => self.push_number(0.0, d),
            // A function's `this` and callee have no register, and the code of a function starts
            // from nothing, so these take no code.
            Op::Pop if self.lp.function.is_none() && self.mark(ip, d - 1) == Mark::Owned => {
                let exit = self.side_exit(ip, d);
                let m = self.operand_mem(d - 1);
                self.check_last(m, exit);
                self.count_down(m);
            }
            // A captured number, read from the closure that the entry was called with (see `emit_function`). A value that is
            // not a number declines the call, before anything has changed.
            Op::GetUpval(k)
                if self.lp.function.as_ref().is_some_and(|f| f.upvals)
                    && self.proto.code.get(ip + 1) != Some(&Op::Undef) =>
            {
                let exit = self.side_exit(ip, d);
                let layout = arr_layout()
                    .expect("the planner admits a captured number only with a known layout");
                let cell_value = cell_value_offset()
                    .expect("the planner admits a captured number only with a known layout");
                let upvals = offset_of!(crate::value::Closure, upvals) as i32;
                let (copied, read) = (self.a.label(), self.a.label());
                // The callee's value holds the closure's allocation, whose closure follows the counts.
                self.a.mov_load(RCX, callee_mem(self.nlocals));
                self.a.mov_load(RCX, Mem { base: RCX, disp: 8 });
                self.a.add64_imm32(RCX, super::RC_VALUE);
                self.a.mov_load(
                    RSI,
                    Mem {
                        base: RCX,
                        disp: upvals + layout.len,
                    },
                );
                self.a.cmp64_imm32(RSI, k as i32);
                self.a.jcc(CC_BE, exit);
                self.a.mov_load(
                    RDX,
                    Mem {
                        base: RCX,
                        disp: upvals + layout.buf,
                    },
                );
                // A variable that the enclosing frame shares is a cell, whose number is read through the cell's allocation.
                self.a.cmp8_imm(
                    Mem {
                        base: RDX,
                        disp: 16 * k as i32,
                    },
                    TAG_CELL,
                );
                self.a.jcc(CC_NE, copied);
                self.a.mov_load(
                    RDX,
                    Mem {
                        base: RDX,
                        disp: 16 * k as i32 + 8,
                    },
                );
                self.a.cmp8_imm(
                    Mem {
                        base: RDX,
                        disp: cell_value,
                    },
                    TAG_NUM,
                );
                self.a.jcc(CC_NE, exit);
                self.a.movsd_load(
                    self.reg(d),
                    Mem {
                        base: RDX,
                        disp: cell_value + 8,
                    },
                );
                self.a.jmp(read);
                self.a.bind(copied);
                self.a.cmp8_imm(
                    Mem {
                        base: RDX,
                        disp: 16 * k as i32,
                    },
                    TAG_NUM,
                );
                self.a.jcc(CC_NE, exit);
                self.a.movsd_load(
                    self.reg(d),
                    Mem {
                        base: RDX,
                        disp: 16 * k as i32 + 8,
                    },
                );
                self.a.bind(read);
            }
            Op::Pop | Op::Undef | Op::This | Op::GetUpval(_) => {}
            Op::Dup if self.lp.function.is_none() && self.mark(ip, d - 1) == Mark::Owned => {
                let (from, to) = (self.operand_mem(d - 1), self.operand_mem(d));
                self.copy_value(from, to);
                self.count_up(to);
            }
            Op::Dup
                if self.lp.function.is_none()
                    && matches!(self.mark(ip, d - 1), Mark::View(_) | Mark::Math) => {}
            Op::Dup if self.mark(ip, d - 1) == Mark::Receiver => {}
            Op::Dup => {
                self.a.movapd_rr(self.reg(d), x);
                let (from, to) = (self.tag_cell(d - 1), self.tag_cell(d));
                self.a.movzx_load8(RAX, from);
                self.a.mov_store8(to, RAX);
            }
            // The `Math` and the `sqrt` of a call of `Math.sqrt` trade places. Neither holds a value that the trade would
            // move: a side exit writes the `sqrt` into the slot of its operand, and never reads the old contents. So the
            // trade is only in the marks.
            Op::Swap
                if self.lp.function.is_none()
                    && matches!(
                        (self.mark(ip, d - 1), self.mark(ip, d - 2)),
                        (Mark::Math | Mark::Sqrt, Mark::Math | Mark::Sqrt)
                    ) => {}
            Op::Swap if self.lp.function.is_none() && !has_register(&self.mark(ip, d - 1)) => {
                // Operands without registers trade their slots, whose values are dead unless they are owned.
                let (top, under) = (self.operand_mem(d - 1), self.operand_mem(d - 2));
                self.swap_values(top, under);
            }
            Op::Swap => {
                self.a.movapd_rr(0, x);
                self.a.movapd_rr(x, y);
                self.a.movapd_rr(y, 0);
                // The tags of the two operands trade places too.
                let (top, under) = (self.tag_cell(d - 1), self.tag_cell(d - 2));
                self.a.movzx_load8(RAX, top);
                self.a.movzx_load8(RCX, under);
                self.a.mov_store8(top, RCX);
                self.a.mov_store8(under, RAX);
            }
            // An array is read in its slot by the `push` that takes it (see `Op::Call`), so it has no register.
            Op::GetLocal(i) if self.lp.objects.contains(&i) => {}
            Op::GetLocal(i) if self.lp.arrays.contains(&i) => {}
            Op::GetLocal(i) => self.push_local(i, d),
            // The store that a fused element read already did (see `binds_element`) does nothing.
            Op::PopToLocal(_) if ip > 0 && self.binds_element(ip - 1) => {}
            Op::PopToLocal(i) if self.lp.objects.contains(&i) => {
                // The value is moved from the operand, or copied from the object local it views, and the local's
                // old value is released. Every check comes before any count changes, so a side exit leaves them
                // as they were.
                let exit = self.side_exit(ip, d);
                let dst = local_mem(i);
                let (src, owned) = match self.mark(ip, d - 1) {
                    Mark::View(s) => (local_mem(s), false),
                    _ => (self.operand_mem(d - 1), true),
                };
                self.check_last(dst, exit);
                if !owned {
                    self.count_up(src);
                }
                self.count_down(dst);
                self.copy_value(src, dst);
                self.clear_prop_caches(i);
            }
            Op::SetLocal(i) | Op::PopToLocal(i)
                if self.mark(ip, d - 1) == Mark::Elem && !self.lp.objects.contains(&i) =>
            {
                // A boolean element is not a number, so the store leaves the loop at the store, where the generic code
                // keeps the tag. A number goes to the local.
                let exit = self.side_exit(ip, d);
                self.a.cmp8_imm(self.tag_cell(d - 1), TAG_NUM);
                self.a.jcc(CC_NE, exit);
                self.write_local(i, x);
            }
            Op::SetLocal(i) | Op::PopToLocal(i) => self.write_local(i, x),
            Op::AddToLocal(i) => {
                // The pending operands below the top are copied out first, since the local is written.
                self.materialize(d - 1);
                let x = self.operand(d - 1);
                self.update_local(i, SSE_ADD, x);
            }
            Op::IncLocal(i) => self.step_counter(i, SSE_ADD, 1, ip),
            Op::DecLocal(i) => self.step_counter(i, SSE_SUB, -1, ip),
            Op::AddLK(i, k) => self.with_constant(i, k, SSE_ADD, self.reg(d)),
            Op::SubLK(i, k) => self.with_constant(i, k, SSE_SUB, self.reg(d)),
            Op::MulLK(i, k) => self.with_constant(i, k, SSE_MUL, self.reg(d)),
            Op::ModLK(i, k) => self.mod_constant(i, k, self.reg(d)),
            Op::Add => self.arith(SSE_ADD, d),
            Op::Sub => self.arith(SSE_SUB, d),
            Op::Mul => self.arith(SSE_MUL, d),
            Op::Div => self.arith(SSE_DIV, d),
            Op::Mod => match self.magic_divisor(ip) {
                Some((n, c)) => self.mod_top(y, n, c, self.mark(ip, d - 2) == Mark::Int),
                None => {
                    let (slow, done) = (self.a.label(), self.a.label());
                    self.int_remainder(y, x, slow);
                    self.a.jmp(done);
                    self.a.bind(slow);
                    self.a.movapd_rr(0, y);
                    self.a.movapd_rr(1, x);
                    self.call_mod(y);
                    self.a.bind(done);
                }
            },
            Op::Neg => {
                self.a.mov_imm64(RAX, 1 << 63);
                self.a.movq_to_xmm(0, RAX);
                self.a.xorpd(x, 0);
            }
            // `a < b` is `b > a`, and an unordered compare (NaN) sets CF, so the `above` tests are false.
            Op::Lt => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                let dst = self.reg(d - 2);
                self.set(dst, CC_A);
            }
            Op::Le => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                let dst = self.reg(d - 2);
                self.set(dst, CC_AE);
            }
            Op::Gt => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                let dst = self.reg(d - 2);
                self.set(dst, CC_A);
            }
            Op::Ge => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                let dst = self.reg(d - 2);
                self.set(dst, CC_AE);
            }
            Op::Eq | Op::StrictEq => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                let dst = self.reg(d - 2);
                self.set_equal(dst, true);
            }
            Op::Ne | Op::StrictNe => {
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                let dst = self.reg(d - 2);
                self.set_equal(dst, false);
            }
            Op::Not => {
                self.zero_test(x);
                self.set(x, CC_E);
            }
            Op::Jump(t) => {
                let target = self.target(t);
                self.a.jmp(target);
            }
            Op::JumpIfFalse(t) => {
                self.materialize(d - 1);
                let x = self.operand(d - 1);
                self.zero_test(x);
                self.jump_if(t, CC_E);
            }
            // `a && b` and `a || b` keep `a` on the stack when they jump, and pop it when they do not.
            Op::AndJump(t) => {
                self.zero_test(x);
                self.jump_if(t, CC_E);
            }
            Op::OrJump(t) => {
                self.zero_test(x);
                self.jump_if(t, CC_NE);
            }
            Op::JumpIfTrue(t) => {
                self.materialize(d - 1);
                let x = self.operand(d - 1);
                self.zero_test(x);
                self.jump_if(t, CC_NE);
            }
            Op::JumpIfNotLt(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfNotLe(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                self.jump_if(t, CC_B);
            }
            // Equal numbers set ZF and not PF, since an unordered (NaN) comparison sets both.
            Op::JumpIfStrictEq(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                let not_equal = self.a.label();
                self.a.jcc(CC_P, not_equal);
                self.jump_if(t, CC_E);
                self.a.bind(not_equal);
            }
            Op::JumpIfNotStrictEq(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(x, y);
                self.jump_if(t, CC_P);
                self.jump_if(t, CC_NE);
            }
            Op::JumpIfNotGt(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfNotGe(t) => {
                self.materialize(d - 2);
                let (x, y) = self.operands(d);
                self.a.ucomisd(y, x);
                self.jump_if(t, CC_B);
            }
            Op::JumpIfLtLK(i, k, t) => {
                self.materialize(d);
                let local = self.local_src(i, 1);
                let c = self.constant(number(self.proto, k).to_bits(), 0);
                self.a.ucomisd(c, local);
                self.jump_if(t, CC_A);
            }
            Op::JumpIfNotLtLK(i, k, t) => {
                self.materialize(d);
                let local = self.local_src(i, 1);
                let c = self.constant(number(self.proto, k).to_bits(), 0);
                self.a.ucomisd(c, local);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfLtLL(p, q, t) => {
                self.materialize(d);
                let a = self.local_src(p, 0);
                let b = self.local_src(q, 1);
                self.a.ucomisd(b, a);
                self.jump_if(t, CC_A);
            }
            Op::JumpIfNotLtLL(p, q, t) => {
                self.materialize(d);
                let a = self.local_src(p, 0);
                let b = self.local_src(q, 1);
                self.a.ucomisd(b, a);
                self.jump_if(t, CC_BE);
            }
            // The body of an inlined method returns by continuing after the call.
            Op::Return if self.inline.is_some() => {
                if let Some(ctx) = self.inline {
                    self.a.jmp(ctx.cont);
                }
            }
            Op::Return => {
                match self.lp.function.as_ref() {
                    // A method returns `undefined`, with status 0 in rax, which the wrapper passes on.
                    Some(f) if f.receiver => self.a.mov_imm32(RAX, 0),
                    // A closure returns its number, with status 0 in rax, which the wrapper passes on too.
                    Some(f) if f.upvals => {
                        self.a.movapd_rr(0, x);
                        self.a.mov_imm32(RAX, 0);
                    }
                    _ => self.a.movapd_rr(0, x),
                }
                self.a.mov_rr(RSP, RBP);
                self.a.pop_reg(RBP);
                self.a.ret();
            }
            // An element bound to an object local is copied into the local directly, and the `PopToLocal` after it does
            // nothing (see `binds_element`). Every check comes before any count changes, as in that `PopToLocal`.
            Op::GetIndexLocal(a) if self.binds_element(ip) => {
                let exit = self.side_exit(ip, d);
                let key = self.reg(d - 1);
                let Some(&Op::PopToLocal(i)) = self.proto.code.get(ip + 1) else {
                    unreachable!("a bound element is followed by its store (see `binds_element`)")
                };
                self.a.mov_load(R8, local_mem(a).at(8));
                self.elem_addr(key, self.mark(ip, d - 1) == Mark::Int, exit);
                self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_OBJ);
                self.a.jcc(CC_NE, exit);
                let (element, dst) = (Mem { base: RDX, disp: 0 }, local_mem(i));
                // The element is an object, so its copy always counts one more reference, and the old value of the local
                // is released first, which the check of its last reference must allow (see `release_checked`).
                self.release_checked(dst, exit);
                self.a.mov_load(R11, element.at(8));
                self.a.add64_mem_imm8(Mem { base: R11, disp: 0 }, 1);
                self.copy_value(element, dst);
                self.clear_prop_caches(i);
            }
            Op::GetIndexLocal(a) if self.lp.object_reads.contains(&ip) => {
                // The element must be an object. It is copied to the operand, and the copy counts one more reference.
                let exit = self.side_exit(ip, d);
                let key = self.reg(d - 1);
                self.a.mov_load(R8, local_mem(a).at(8));
                self.elem_addr(key, self.mark(ip, d - 1) == Mark::Int, exit);
                self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_OBJ);
                self.a.jcc(CC_NE, exit);
                let to = self.operand_mem(d - 1);
                self.copy_value(Mem { base: RDX, disp: 0 }, to);
                self.count_up(to);
            }
            Op::GetIndexLocal(a) => {
                let exit = self.side_exit(ip, d);
                let key = self.operand(d - 1);
                let (dst, tag) = (self.reg(d - 1), self.tag_cell(d - 1));
                self.a.mov_load(R8, local_mem(a).at(8));
                self.elem_read(key, dst, tag, self.mark(ip, d - 1) == Mark::Int, exit);
            }
            Op::GetIndex2Local(a) => {
                let exit = self.side_exit(ip, d);
                let whole = self.mark(ip, d - 2) == Mark::Int;
                // The row key and the key are read where they are, and the element is written to the depth of the row.
                let (row, key, tag) = (
                    self.operand(d - 2),
                    self.operand(d - 1),
                    self.tag_cell(d - 2),
                );
                match self.row_slots(ip) {
                    Some((key_slot, row_slot)) if whole => {
                        self.row_cached(row, key_slot, row_slot, local_mem(a).at(8), exit)
                    }
                    _ => {
                        self.a.mov_load(R8, local_mem(a).at(8));
                        self.elem_row(row, whole, exit)
                    }
                }
                let dst = self.reg(d - 2);
                self.elem_read(key, dst, tag, self.mark(ip, d - 1) == Mark::Int, exit);
            }
            Op::SetIndexLocal(a) => {
                // The key and the value are read where they are. The exit copies the pending operands back first (see
                // `side_exit`), so it is taken before they are read.
                let exit = self.side_exit(ip, d);
                let boolean = self.mark(ip, d - 1) == Mark::Bool;
                let (value, key) = self.operands(d);
                let dst = self.reg(d - 2);
                self.a.mov_load(R8, local_mem(a).at(8));
                self.elem_write(key, value, boolean, self.mark(ip, d - 2) == Mark::Int, exit);
                // A statement drops the result, so nothing reads it and it is not copied.
                if !self.discards(ip) {
                    self.a.movapd_rr(dst, value);
                }
            }
            Op::SetIndex2Local(a) => {
                let exit = self.side_exit(ip, d);
                let boolean = self.mark(ip, d - 1) == Mark::Bool;
                let (row, key, value) = (self.reg(d - 3), self.reg(d - 2), self.reg(d - 1));
                self.a.mov_load(R8, local_mem(a).at(8));
                self.elem_row(row, self.mark(ip, d - 3) == Mark::Int, exit);
                self.elem_write(key, value, boolean, self.mark(ip, d - 2) == Mark::Int, exit);
                self.a.movapd_rr(row, value);
            }
            // The built-in `Math` is not read here: the entry checked that `Math.sqrt` is the built-in (see `emit`).
            Op::GetGlobal(k) if self.lp.math.map(|m| m.0) == Some(k) => {}
            Op::GetGlobal(k) => {
                // A global is read in place, as the generic code reads it (see `find_global`). It must be an object,
                // which the operand takes a reference to.
                let exit = self.side_exit(ip, d);
                let name = &self.proto.names[k as usize] as *const std::rc::Rc<str> as usize;
                let hint =
                    &self.proto.global_hints[k as usize] as *const std::cell::Cell<u32> as usize;
                let layout =
                    entry_layout().expect("the planner admits a global only with a known layout");
                find_global(&mut *self.a, name, hint, layout, exit);
                let value = Mem {
                    base: RDX,
                    disp: layout.value,
                };
                self.a.cmp8_imm(value, TAG_OBJ);
                self.a.jcc(CC_NE, exit);
                let to = self.operand_mem(d);
                self.copy_value(value, to);
                self.count_up(to);
            }
            // The callee of a method is looked up by the call, which takes the receiver from the same local (see `Mark::Method`).
            Op::GetLocalProp(..) if matches!(self.mark(ip + 1, d), Mark::Method(..)) => {}
            Op::GetLocalProp(s, n) if self.lp.objects.contains(&s) => {
                self.prop_read(ip, d, self.reg(d), local_mem(s), n, Some(s));
            }
            Op::GetLocalProp(s, n) if is_length(self.proto, n) => {
                // The length of the array in local `s`, which the loop checked on entry to be an array.
                let layout =
                    arr_layout().expect("the planner admits a length only with a known layout");
                self.a.mov_load(RCX, local_mem(s).at(8));
                self.a.add64_imm32(RCX, layout.vec);
                self.a.mov_load(
                    RSI,
                    Mem {
                        base: RCX,
                        disp: layout.len,
                    },
                );
                self.a.cvtsi2sd(self.reg(d), RSI);
            }
            Op::GetLocalProp(s, _) => self.push_probe(ip, d, s),
            Op::GetProp(_) if self.lp.function.is_none() && self.mark(ip, d - 1) == Mark::Math => {}
            Op::GetProp(n) if self.lp.function.is_none() && self.mark(ip, d - 1) == Mark::Owned => {
                // The `sqrt` of an object: the property must be the built-in. The object gives up its reference after
                // every check, so a side exit leaves it as it was.
                let exit = self.side_exit(ip, d);
                let obj = self.operand_mem(d - 1);
                let site = self.prop_site(n);
                find_prop(&mut *self.a, obj, site, exit);
                self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_NATIVE);
                self.a.jcc(CC_NE, exit);
                self.a.mov_load(RCX, Mem { base: RDX, disp: 8 });
                self.a.mov_imm64(RAX, sqrt_address());
                self.a.cmp_rr64(RCX, RAX);
                self.a.jcc(CC_NE, exit);
                self.check_last(obj, exit);
                self.count_down(obj);
            }
            Op::GetProp(n) => {
                let view = self.mark(ip, d - 1);
                let obj = self.view_mem(view);
                self.prop_read(ip, d, self.reg(d - 1), obj, n, view_local(view));
                // The property's address is kept in r14 for a `SetProp` that follows (see `cache_holds`).
                if self.r14_free() {
                    self.prop_cache = Some((ip, n, view));
                }
            }
            Op::SetProp(n) => {
                // The object is a view or the receiver, and the value is a number that replaces a number that the
                // property holds.
                let view = self.mark(ip, d - 2);
                let cached = self.prop_cache.is_some_and(|(at, name, read)| {
                    name == n && read == view && self.cache_holds(at, ip, view)
                });
                if cached {
                    // The `GetProp` before it found the property, and nothing since could move the property or change
                    // the object, so its address is still in r14. The property held a number then, and still does.
                    self.a
                        .movsd_store(Mem { base: R14, disp: 8 }, self.reg(d - 1));
                    self.a.movapd_rr(self.reg(d - 2), self.reg(d - 1));
                } else {
                    let obj = self.view_mem(view);
                    let exit = self.side_exit(ip, d);
                    self.prop_address(obj, view_local(view), n, exit);
                    self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_NUM);
                    self.a.jcc(CC_NE, exit);
                    self.a
                        .movsd_store(Mem { base: RDX, disp: 8 }, self.reg(d - 1));
                    self.a.movapd_rr(self.reg(d - 2), self.reg(d - 1));
                }
            }
            Op::Call(1) if self.lp.function.is_none() && self.mark(ip, d - 3) == Mark::Sqrt => {
                // The object is released after its checks, as the other side exits leave the counts alone until then.
                if self.mark(ip, d - 2) == Mark::Owned {
                    let exit = self.side_exit(ip, d);
                    let this = self.operand_mem(d - 2);
                    self.check_last(this, exit);
                    self.count_down(this);
                }
                self.a.sse(SSE_SQRT, self.reg(d - 3), self.reg(d - 1));
            }
            Op::Call(1) if matches!(self.mark(ip, d - 3), Mark::Method(..)) => {
                self.call_method(ip, d)
            }
            Op::Call(_) if self.lp.function.is_none() => self.push_call(ip, d),
            Op::Call(argc) => self.call(d, argc as i32),
            _ => unreachable!("the planners admit only the operations above"),
        }
    }

    /// The call of a method (see `Mark::Method`). The callee is looked up on the object in the local, which must hold a
    /// closure whose prototype is a method entry for one argument. The entry takes the argument as a double on the
    /// native stack, and the receiver's address. The entry clobbers every xmm register, so the live ones are saved
    /// around the call. A closure of another kind, or a call that the entry declines, leaves the loop at the read of
    /// the callee, where the generic code makes the call (see `side_exit`).
    fn call_method(&mut self, ip: usize, d: i32) {
        let Mark::Method(s, n, read) = self.mark(ip, d - 3) else {
            unreachable!("the planner admits a call only of a method's callee (see `step`)")
        };
        let exit = self.side_exit(read as usize, d - 3);
        let obj = local_mem(s);
        self.prop_address(obj, Some(s), n, exit);
        self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_FUNC);
        self.a.jcc(CC_NE, exit);
        self.a.mov_load(RCX, Mem { base: RDX, disp: 8 });
        self.a.add64_imm32(RCX, super::RC_VALUE);
        self.a.mov_load(
            RDX,
            Mem {
                base: RCX,
                disp: std::mem::offset_of!(crate::value::Closure, proto) as i32,
            },
        );
        self.a.add64_imm32(RDX, super::RC_VALUE);
        // A loop that runs the callee's body in place (see `with_inline`) checks that the callee is that body's function.
        // That function takes the one argument of the call, so the argument count is not checked for it.
        let inlined = self
            .lp
            .inline
            .as_deref()
            .filter(|i| i.call == ip)
            .map(|i| std::rc::Rc::as_ptr(&i.proto));
        if let Some(target) = inlined {
            self.a.mov_imm64(R11, target as usize as u64);
            self.a.cmp_rr64(RDX, R11);
            self.a.jcc(CC_NE, exit);
            self.inline_body(ip, d, s, exit);
            return;
        }
        self.a.mov32_load(
            R9,
            Mem {
                base: RDX,
                disp: std::mem::offset_of!(Proto, method_argc) as i32,
            },
        );
        self.a.cmp64_imm32(R9, 1);
        self.a.jcc(CC_NE, exit);
        self.a.mov_load(
            RAX,
            Mem {
                base: RDX,
                disp: std::mem::offset_of!(Proto, method_entry) as i32,
            },
        );
        self.a.test64(RAX, RAX);
        self.a.jcc(CC_E, exit);
        // The live registers are saved in the slots below the argument, and the room keeps the stack aligned.
        let live = self.live_registers(ip, d - 3);
        let saved = live.len() as i32;
        let room = 16 * ((saved + 2) / 2);
        self.a.sub_rsp32(room);
        for (k, &x) in live.iter().enumerate() {
            self.a.movsd_store(
                Mem {
                    base: RSP,
                    disp: 8 * k as i32,
                },
                x,
            );
        }
        let arg = self.reg(d - 1);
        self.a.movsd_store(
            Mem {
                base: RSP,
                disp: 8 * saved,
            },
            arg,
        );
        self.a.lea(
            RDI,
            Mem {
                base: RSP,
                disp: 8 * saved,
            },
        );
        self.a.lea(RSI, obj);
        self.a.call_reg(RAX);
        for (k, &x) in live.iter().enumerate() {
            self.a.movsd_load(
                x,
                Mem {
                    base: RSP,
                    disp: 8 * k as i32,
                },
            );
        }
        self.a.add_rsp32(room);
        // The entry returns 1 when it declined, before it wrote anything. Then the loop leaves at the read.
        self.a.test64(RAX, RAX);
        self.a.jcc(CC_NE, exit);
        // The entry ran script code, which may replace a row, so the row caches go. That code may also have changed an
        // object, which the property caches notice from the count of structural changes.
        self.forget_row_caches();
        self.check_structure();
    }

    /// The xmm registers that hold a live value below `depth` operands: the locals and constants that the loop keeps in
    /// registers, and the operands below `depth` that have one.
    fn live_registers(&self, ip: usize, depth: i32) -> Vec<u8> {
        let mut live: Vec<u8> = self
            .lp
            .locals
            .iter()
            .filter_map(|&(_, home)| match home {
                Home::Reg(x) => Some(x),
                Home::Mem(_) => None,
            })
            .collect();
        live.extend(self.lp.consts.iter().map(|&(_, x)| x));
        for k in 0..depth {
            if has_register(&self.mark(ip, k)) {
                live.push(self.reg(k));
            }
        }
        live.sort_unstable();
        live.dedup();
        live
    }

    /// Whether the operator at `ip` is followed by a `Pop`, which drops its result, as a statement does.
    fn discards(&self, ip: usize) -> bool {
        matches!(self.proto.code.get(ip + 1), Some(&Op::Pop))
    }

    /// The probe of a `push` on the array in local `s`: the local must hold an array with room for one more element.
    /// Anything else leaves the loop at `ip`, where the generic code runs the instruction. Nothing is on the stack
    /// before the probe, so the side exit writes back the operands below it, and no more.
    fn push_probe(&mut self, ip: usize, d: i32, s: u32) {
        let exit = self.side_exit(ip, d);
        let layout = arr_layout().expect("the planner admits a push only with a known layout");
        self.a.cmp8_imm(local_mem(s), TAG_ARR);
        self.a.jcc(CC_NE, exit);
        self.a.mov_load(R8, local_mem(s).at(8));
        self.a.mov_load(
            RAX,
            Mem {
                base: R8,
                disp: layout.vec + layout.len,
            },
        );
        self.a.mov_load(
            RDX,
            Mem {
                base: R8,
                disp: layout.vec + layout.cap,
            },
        );
        self.a.cmp_rr64(RAX, RDX);
        self.a.jcc(CC_AE, exit);
    }

    /// The call of a `push` whose probe checked the room (see `push_probe`). The value, a number or a boolean, goes
    /// past the end of the array in local `s`, and the array's length grows. The new length is the result, which takes
    /// the register of the callee.
    fn push_call(&mut self, ip: usize, d: i32) {
        let Mark::Array(s) = self.mark(ip, d - 2) else {
            unreachable!("the planner admits a call only as a push of an array (see `step`)")
        };
        let layout = arr_layout().expect("the planner admits a push only with a known layout");
        let value = self.reg(d - 1);
        let boolean = self.mark(ip, d - 1) == Mark::Bool;
        self.a.mov_load(R8, local_mem(s).at(8));
        self.a.mov_load(
            RSI,
            Mem {
                base: R8,
                disp: layout.vec + layout.len,
            },
        );
        self.a.mov_load(
            RDX,
            Mem {
                base: R8,
                disp: layout.vec + layout.buf,
            },
        );
        self.a.mov_rr(RAX, RSI);
        self.a.shl64_imm(RAX, 4);
        self.a.add64(RDX, RAX);
        if boolean {
            self.a.mov8_imm(Mem { base: RDX, disp: 0 }, TAG_BOOL);
            self.a.cvttsd2si(RAX, value);
            self.a.mov_store8(Mem { base: RDX, disp: 1 }, RAX);
        } else {
            self.a.mov8_imm(Mem { base: RDX, disp: 0 }, TAG_NUM);
            self.a.movsd_store(Mem { base: RDX, disp: 8 }, value);
        }
        self.a.add64_imm8(RSI, 1);
        self.a.mov_store(
            Mem {
                base: R8,
                disp: layout.vec + layout.len,
            },
            RSI,
        );
        // A statement drops the new length, so nothing reads it and it is not converted.
        if !self.discards(ip) {
            self.a.cvtsi2sd(self.reg(d - 3), RSI);
        }
    }

    /// A call of the function's own body with `argc` arguments, which are the top operands. The callee
    /// and `this` below them take no registers, and the result replaces them.
    fn call(&mut self, d: i32, argc: i32) {
        let base = d - argc - 2;
        for k in 0..base {
            self.a.movsd_store(self.operand_slot(k), self.reg(k));
        }
        for j in 0..argc {
            self.a.movapd_rr(j as u8, self.reg(d - argc + j));
        }
        let body = self
            .body
            .expect("calls are only planned in a function region");
        self.a.call_label(body);
        self.a.movapd_rr(self.reg(base), 0);
        for k in 0..base {
            self.a.movsd_load(self.reg(k), self.operand_slot(k));
        }
    }

    /// The xmm register of the operand at depth `k`.
    fn reg(&self, k: i32) -> u8 {
        if let Some(ctx) = self.inline {
            return (ctx.reg_base as i32 + k) as u8;
        }
        let base = if self.lp.function.is_some() {
            FN_TEMP_BASE
        } else {
            TEMP_BASE
        };
        (base as i32 + k) as u8
    }

    /// The frame slot where a call saves operand register `k`, below the locals.
    fn operand_slot(&self, k: i32) -> Mem {
        Mem {
            base: RBP,
            disp: -8 * (self.nlocals + k + 1),
        }
    }

    /// The home of local `i`.
    fn home(&self, i: u32) -> Home {
        self.lp
            .locals
            .iter()
            .find(|&&(local, _)| local == i)
            .map(|&(_, home)| home)
            .expect("every local of the region has a home")
    }

    /// Copies local `i` into xmm `dst`.
    fn read_local(&mut self, i: u32, dst: u8) {
        match self.home(i) {
            Home::Reg(x) => self.a.movapd_rr(dst, x),
            Home::Mem(m) => self.a.movsd_load(dst, m),
        }
    }

    /// The register that holds local `i`: its own, or `scratch`, which is loaded with it.
    fn local_src(&mut self, i: u32, scratch: u8) -> u8 {
        match self.home(i) {
            Home::Reg(x) => x,
            Home::Mem(m) => {
                self.a.movsd_load(scratch, m);
                scratch
            }
        }
    }

    /// Stores xmm `src` as local `i`.
    fn write_local(&mut self, i: u32, src: u8) {
        match self.home(i) {
            Home::Reg(x) => self.a.movapd_rr(x, src),
            Home::Mem(m) => {
                // A loop's frame slot is a value, whose tag comes before the payload that `m` addresses. A
                // function's slot holds the number alone.
                if self.lp.function.is_none() {
                    self.a.mov8_imm(m.at(-8), TAG_NUM);
                }
                self.a.movsd_store(m, src);
            }
        }
    }

    /// `local op= src`, by `sse`.
    fn update_local(&mut self, i: u32, sse: u8, src: u8) {
        match self.home(i) {
            Home::Reg(x) => self.a.sse(sse, x, src),
            Home::Mem(m) => {
                self.a.movsd_load(0, m);
                self.a.sse(sse, 0, src);
                self.a.movsd_store(m, 0);
            }
        }
    }

    /// Leaves in `dst` (the left operand) the remainder of `left` by `right`, when both are whole numbers, `left` has no sign
    /// bit and `right` is positive. Any other pair jumps to `slow`, which computes the remainder in full.
    fn int_remainder(&mut self, left: u8, right: u8, slow: Label) {
        self.a.cvttsd2si(RAX, left);
        self.a.cvtsi2sd(0, RAX);
        self.a.ucomisd(left, 0);
        self.a.jcc(CC_NE, slow);
        self.a.jcc(CC_P, slow);
        // The sign bit also marks -0, whose remainder is -0, so it takes the slow path too.
        self.a.movmskpd(RAX, left);
        self.a.mov_imm32(RCX, 1);
        self.a.test32(RAX, RCX);
        self.a.jcc(CC_NE, slow);
        self.a.cvttsd2si(RCX, right);
        self.a.cvtsi2sd(0, RCX);
        self.a.ucomisd(right, 0);
        self.a.jcc(CC_NE, slow);
        self.a.jcc(CC_P, slow);
        self.a.test32(RCX, RCX);
        self.a.jcc(CC_LE, slow);
        self.a.cvttsd2si(RAX, left);
        self.a.mov_imm32(RDX, 0);
        self.a.div32(RCX);
        self.a.cvtsi2sd(left, RDX);
    }

    /// Binds the label of instruction `ip`. A jump that reaches it leaves every operand in its register, so the code that
    /// falls through to it copies the pending operands first.
    fn bind_at(&mut self, at: Label, ip: usize) {
        if self.jumped_to[ip] {
            self.materialize(PENDING as i32);
        }
        self.a.bind(at);
    }

    /// Copies the pending operands at depths below `below` into their registers.
    fn materialize(&mut self, below: i32) {
        for k in 0..(below.max(0) as usize).min(PENDING) {
            if let Some(src) = self.pending[k].take() {
                self.load_src(k as i32, src);
            }
        }
    }

    /// Copies the pending operand `src` into the register of depth `k`.
    fn load_src(&mut self, k: i32, src: Src) {
        let dst = self.reg(k);
        match src {
            Src::Reg(r) => {
                if r != dst {
                    self.a.movapd_rr(dst, r);
                }
            }
            Src::Mem(m) => self.a.movsd_load(dst, m),
        }
    }

    /// The register of the operand at depth `k`, for an operator that only reads it. A pending operand is read where it
    /// is, so it is not copied for this read.
    fn operand(&mut self, k: i32) -> u8 {
        match self.pending[k as usize].take() {
            Some(Src::Reg(r)) => r,
            Some(Src::Mem(m)) => {
                let dst = self.reg(k);
                self.a.movsd_load(dst, m);
                dst
            }
            None => self.reg(k),
        }
    }

    /// The registers of the operands of an operator at height `d`: the right one (at depth `d - 1`), then the left one.
    fn operands(&mut self, d: i32) -> (u8, u8) {
        let x = self.operand(d - 1);
        let y = self.operand(d - 2);
        (x, y)
    }

    /// Pushes the number `n` at depth `d`. A constant of the pool is read where it is, and another one is loaded into the
    /// register of the depth.
    fn push_number(&mut self, n: f64, d: i32) {
        let c = self.constant(n.to_bits(), self.reg(d));
        if c != self.reg(d) {
            self.pending[d as usize] = Some(Src::Reg(c));
        }
    }

    /// Pushes local `i` at depth `d`, to be read where the local is.
    fn push_local(&mut self, i: u32, d: i32) {
        self.pending[d as usize] = Some(match self.home(i) {
            Home::Reg(x) => Src::Reg(x),
            Home::Mem(m) => Src::Mem(m),
        });
    }

    /// `left op right`, into the register of the left operand, for the operators that read both operands in place.
    fn arith(&mut self, sse: u8, d: i32) {
        let (x, y) = self.operands(d);
        let dst = self.reg(d - 2);
        if y != dst {
            self.a.movapd_rr(dst, y);
        }
        self.a.sse(sse, dst, x);
    }

    /// The byte where the tag of the element operand at depth `k` is kept, which a side exit writes back.
    fn tag_cell(&self, k: i32) -> Mem {
        Mem {
            base: RSP,
            disp: TAG_CELLS + k,
        }
    }

    /// The kind of the operand at depth `k` before instruction `ip`.
    fn mark(&self, ip: usize, k: i32) -> Mark {
        self.lp.marks[ip - self.lp.header][k as usize]
    }

    /// The receiver of the method's body: the object local that the call reads the method from, when the body runs in
    /// place, and the receiver of a function region otherwise.
    fn receiver_home(&self) -> Mem {
        match self.inline {
            Some(ctx) => ctx.receiver,
            None => receiver_mem(self.nlocals),
        }
    }

    /// Runs the body of the method that the loop inlines in place of the call at `ip`, whose callee is at depth `d - 3`.
    /// The body's operands go on from the callee's depth. Its declines leave at the call's exit, which is safe because
    /// they come before any write, and its return continues after the call.
    fn inline_body(&mut self, ip: usize, d: i32, s: u32, exit: Label) {
        let lp = self.lp;
        let inl = lp.inline.as_deref().expect("an inlined call has its body");
        let arg = self.reg(d - 1);
        self.a.movsd_store(arg_payload(), arg);
        let cont = self.at[ip + 1 - lp.header];
        let at: Vec<Label> = (0..inl.proto.code.len()).map(|_| self.a.label()).collect();
        let not_entered = self.a.label();
        {
            let mut body = Emitter {
                a: &mut *self.a,
                proto: &*inl.proto,
                depth: &inl.depth,
                lp: &inl.lp,
                at,
                exits: Vec::new(),
                not_entered,
                slow: Vec::new(),
                body: None,
                nlocals: inl.proto.nlocals as i32,
                sides: Vec::new(),
                prop_cache: None,
                inline: Some(InlineCtx {
                    receiver: local_mem(s),
                    reg_base: (TEMP_BASE as i32 + d - 3) as u8,
                    exit,
                    cont,
                }),
                pending: [None; PENDING],
                jumped_to: jump_targets(&inl.proto),
            };
            body.emit_inline();
        }
        self.prop_cache = None;
        self.forget_row_caches();
        self.check_structure();
    }

    /// Emits the instructions of an inlined body, then its out-of-line paths. The body ends by jumping to the
    /// continuation of the call, which its return jumps to as well.
    fn emit_inline(&mut self) {
        for ip in 0..self.proto.code.len() {
            let at = self.at[ip];
            self.bind_at(at, ip);
            let d = self.depth[ip];
            if d < 0 {
                self.a.ud2();
            } else {
                let op = self.proto.code[ip];
                self.op(op, d, ip);
            }
        }
        self.materialize(PENDING as i32);
        if let Some(ctx) = self.inline {
            self.a.jmp(ctx.cont);
        }
        for (entry, join, dst, src, c) in std::mem::take(&mut self.slow) {
            self.a.bind(entry);
            self.a.movapd_rr(0, src);
            self.a.mov_imm64(RCX, c.to_bits());
            self.a.movq_to_xmm(1, RCX);
            self.call_mod(dst);
            self.a.jmp(join);
        }
    }

    /// A stub that leaves the loop so that the generic code runs the element operation at `ip`, which has `d` operands.
    fn side_exit(&mut self, ip: usize, d: i32) -> Label {
        if let Some(ctx) = self.inline {
            return ctx.exit;
        }
        let stub = self.a.label();
        // The operands still pending here are read where they are, so the stub copies them into their registers first.
        let pending: Vec<(i32, Src)> = (0..PENDING)
            .filter_map(|k| self.pending[k].map(|src| (k as i32, src)))
            .collect();
        self.sides.push((stub, ip, d, pending));
        stub
    }

    /// Writes the `d` operands back to the stack, each with the tag its kind has.
    fn spill(&mut self, ip: usize, d: i32) {
        for k in 0..d {
            let slot = local_mem((self.proto.nlocals as i32 + k) as u32);
            let x = self.reg(k);
            match self.mark(ip, k) {
                Mark::Bool => {
                    self.a.mov8_imm(slot, TAG_BOOL);
                    self.a.cvttsd2si(RAX, x);
                    self.a.mov_store8(slot.at(1), RAX);
                }
                Mark::Elem => {
                    let (num, done) = (self.a.label(), self.a.label());
                    let tag = self.tag_cell(k);
                    self.a.movzx_load8(RAX, tag);
                    self.a.mov_store8(slot, RAX);
                    self.a.cmp64_imm8(RAX, TAG_BOOL);
                    self.a.jcc(CC_NE, num);
                    self.a.cvttsd2si(RAX, x);
                    self.a.mov_store8(slot.at(1), RAX);
                    self.a.jmp(done);
                    self.a.bind(num);
                    self.a.movsd_store(slot.at(8), x);
                    self.a.bind(done);
                }
                Mark::Push(_) | Mark::Array(_) | Mark::Method(..) => {
                    unreachable!("a receiver is never on the stack at a side exit (see `step`)")
                }
                Mark::View(s) => {
                    self.copy_value(local_mem(s), slot);
                    self.count_up(slot);
                }
                Mark::Owned => {}
                Mark::Sqrt => {
                    self.a.mov8_imm(slot, TAG_NATIVE);
                    self.a.mov_imm64(RAX, sqrt_address());
                    self.a.mov_store(slot.at(8), RAX);
                }
                Mark::Math => unreachable!(
                    "the built-in `Math` is never on the stack at a side exit (see `step`)"
                ),
                _ => {
                    self.a.mov8_imm(slot, TAG_NUM);
                    self.a.movsd_store(slot.at(8), x);
                }
            }
        }
    }

    /// Leaves the address of element `key` of the array in r8 in rdx, with the index in rax. A key that is not a whole
    /// index inside the array jumps to `exit`.
    fn elem_addr(&mut self, key: u8, whole: bool, exit: Label) {
        // `allowed` admits element operations only when the layout is known.
        let layout =
            arr_layout().expect("the planner admits array elements only with a known layout");
        self.a.cvttsd2si(RAX, key);
        // A whole key is an index as it is. Any other key is a whole index when truncating it and converting it back give
        // the same number. NaN fails the comparison, which sets the parity flag when the operands are unordered. A whole key
        // out of the 32-bit range truncates to the most negative int, which the bounds below refuse.
        if !whole {
            self.a.cvtsi2sd(1, RAX);
            self.a.ucomisd(key, 1);
            self.a.jcc(CC_NE, exit);
            self.a.jcc(CC_P, exit);
        }
        // The index is zero-extended, so a negative one is huge as an unsigned number, and the bound below refuses it.
        self.a.mov_load(
            RSI,
            Mem {
                base: R8,
                disp: layout.vec + layout.len,
            },
        );
        self.a.cmp_rr64(RAX, RSI);
        self.a.jcc(CC_AE, exit);
        self.a.mov_load(
            RDX,
            Mem {
                base: R8,
                disp: layout.vec + layout.buf,
            },
        );
        self.a.shl64_imm(RAX, 4);
        self.a.add64(RDX, RAX);
    }

    /// Loads element `key` of the array in r8 into `dst`, as a number or as 0.0 or 1.0 for a boolean, and its tag into
    /// `tag`. Any other element jumps to `exit`.
    fn elem_read(&mut self, key: u8, dst: u8, tag: Mem, whole: bool, exit: Label) {
        self.elem_addr(key, whole, exit);
        let (num, done) = (self.a.label(), self.a.label());
        self.a.movzx_load8(RAX, Mem { base: RDX, disp: 0 });
        self.a.cmp64_imm8(RAX, TAG_NUM);
        self.a.jcc(CC_E, num);
        self.a.cmp64_imm8(RAX, TAG_BOOL);
        self.a.jcc(CC_NE, exit);
        self.a.movzx_load8(RCX, Mem { base: RDX, disp: 1 });
        self.a.cvtsi2sd(dst, RCX);
        self.a.jmp(done);
        self.a.bind(num);
        self.a.movsd_load(dst, Mem { base: RDX, disp: 8 });
        self.a.bind(done);
        self.a.mov_store8(tag, RAX);
    }

    /// Stores the number or boolean in `src` as element `key` of the array in r8. An element that holds a reference
    /// jumps to `exit`, before anything is written.
    fn elem_write(&mut self, key: u8, src: u8, boolean: bool, whole: bool, exit: Label) {
        self.elem_addr(key, whole, exit);
        self.a.movzx_load8(RAX, Mem { base: RDX, disp: 0 });
        // The tags up to number are undefined, null, boolean and number, which hold no reference.
        self.a.cmp64_imm8(RAX, TAG_NUM);
        self.a.jcc(CC_A, exit);
        if boolean {
            self.a.mov8_imm(Mem { base: RDX, disp: 0 }, TAG_BOOL);
            self.a.cvttsd2si(RAX, src);
            self.a.mov_store8(Mem { base: RDX, disp: 1 }, RAX);
        } else {
            self.a.mov8_imm(Mem { base: RDX, disp: 0 }, TAG_NUM);
            self.a.movsd_store(Mem { base: RDX, disp: 8 }, src);
        }
    }

    /// The key and row slots of the row cache of the read at `ip`, when the loop keeps one (see `row_caches`).
    fn row_slots(&self, ip: usize) -> Option<(Mem, Mem)> {
        let j = self.lp.row_caches.iter().position(|&b| b == ip)?;
        Some((row_cache_mem(j, 0), row_cache_mem(j, 8)))
    }

    /// Makes r8 the row at the whole key in xmm `key` of the array in r8, as `elem_row` does, and keeps the key and the row
    /// in the slots of the array's cache. A key that the cache holds takes its row from there, without the bounds and tag
    /// checks, which passed when the key was stored. The rows of the array do not change while the loop runs: an element
    /// store exits before it overwrites a reference (see `elem_write`), and a loop keeps no cache for an array it rebinds
    /// (see `plan`). An empty slot holds `NO_KEY`, which no key takes.
    fn row_cached(&mut self, key: u8, key_slot: Mem, row_slot: Mem, array: Mem, exit: Label) {
        let (miss, done) = (self.a.label(), self.a.label());
        self.a.cvttsd2si(RCX, key);
        self.a.mov_load(RAX, key_slot);
        self.a.cmp_rr64(RCX, RAX);
        self.a.jcc(CC_NE, miss);
        self.a.mov_load(R8, row_slot);
        self.a.jmp(done);
        self.a.bind(miss);
        self.a.mov_load(R8, array);
        self.elem_row(key, true, exit);
        self.a.mov_store(key_slot, RCX);
        self.a.mov_store(row_slot, R8);
        self.a.bind(done);
    }

    /// Makes r8 the array that element `key` of the array in r8 holds. An element that is not an array jumps to `exit`.
    fn elem_row(&mut self, key: u8, whole: bool, exit: Label) {
        self.elem_addr(key, whole, exit);
        self.a.movzx_load8(RAX, Mem { base: RDX, disp: 0 });
        self.a.cmp64_imm8(RAX, TAG_ARR);
        self.a.jcc(CC_NE, exit);
        self.a.mov_load(R8, Mem { base: RDX, disp: 8 });
    }

    /// The register that holds the number `bits`: its hoisted register, or `scratch`, which is loaded
    /// with it.
    /// The value slot of operand `k`, in the value stack, where an owned operand is kept (see `Mark::Owned`).
    fn operand_mem(&self, k: i32) -> Mem {
        local_mem((self.proto.nlocals as i32 + k) as u32)
    }

    /// Copies the 16-byte value at `src` to `dst`, without counting it.
    fn copy_value(&mut self, src: Mem, dst: Mem) {
        self.a.mov_load(RAX, src);
        self.a.mov_load(RCX, src.at(8));
        self.a.mov_store(dst, RAX);
        self.a.mov_store(dst.at(8), RCX);
    }

    /// Counts one more reference to the value at `m`, when it owns one.
    fn count_up(&mut self, m: Mem) {
        let plain = self.a.label();
        self.a.movzx_load8(R9, m);
        self.a.mov_imm32(R10, OWNING);
        self.a.bt(R10, R9);
        self.a.jcc(CC_AE, plain);
        self.a.mov_load(R11, m.at(8));
        self.a.add64_mem_imm8(Mem { base: R11, disp: 0 }, 1);
        self.a.bind(plain);
    }

    /// Jumps to `exit` when the value at `m` owns the last reference, which releasing it would free.
    fn check_last(&mut self, m: Mem, exit: Label) {
        let plain = self.a.label();
        self.a.movzx_load8(R9, m);
        self.a.mov_imm32(R10, OWNING);
        self.a.bt(R10, R9);
        self.a.jcc(CC_AE, plain);
        self.a.mov_load(R11, m.at(8));
        self.a.cmp64_mem_imm8(Mem { base: R11, disp: 0 }, 1);
        self.a.jcc(CC_BE, exit);
        self.a.bind(plain);
    }

    /// Releases the reference of the value at `m`, when it owns one, after `check_last` has ruled out the last one. Both
    /// steps share one check of the owning tag, so that a store of an object local takes one pass over the old value.
    fn release_checked(&mut self, m: Mem, exit: Label) {
        let plain = self.a.label();
        self.a.movzx_load8(R9, m);
        self.a.mov_imm32(R10, OWNING);
        self.a.bt(R10, R9);
        self.a.jcc(CC_AE, plain);
        self.a.mov_load(R11, m.at(8));
        self.a.cmp64_mem_imm8(Mem { base: R11, disp: 0 }, 1);
        self.a.jcc(CC_BE, exit);
        self.a.add64_mem_imm8(Mem { base: R11, disp: 0 }, -1);
        self.a.bind(plain);
    }

    /// Releases the reference of the value at `m`, when it owns one. It must not be the last one (see `check_last`).
    fn count_down(&mut self, m: Mem) {
        let plain = self.a.label();
        self.a.movzx_load8(R9, m);
        self.a.mov_imm32(R10, OWNING);
        self.a.bt(R10, R9);
        self.a.jcc(CC_AE, plain);
        self.a.mov_load(R11, m.at(8));
        self.a.add64_mem_imm8(Mem { base: R11, disp: 0 }, -1);
        self.a.bind(plain);
    }

    /// Swaps the 16-byte values in two slots.
    fn swap_values(&mut self, x: Mem, y: Mem) {
        self.a.mov_load(RAX, x);
        self.a.mov_load(RCX, x.at(8));
        self.a.mov_load(RSI, y);
        self.a.mov_load(RDI, y.at(8));
        self.a.mov_store(x, RSI);
        self.a.mov_store(x.at(8), RDI);
        self.a.mov_store(y, RAX);
        self.a.mov_store(y.at(8), RCX);
    }

    /// The property site of name `n`: its hint and the layout of properties.
    fn prop_site(&self, n: u32) -> PropSite {
        PropSite {
            hint: &self.proto.hints[n as usize] as *const PropHint as usize,
            layout: prop_layout().expect("the planner admits a property only with a known layout"),
        }
    }

    /// Whether r14 may hold a property's address. A loop saves r14 on entry. A batch keeps its index in r14 while it
    /// calls a plain function's body, which writes no property, so only a method or a closure body may write r14.
    fn r14_free(&self) -> bool {
        self.lp
            .function
            .as_ref()
            .map_or(true, |f| f.receiver || f.upvals)
    }

    /// Whether the property that the `GetProp` at `at` read is still where that read found it, at `ip`, where
    /// the `SetProp` that matches it runs. The instructions between them run straight through, run no script
    /// code, write no property and leave the object's local alone, and none of them can be entered from elsewhere.
    fn cache_holds(&self, at: usize, ip: usize, view: Mark) -> bool {
        let view_slot = match view {
            Mark::View(s) => Some(s),
            _ => None,
        };
        // The store itself is a resume point, which is no reason to refuse it: a jump to it is what would break the address.
        (at + 1..=ip).all(|k| {
            !self.is_jump_target(k)
                && (k == ip
                    || (!self.lp.resumes.contains(&k) && window_op(self.proto.code[k], view_slot)))
        })
    }

    /// Whether the element read at `ip` is stored straight into an object local: it is one of the reads that the loop
    /// keeps as objects (see `Region::object_reads`), and the store after it is not a jump target, so nothing reaches
    /// the store without the read.
    fn binds_element(&self, ip: usize) -> bool {
        self.lp.object_reads.contains(&ip)
            && matches!(self.proto.code.get(ip + 1), Some(Op::PopToLocal(i)) if self.lp.objects.contains(i))
            && !self.is_jump_target(ip + 1)
    }

    /// Whether an instruction other than the one before `k` jumps to `k`.
    fn is_jump_target(&self, k: usize) -> bool {
        self.proto
            .code
            .iter()
            .enumerate()
            .any(|(j, &op)| j + 1 != k && successors(op, j, 0).iter().any(|&(t, _)| t == k))
    }

    /// The memory of the object that a property operand reads in place: a local's slot, or a method's receiver.
    fn view_mem(&self, m: Mark) -> Mem {
        match m {
            Mark::View(s) => local_mem(s),
            Mark::Receiver => self.receiver_home(),
            _ => unreachable!("a property is read only of a view or of the receiver (see `step` and `function_step`)"),
        }
    }

    /// The property cache of `name` on the object local `local`, when the loop keeps one (see `plan`).
    fn prop_cache_slot(&self, local: u32, name: u32) -> Option<Mem> {
        let j = self
            .lp
            .prop_caches
            .iter()
            .position(|&c| c == (local, name))?;
        Some(prop_cache_mem(j))
    }

    /// Leaves in RDX the address of property `name` of the object at `obj`, which is the object local `local` when it is
    /// one. A cached address is the one that the lookup found before: the loop adds no property to an object, and the
    /// values of an object stay in place until one is added, so the address holds while the local keeps its object (see
    /// `clear_prop_caches`). A call can change an object, which `check_structure` notices after the call.
    fn prop_address(&mut self, obj: Mem, local: Option<u32>, name: u32, exit: Label) {
        let site = self.prop_site(name);
        match local.and_then(|s| self.prop_cache_slot(s, name)) {
            Some(slot) => {
                let done = self.a.label();
                self.a.mov_load(RDX, slot);
                self.a.test64(RDX, RDX);
                self.a.jcc(CC_NE, done);
                find_prop(&mut *self.a, obj, site, exit);
                if !self.lp.calls {
                    // Without a call, a number stays a number under the loop's writes, which store only numbers over
                    // numbers. So the check is made once, here, and a hit does not repeat it (see `prop_read`).
                    self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_NUM);
                    self.a.jcc(CC_NE, exit);
                }
                self.a.mov_store(slot, RDX);
                self.a.bind(done);
            }
            None => find_prop(&mut *self.a, obj, site, exit),
        }
    }

    /// Loads the count of structural changes into `dst` (see `epoch_address`).
    fn epoch_into(&mut self, dst: u8) {
        self.a.mov_imm64(dst, epoch_address());
        self.a.mov_load(dst, Mem { base: dst, disp: 0 });
    }

    /// Drops the property caches when the count of structural changes has moved since they were last checked. Script code
    /// that a call runs is the only code in a loop that can change an object, so the count is checked after each call.
    fn check_structure(&mut self) {
        if self.lp.prop_caches.is_empty() {
            return;
        }
        let done = self.a.label();
        self.epoch_into(RAX);
        self.a.mov_load(RSI, prop_epoch_mem());
        self.a.cmp_rr64(RAX, RSI);
        self.a.jcc(CC_E, done);
        self.a.mov_store(prop_epoch_mem(), RAX);
        self.a.mov_imm64(RAX, 0);
        for j in 0..self.lp.prop_caches.len() {
            self.a.mov_store(prop_cache_mem(j), RAX);
        }
        self.a.bind(done);
    }

    /// Forgets every row cache. Script code may have replaced a row of an array, so the row caches of the loop are wrong
    /// after it (see `call_method`). The property caches check the count of structural changes instead.
    fn forget_row_caches(&mut self) {
        for j in 0..self.lp.row_caches.len() {
            self.a.mov_imm64(RAX, NO_KEY);
            self.a.mov_store(row_cache_mem(j, 0), RAX);
        }
    }

    /// Clears the property caches of the object local `local`, which the loop has just rebound: they hold the addresses of
    /// the values of the object that the local held before.
    fn clear_prop_caches(&mut self, local: u32) {
        let slots: Vec<Mem> = (0..self.lp.prop_caches.len())
            .filter(|&j| self.lp.prop_caches[j].0 == local)
            .map(prop_cache_mem)
            .collect();
        if !slots.is_empty() {
            self.a.mov_imm64(RAX, 0);
        }
        for slot in slots {
            self.a.mov_store(slot, RAX);
        }
    }

    /// Reads the number that property `n` of the object at `obj` holds into xmm `dst`. A property that is not a number,
    /// or an object that the hint does not find, exits at `ip`, which the generic code runs, with `d` operands on the
    /// stack.
    fn prop_read(&mut self, ip: usize, d: i32, dst: u8, obj: Mem, n: u32, local: Option<u32>) {
        let exit = self.side_exit(ip, d);
        self.prop_address(obj, local, n, exit);
        if self.r14_free() {
            self.a.mov_rr(R14, RDX);
        }
        // The check of a cached address was made when the cache was filled, and it still holds (see `prop_address`).
        let checked = !self.lp.calls && local.is_some_and(|s| self.prop_cache_slot(s, n).is_some());
        if !checked {
            self.a.cmp8_imm(Mem { base: RDX, disp: 0 }, TAG_NUM);
            self.a.jcc(CC_NE, exit);
        }
        self.a.movsd_load(dst, Mem { base: RDX, disp: 8 });
    }

    fn constant(&mut self, bits: u64, scratch: u8) -> u8 {
        let lp = self.lp;
        match lp.consts.iter().find(|&&(b, _)| b == bits) {
            Some(&(_, x)) => x,
            None => {
                self.a.mov_imm64(RAX, bits);
                self.a.movq_to_xmm(scratch, RAX);
                scratch
            }
        }
    }

    /// `local++` or `local--`, by `sse`, which `delta` (1 or -1) describes. The integer copy takes the
    /// same step, and a step that takes it out of range leaves the loop after the step, at the next
    /// instruction.
    fn step_counter(&mut self, i: u32, sse: u8, delta: i8, ip: usize) {
        let one = self.constant(1f64.to_bits(), 1);
        self.update_local(i, sse, one);
        if self.lp.int_local == Some(i) {
            self.a.add64_imm8(R13, delta);
            if let Some(c) = self.lp.rem {
                // The remainder steps with the copy: it goes up by one, and starts again at zero at c.
                self.a.mov_imm32(RDX, 0);
                self.a.add64_imm8(R15, 1);
                self.a.cmp64_imm32(R15, c as i32);
                self.a.cmov(CC_E, R15, RDX);
            }
            let target = self.exit_label(ip + 1);
            if delta > 0 {
                self.a.cmp64_imm32(R13, INT_LIMIT);
                self.a.jcc(CC_G, target);
            } else {
                self.a.cmp64_imm32(R13, -INT_LIMIT);
                self.a.jcc(CC_L, target);
            }
        }
    }

    /// `r13 % c` into `dst`, for the integer copy of a local. The magnitude goes through the reciprocal
    /// of `c`, and the sign bit of the copy becomes the sign of the result, so that a zero remainder of
    /// a negative copy is -0.
    fn mod_shadow(&mut self, c: u32, dst: u8) {
        let (m, s) = magic(c);
        self.a.mov_rr(RCX, R13);
        self.a.mov_rr(R8, RCX);
        self.a.neg64(R8);
        self.a.cmovs(R8, RCX);
        self.a.mov_rr(R9, R8);
        self.a.mov_imm64(R10, m);
        self.a.imul64(R8, R10);
        self.a.shr64_imm(R8, s);
        self.a.imul64_imm(R8, R8, c as i32);
        self.a.sub64(R9, R8);
        self.a.mov_rr(RDX, RCX);
        self.a.shr64_imm(RDX, 63);
        self.a.shl64_imm(RDX, 63);
        self.a.movq_to_xmm(0, RDX);
        // cvtsi2sd keeps the destination's old upper bits, so zeroing it first breaks a false dependency.
        self.a.xorpd(dst, dst);
        self.a.cvtsi2sd64(dst, R9);
        self.a.xorpd(dst, 0);
    }

    /// `local op k` into `dst`.
    fn with_constant(&mut self, i: u32, k: u32, sse: u8, dst: u8) {
        self.read_local(i, dst);
        let c = self.constant(number(self.proto, k).to_bits(), 0);
        self.a.sse(sse, dst, c);
    }

    /// `local % k` into `dst`. An integer divisor takes the reciprocal path, and a local that is not
    /// an integer takes `js_mod`, which is the only path for a divisor that is not an integer.
    fn mod_constant(&mut self, i: u32, k: u32, dst: u8) {
        let c = number(self.proto, k);
        if self.lp.int_local == Some(i) {
            if let Some(n) = int_divisor(c) {
                if self.lp.rem == Some(n) {
                    // The remainder is kept in r15 for a copy that is not negative (see `step_counter`).
                    self.a.xorpd(dst, dst);
                    self.a.cvtsi2sd64(dst, R15);
                } else {
                    self.mod_shadow(n, dst);
                }
                return;
            }
        }
        let src = self.local_src(i, 1);
        let (slow, join) = (self.a.label(), self.a.label());
        match int_divisor(c) {
            Some(n) => emit_mod_const(self.a, n, src, dst, slow, join, self.lp.whole[i as usize]),
            None => {
                self.a.jmp(slow);
                self.a.bind(join);
            }
        }
        self.slow.push((slow, join, dst, src, c));
    }

    /// The integer divisor `n` and its value `c` when the `%` at `ip` divides by a number constant that the instruction
    /// before it pushed. No other instruction may jump to `ip`, or the right operand could come from somewhere else.
    fn magic_divisor(&self, ip: usize) -> Option<(u32, f64)> {
        if ip == 0 || ip - 1 < self.lp.header {
            return None;
        }
        let k = match self.proto.code[ip - 1] {
            Op::Const(k) => k,
            _ => return None,
        };
        let c = match self.proto.consts.get(k as usize) {
            Some(Value::Num(c)) => *c,
            _ => return None,
        };
        let n = int_divisor(c)?;
        let jumped = self
            .proto
            .code
            .iter()
            .enumerate()
            .any(|(j, &op)| j != ip - 1 && successors(op, j, 0).iter().any(|&(t, _)| t == ip));
        (!jumped).then_some((n, c))
    }

    /// `top % c` in place, for the integer divisor `n` of `c` (see `magic_divisor`).
    fn mod_top(&mut self, top: u8, n: u32, c: f64, whole: bool) {
        let (slow, join) = (self.a.label(), self.a.label());
        emit_mod_const(self.a, n, top, top, slow, join, whole);
        self.slow.push((slow, join, top, top, c));
    }

    /// Sets xmm0 to 0.0 and compares `x` with it, so that ZF is set when `x` is falsy: zero, or NaN.
    fn zero_test(&mut self, x: u8) {
        self.a.xorpd(0, 0);
        self.a.ucomisd(x, 0);
    }

    /// Writes 1.0 to `dst` if condition `cc` holds on the flags, else 0.0. `mov` leaves the flags alone.
    fn set(&mut self, dst: u8, cc: u8) {
        self.a.mov_imm32(RAX, 0);
        self.a.setcc(cc, RAX);
        self.a.cvtsi2sd(dst, RAX);
    }

    /// Writes `dst` as 1.0 when the compared numbers are equal, or 0.0. Equal means ZF without PF, so
    /// a NaN is never equal. For `equal == false`, it is the negation.
    fn set_equal(&mut self, dst: u8, equal: bool) {
        self.a.mov_imm32(RAX, 0);
        self.a.mov_imm32(RCX, 0);
        if equal {
            self.a.setcc(CC_E, RAX);
            self.a.setcc(CC_NP, RCX);
            self.a.and8(RAX, RCX);
        } else {
            self.a.setcc(CC_NE, RAX);
            self.a.setcc(CC_P, RCX);
            self.a.or8(RAX, RCX);
        }
        self.a.cvtsi2sd(dst, RAX);
    }

    /// Jumps to instruction `t` when condition `cc` holds.
    fn jump_if(&mut self, t: u32, cc: u8) {
        let target = self.target(t);
        self.a.jcc(cc, target);
    }

    /// Calls `js_mod` on xmm0 and xmm1, and stores the result in `dst`. The call clobbers every xmm
    /// register, so the others are saved on the stack. `dst` is left out, since it takes the result.
    fn call_mod(&mut self, dst: u8) {
        let saved: Vec<u8> = (2..16).filter(|&r| r != dst).collect();
        for (k, &r) in saved.iter().enumerate() {
            self.a.movsd_store(js_spill(k), r);
        }
        self.a
            .mov_imm64(RAX, rt::js_mod as *const () as usize as u64);
        self.a.call_reg(RAX);
        self.a.movapd_rr(dst, 0);
        for (k, &r) in saved.iter().enumerate() {
            self.a.movsd_load(r, js_spill(k));
        }
    }

    /// Stores every local's number back to its frame slot, and marks the slot as a number.
    fn writeback(&mut self) {
        let lp = self.lp;
        for &(i, home) in &lp.locals {
            if let Home::Reg(x) = home {
                self.a.mov8_imm(local_mem(i), TAG_NUM);
                self.a.movsd_store(local_mem(i).at(8), x);
            }
        }
    }

    /// The stub that leaves a loop at instruction `t`, which is one of its exits.
    fn exit_label(&self, t: usize) -> Label {
        let k = self
            .lp
            .exits
            .iter()
            .position(|&e| e == t)
            .expect("every exit is planned");
        self.exits[k]
    }

    /// The label of instruction `t`: its code inside the region, or the stub that leaves a loop.
    fn target(&self, t: u32) -> Label {
        let lp = self.lp;
        let t = t as usize;
        if (lp.header..=lp.last).contains(&t) {
            self.at[t - lp.header]
        } else {
            let k = lp
                .exits
                .iter()
                .position(|&e| e == t)
                .expect("every exit is planned");
            self.exits[k]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::codegen::depths;
    use crate::jit::types::number_slots;
    use std::rc::Rc;

    fn proto(code: Vec<Op>, consts: Vec<Value>, nlocals: u32, nparams: u32) -> Proto {
        Proto {
            name: String::new(),
            file: Rc::from(""),
            lines: vec![0; code.len()],
            code,
            consts,
            names: Vec::new(),
            hints: Vec::new(),
            global_hints: Vec::new(),
            call_entry: Default::default(),
            call_frame_len: Default::default(),
            call_argc: std::cell::Cell::new(u32::MAX),
            numeric_entry: Default::default(),
            numeric_argc: std::cell::Cell::new(u32::MAX),
            numeric_bool: Default::default(),
            method_entry: Default::default(),
            method_argc: std::cell::Cell::new(u32::MAX),
            closure_entry: Default::default(),
            closure_argc: std::cell::Cell::new(u32::MAX),
            fast_mask: std::cell::Cell::new(0),
            closure_bool: Default::default(),
            factory: Default::default(),
            protos: Vec::new(),
            upvals: Vec::new(),
            nparams,
            has_rest: false,
            nlocals,
            is_arrow: false,
            native: Default::default(),
        }
    }

    fn analysis(p: &Proto, params: usize) -> (Vec<i32>, Numbers) {
        let (depth, deepest) = depths(p).unwrap();
        let types = number_slots(p, &depth, deepest, params);
        (depth, types)
    }

    fn plan_at(p: &Proto, header: usize) -> Option<Region> {
        let (depth, types) = analysis(p, 0);
        plan(p, &depth, &types, header)
    }

    /// `for (let j = 0; j < n; j++) a[j] = false`, with `a` in local 0, `n` in local 1 and `j` in local 2. `store` is the
    /// operation that stores the value, and `after` runs in place of the `Pop` that follows it.
    fn fill_loop(store: Op, after: Op) -> Proto {
        proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(2),
                Op::GetLocal(2),
                Op::GetLocal(1),
                Op::JumpIfNotLt(11),
                Op::GetLocal(2),
                Op::False,
                store,
                after,
                Op::IncLocal(2),
                Op::Jump(2),
                Op::Return,
            ],
            vec![Value::Num(0.0)],
            3,
            2,
        )
    }

    #[test]
    fn an_array_store_in_a_loop_is_planned_with_the_array_in_its_frame_slot() {
        let region =
            plan_at(&fill_loop(Op::SetIndexLocal(0), Op::Pop), 2).expect("the loop is planned");
        assert_eq!(region.arrays, vec![0]);
        assert!(
            region.locals.iter().all(|&(i, _)| i != 0),
            "the array has no register"
        );
    }

    #[test]
    fn an_array_the_loop_assigns_is_not_planned() {
        assert!(plan_at(&fill_loop(Op::SetIndexLocal(0), Op::PopToLocal(0)), 2).is_none());
    }

    #[test]
    fn an_element_is_not_compared_strictly_with_a_boolean() {
        // `while (j < n) { if (a[j] === false) ...; j++ }`: the element may be a number, and strict equality
        // tells the two apart.
        let p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(2),
                Op::GetLocal(2),
                Op::GetLocal(1),
                Op::JumpIfNotLt(12),
                Op::GetLocal(2),
                Op::GetIndexLocal(0),
                Op::False,
                Op::StrictEq,
                Op::JumpIfFalse(10),
                Op::IncLocal(2),
                Op::Jump(2),
                Op::Return,
            ],
            vec![Value::Num(0.0)],
            3,
            2,
        );
        assert!(plan_at(&p, 2).is_none());
    }

    #[test]
    fn a_counting_loop_is_planned_with_its_locals_in_registers() {
        // `sum = 0; i = 0; do { sum += i % 7; i++ } while (i < 20)`, which the compiler lays out with
        // its test at the bottom and a jump into the test.
        let p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Const(0),
                Op::PopToLocal(1),
                Op::Jump(8),
                Op::ModLK(1, 2),
                Op::AddToLocal(0),
                Op::IncLocal(1),
                Op::JumpIfLtLK(1, 1, 5),
                Op::Return,
            ],
            vec![Value::Num(0.0), Value::Num(20.0), Value::Num(7.0)],
            2,
            0,
        );
        let lp = plan_at(&p, 5).expect("the loop is planned");
        assert_eq!(lp.entries, vec![5, 8]);
        // The step of `i` leaves the loop at 8 when its integer copy goes out of range.
        assert_eq!(lp.exits, vec![9, 8]);
        assert_eq!(lp.int_local, Some(1));
        assert!(matches!(
            lp.locals[..],
            [(1, Home::Reg(2)), (0, Home::Reg(3))]
        ));
        // The loop's constants are 1.0 and 20.0, and they take the top registers.
        assert_eq!(lp.consts, vec![(1f64.to_bits(), 15), (20f64.to_bits(), 14)]);
        assert!(lp.guards.is_empty());
        // The remainder by 7 is kept in r15, and steps with the counter.
        assert_eq!(lp.rem, Some(7));
    }

    #[test]
    fn a_remainder_is_not_kept_for_two_divisors_or_a_counter_that_steps_down() {
        // The same loop, with `i % 5` as well, which takes the copy by another divisor.
        let two = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Const(0),
                Op::PopToLocal(1),
                Op::Jump(10),
                Op::ModLK(1, 2),
                Op::AddToLocal(0),
                Op::ModLK(1, 3),
                Op::AddToLocal(0),
                Op::IncLocal(1),
                Op::JumpIfLtLK(1, 1, 5),
                Op::Return,
            ],
            vec![
                Value::Num(0.0),
                Value::Num(20.0),
                Value::Num(7.0),
                Value::Num(5.0),
            ],
            2,
            0,
        );
        assert_eq!(plan_at(&two, 5).expect("the loop is planned").rem, None);
        // The same loop, with the counter stepping down.
        let down = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Const(0),
                Op::PopToLocal(1),
                Op::Jump(8),
                Op::ModLK(1, 2),
                Op::AddToLocal(0),
                Op::DecLocal(1),
                Op::JumpIfLtLK(1, 1, 5),
                Op::Return,
            ],
            vec![Value::Num(0.0), Value::Num(20.0), Value::Num(7.0)],
            2,
            0,
        );
        assert_eq!(plan_at(&down, 5).expect("the loop is planned").rem, None);
    }

    #[test]
    fn a_local_that_may_not_be_a_number_gets_a_guard() {
        // `x = undefined; i = 0; do { x++; i++ } while (i < 10)`.
        let p = proto(
            vec![
                Op::Undef,
                Op::PopToLocal(2),
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Jump(7),
                Op::IncLocal(2),
                Op::IncLocal(0),
                Op::JumpIfLtLK(0, 1, 5),
                Op::Return,
            ],
            vec![Value::Num(0.0), Value::Num(10.0)],
            3,
            0,
        );
        let lp = plan_at(&p, 5).expect("the loop is planned");
        assert_eq!(lp.guards, vec![2]);
        assert!(matches!(
            lp.locals[..],
            [(0, Home::Reg(2)), (2, Home::Reg(3))]
        ));
    }

    #[test]
    fn a_whole_local_the_loop_does_not_use_is_not_checked_on_entry() {
        // `for (j = 0; j < n; j++) {}`, with an array in local 2 that only the code after the loop reads. The array is
        // live on entry, but the loop never touches it, so its frame slot, which holds an array, is not checked.
        let p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::GetLocal(0),
                Op::GetLocal(1),
                Op::JumpIfNotLt(8),
                Op::IncLocal(0),
                Op::Jump(2),
                Op::Return,
                Op::GetLocal(2),
                Op::Return,
            ],
            vec![Value::Num(0.0)],
            3,
            0,
        );
        let lp = plan_at(&p, 2).expect("the loop is planned");
        assert_eq!(lp.whole_guards, vec![0, 1]);
    }

    #[test]
    fn an_array_whose_rows_the_loop_only_reads_keeps_a_row_cache() {
        // `for (j = 0; j < 4; j++) s = a[j][j]`, with `a` in local 0, `j` in local 1 and `s` in local 2.
        let p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(1),
                Op::GetLocal(1),
                Op::GetLocal(1),
                Op::GetIndex2Local(0),
                Op::PopToLocal(2),
                Op::IncLocal(1),
                Op::JumpIfLtLK(1, 1, 2),
                Op::Return,
            ],
            vec![Value::Num(0.0), Value::Num(4.0)],
            3,
            0,
        );
        // The read `a[j][j]` is the `GetIndex2Local` at instruction 4, which keeps the cache.
        assert_eq!(
            plan_at(&p, 2).expect("the loop is planned").row_caches,
            vec![4]
        );
    }

    #[test]
    fn a_loop_that_reads_a_property_of_an_object_keeps_its_address() {
        // `for (j = 0; j < 4; j++) s = o.x`, with `o` in local 0, `j` in local 1 and `s` in local 2, and `x` the name 0.
        let mut p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(1),
                Op::GetLocalProp(0, 0),
                Op::PopToLocal(2),
                Op::IncLocal(1),
                Op::JumpIfLtLK(1, 1, 2),
                Op::Return,
            ],
            vec![Value::Num(0.0), Value::Num(4.0)],
            3,
            0,
        );
        p.names = vec![Rc::from("x")];
        assert_eq!(
            plan_at(&p, 2).expect("the loop is planned").prop_caches,
            vec![(0, 0)]
        );
    }

    #[test]
    fn loops_with_work_outside_numbers_are_not_planned() {
        // Undefined is not a number, so the loop is not planned.
        let undef = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Undef,
                Op::PopToLocal(1),
                Op::Jump(2),
            ],
            vec![Value::Num(0.0)],
            2,
            0,
        );
        assert!(plan_at(&undef, 2).is_none());
        // A comparison that is not consumed by a jump would leave a boolean in a register.
        let stored = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::GetLocal(0),
                Op::Const(0),
                Op::Lt,
                Op::PopToLocal(1),
                Op::Jump(2),
            ],
            vec![Value::Num(0.0)],
            2,
            0,
        );
        assert!(plan_at(&stored, 2).is_none());
    }

    /// `function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); }`, as the compiler
    /// lays it out: the function is in upvalue 0, and `this` is `undefined` for a plain call.
    fn fib() -> Proto {
        proto(
            vec![
                Op::JumpIfNotLtLK(0, 0, 4),
                Op::GetLocal(0),
                Op::Return,
                Op::Undef,
                Op::GetUpval(0),
                Op::Undef,
                Op::GetLocal(0),
                Op::Const(1),
                Op::Sub,
                Op::Call(1),
                Op::GetUpval(0),
                Op::Undef,
                Op::GetLocal(0),
                Op::Const(0),
                Op::Sub,
                Op::Call(1),
                Op::Add,
                Op::Return,
            ],
            vec![Value::Num(2.0), Value::Num(1.0)],
            1,
            1,
        )
    }

    #[test]
    fn a_recursive_number_function_is_planned_with_its_self_reference() {
        let p = fib();
        let (depth, types) = analysis(&p, 1);
        let lp = plan_function(&p, &depth, &types).expect("fib is planned");
        let function = lp.function.expect("a function region");
        assert_eq!((function.nparams, function.self_upval), (1, Some(0)));
    }

    #[test]
    fn a_function_that_may_return_undefined_is_not_planned() {
        // `function f(n) { if (n < 2) return; return n }`, which returns undefined on one path.
        let p = proto(
            vec![
                Op::JumpIfNotLtLK(0, 0, 3),
                Op::Undef,
                Op::Return,
                Op::GetLocal(0),
                Op::Return,
            ],
            vec![Value::Num(2.0)],
            1,
            1,
        );
        let (depth, types) = analysis(&p, 1);
        assert!(plan_function(&p, &depth, &types).is_none());
    }

    /// A function that pushes `pushes` copies of a constant and then adds them up, so its deepest operand
    /// stack is `pushes` deep, at the first `Add`.
    fn deep_sum(pushes: usize) -> Proto {
        let mut code = vec![Op::Const(0); pushes];
        code.extend(std::iter::repeat_n(Op::Add, pushes - 1));
        code.push(Op::Return);
        proto(code, vec![Value::Num(1.0)], 1, 1)
    }

    #[test]
    fn a_function_is_planned_up_to_the_deepest_operand_registers() {
        // Operands sit in xmm2 up to xmm15 in a function, so a depth of 13 is the most it takes.
        let p = deep_sum(13);
        let (depth, types) = analysis(&p, 1);
        assert!(plan_function(&p, &depth, &types).is_some());
        let p = deep_sum(14);
        let (depth, types) = analysis(&p, 1);
        assert!(plan_function(&p, &depth, &types).is_none());
    }
}
