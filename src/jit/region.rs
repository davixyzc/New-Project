//! Code that works on numbers only, with its locals in registers.
//!
//! The generic code keeps each local in its frame slot and loads and stores it on every step. Two
//! kinds of region avoid that for code that cannot run user code or touch the heap: numbers, locals,
//! arithmetic, comparisons and jumps.
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
    emit_mod_const, int_divisor, local_uses, magic, successors, TAG_BOOL, TAG_NUM,
};
use super::rt;
use super::types::Numbers;
use super::x64::*;
use super::Batch;
use crate::bytecode::{Op, Proto};
use crate::value::Value;
use std::mem::offset_of;

/// The xmm registers that hold locals of a loop, from xmm2 up. xmm0 and xmm1 are scratch.
const LOCAL_REGS: std::ops::Range<u8> = 2..8;
/// The xmm register of the operand at depth 0. Operand `k` is in `TEMP_BASE + k`, up to xmm15.
const TEMP_BASE: u8 = 8;
/// The deepest operand stack a region handles, which leaves the registers above it for constants.
const MAX_DEPTH: i32 = 7;
/// Arguments a function region takes, in xmm0 and up.
pub(super) const MAX_ARGS: usize = 6;
/// The bytes of a loop region's frame, below the saved `r13`: room to save the xmm registers a call
/// clobbers, and a size that keeps rsp 16-byte aligned at those calls.
const FRAME: i32 = 136;
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

/// What a function region needs to know about its function.
pub(super) struct Function {
    pub(super) nparams: u32,
    /// The upvalue that the function calls itself through, if it does. Native code calls the
    /// function's own code directly, which is only right if that upvalue holds the function.
    pub(super) self_upval: Option<u32>,
    /// Whether the function returns a boolean, which native code returns as 0.0 or 1.0.
    pub(super) returns_bool: bool,
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
    /// Set for a function region.
    pub(super) function: Option<Function>,
}

/// What an operand is, in the stack that `plan_function` follows.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    /// A number, in the operand's register.
    Num,
    /// A boolean, from a comparison or `!`, held as 0.0 or 1.0. Only a jump or a return takes it.
    Bool,
    /// The function, pushed by `GetUpval`. Only a call takes it, and it has no register.
    Callee,
    /// The `this` of a plain call, which is `undefined`. Only a call takes it, and it has no register.
    This,
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
    let mut entries = vec![header];
    let mut exits = Vec::new();
    let mut uses = vec![0u32; proto.nlocals as usize];
    let mut other_writes = vec![false; proto.nlocals as usize];
    let mut mod_uses = vec![0u32; proto.nlocals as usize];
    // The divisor of the first `local % c` on each local, and whether a later one differs or the local is
    // decremented, which keeps a remainder from stepping with it.
    let mut mod_divisor = vec![0u32; proto.nlocals as usize];
    let mut mod_mixed = vec![false; proto.nlocals as usize];
    let mut decremented = vec![false; proto.nlocals as usize];
    let mut reads = Vec::new();
    let mut deepest = 0;
    let mut wanted: Vec<u64> = Vec::new();
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
        if is_test(op) && !consumed(code, ip) {
            return None;
        }
        if d > MAX_DEPTH {
            return None;
        }
        deepest = deepest.max(d + 1);
        if let Some(bits) = constant_of(proto, op) {
            if !wanted.contains(&bits) {
                wanted.push(bits);
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
        for i in local_uses(op) {
            uses[i as usize] += 1;
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
        reads.extend(reads_of(op).into_iter().map(|i| (ip, i)));
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
        .filter(|&i| uses[i as usize] > 0)
        .collect();
    if order.len() > LOCAL_REGS.len() {
        return None;
    }
    order.sort_by_key(|&i| (std::cmp::Reverse(uses[i as usize]), i));
    let locals: Vec<(u32, Home)> = order
        .into_iter()
        .zip(LOCAL_REGS)
        .map(|(i, x)| (i, Home::Reg(x)))
        .collect();
    // Constants take registers from the top down, which must not reach the operand registers.
    let fits = TEMP_BASE as usize + deepest as usize + wanted.len() <= 16;
    let consts: Vec<(u64, u8)> = if fits {
        wanted
            .iter()
            .enumerate()
            .map(|(n, &bits)| (bits, (15 - n) as u8))
            .collect()
    } else {
        Vec::new()
    };
    // A read is only safe where the analysis knows the local is a number. Elsewhere the entry must
    // check it.
    let mut guards: Vec<u32> = Vec::new();
    for (ip, i) in reads {
        if !known(types, ip, i) && !guards.contains(&i) {
            guards.push(i);
        }
    }
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
        function: None,
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
    let mut self_upval = None;
    let mut ret_kind = None;
    while let Some(ip) = work.pop() {
        let d = depth[ip];
        if d < 0 || d > MAX_DEPTH {
            return None;
        }
        let op = code[ip];
        let before = states[ip].clone()?;
        let after = function_step(
            proto,
            ip,
            op,
            &before,
            types,
            nparams,
            &mut self_upval,
            &mut ret_kind,
        )?;
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
        function: Some(Function {
            nparams,
            self_upval,
            returns_bool: ret_kind.unwrap_or(false),
        }),
    })
}

/// The operands after `op` runs on the operands `before`, or `None` when `op` is not number work.
fn function_step(
    proto: &Proto,
    ip: usize,
    op: Op,
    before: &[Mark],
    types: &Numbers,
    nparams: u32,
    self_upval: &mut Option<u32>,
    ret_kind: &mut Option<bool>,
) -> Option<Vec<Mark>> {
    let mut s = before.to_vec();
    let top_numbers =
        |k: usize| before.len() >= k && before[before.len() - k..].iter().all(|&m| m == Mark::Num);
    let top_truthy = || matches!(before.last(), Some(Mark::Num | Mark::Bool));
    let number = |k: u32| matches!(proto.consts.get(k as usize), Some(Value::Num(_)));
    let known_at = |i: u32| known(types, ip, i);
    match op {
        Op::Const(k) if number(k) => s.push(Mark::Num),
        Op::Undef => s.push(Mark::This),
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
        Op::JumpIfNotLt(_) | Op::JumpIfNotLe(_) | Op::JumpIfNotGt(_) | Op::JumpIfNotGe(_)
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
        _ => return None,
    }
    Some(s)
}

/// Whether the number the test at `ip` leaves is consumed by the jump after it.
fn consumed(code: &[Op], ip: usize) -> bool {
    matches!(
        code.get(ip + 1),
        Some(Op::JumpIfFalse(_) | Op::JumpIfTrue(_))
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
        Op::JumpIfLtLL(..) | Op::JumpIfNotLtLL(..) => true,
        _ => false,
    }
}

/// The number an instruction uses as an operand, when it has one that is kept in a register.
fn constant_of(proto: &Proto, op: Op) -> Option<u64> {
    let k = match op {
        Op::IncLocal(_) | Op::DecLocal(_) => return Some(1f64.to_bits()),
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

/// The xmm register of the operand at depth `k`.
fn reg(k: i32) -> u8 {
    (TEMP_BASE as i32 + k) as u8
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

/// The stack slot that a call of `js_mod` saves register `k` in.
fn js_spill(k: usize) -> Mem {
    Mem {
        base: RSP,
        disp: 8 * k as i32,
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
    };
    e.a.bind(entry);
    e.a.push_reg(R13);
    e.a.push_reg(R15);
    e.a.sub_rsp32(FRAME);
    for &i in &lp.guards {
        e.a.cmp8_imm(local_mem(i), TAG_NUM);
        e.a.jcc(CC_NE, not_entered);
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
    a.mov_imm32(RAX, 0);
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
        body: Some(body),
        nlocals,
    };
    e.body_ops();
    let a = e.a;
    let (nparams, returns_bool) = (function.nparams, function.returns_bool);
    let map = if nparams <= 2 {
        Some(emit_batch(a, body, Mode::Map, nparams, returns_bool))
    } else {
        None
    };
    let filter = if nparams <= 2 {
        Some(emit_batch(a, body, Mode::Filter, nparams, returns_bool))
    } else {
        None
    };
    // A reduce that returns booleans would hand a boolean to the next call, so it runs in the interpreter.
    let reduce = if nparams <= 3 && !returns_bool {
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
}

impl Emitter<'_, '_> {
    /// Emits the code of every instruction in the region, then the out-of-line paths.
    fn body_ops(&mut self) {
        let lp = self.lp;
        for ip in lp.header..=lp.last {
            let at = self.at[ip - lp.header];
            self.a.bind(at);
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
    }

    /// Emits the stubs that leave a loop, and the return for a guard that failed.
    fn leave_loop(&mut self) {
        let lp = self.lp;
        for (k, &t) in lp.exits.iter().enumerate() {
            let stub = self.exits[k];
            self.a.bind(stub);
            self.writeback();
            self.a.add_rsp32(FRAME);
            self.a.pop_reg(R15);
            self.a.pop_reg(R13);
            self.a.mov_imm32(RAX, t as u32);
            self.a.ret();
        }
        let not_entered = self.not_entered;
        self.a.bind(not_entered);
        self.a.add_rsp32(FRAME);
        self.a.pop_reg(R15);
        self.a.pop_reg(R13);
        self.a.mov_imm64(RAX, NOT_ENTERED);
        self.a.ret();
    }

    /// Emits `op` at instruction `ip`, which runs with `d` operands on the stack.
    fn op(&mut self, op: Op, d: i32, ip: usize) {
        let (x, y) = (reg(d - 1), reg(d - 2));
        match op {
            Op::Const(k) => {
                let c = self.constant(number(self.proto, k).to_bits(), reg(d));
                if c != reg(d) {
                    self.a.movapd_rr(reg(d), c);
                }
            }
            // A function's `this` and callee have no register, and the code of a function starts
            // from nothing, so these take no code.
            Op::Pop | Op::Undef | Op::GetUpval(_) => {}
            Op::Dup => self.a.movapd_rr(reg(d), x),
            Op::Swap => {
                self.a.movapd_rr(0, x);
                self.a.movapd_rr(x, y);
                self.a.movapd_rr(y, 0);
            }
            Op::GetLocal(i) => self.read_local(i, reg(d)),
            Op::SetLocal(i) | Op::PopToLocal(i) => self.write_local(i, x),
            Op::AddToLocal(i) => self.update_local(i, SSE_ADD, x),
            Op::IncLocal(i) => self.step_counter(i, SSE_ADD, 1, ip),
            Op::DecLocal(i) => self.step_counter(i, SSE_SUB, -1, ip),
            Op::AddLK(i, k) => self.with_constant(i, k, SSE_ADD, reg(d)),
            Op::SubLK(i, k) => self.with_constant(i, k, SSE_SUB, reg(d)),
            Op::MulLK(i, k) => self.with_constant(i, k, SSE_MUL, reg(d)),
            Op::ModLK(i, k) => self.mod_constant(i, k, reg(d)),
            Op::Add => self.a.sse(SSE_ADD, y, x),
            Op::Sub => self.a.sse(SSE_SUB, y, x),
            Op::Mul => self.a.sse(SSE_MUL, y, x),
            Op::Div => self.a.sse(SSE_DIV, y, x),
            Op::Mod => {
                self.a.movapd_rr(0, y);
                self.a.movapd_rr(1, x);
                self.call_mod(y);
            }
            Op::Neg => {
                self.a.mov_imm64(RAX, 1 << 63);
                self.a.movq_to_xmm(0, RAX);
                self.a.xorpd(x, 0);
            }
            // `a < b` is `b > a`, and an unordered compare (NaN) sets CF, so the `above` tests are false.
            Op::Lt => {
                self.a.ucomisd(x, y);
                self.set(y, CC_A);
            }
            Op::Le => {
                self.a.ucomisd(x, y);
                self.set(y, CC_AE);
            }
            Op::Gt => {
                self.a.ucomisd(y, x);
                self.set(y, CC_A);
            }
            Op::Ge => {
                self.a.ucomisd(y, x);
                self.set(y, CC_AE);
            }
            Op::Eq | Op::StrictEq => {
                self.a.ucomisd(y, x);
                self.set_equal(y, true);
            }
            Op::Ne | Op::StrictNe => {
                self.a.ucomisd(y, x);
                self.set_equal(y, false);
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
                self.zero_test(x);
                self.jump_if(t, CC_E);
            }
            Op::JumpIfTrue(t) => {
                self.zero_test(x);
                self.jump_if(t, CC_NE);
            }
            Op::JumpIfNotLt(t) => {
                self.a.ucomisd(x, y);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfNotLe(t) => {
                self.a.ucomisd(x, y);
                self.jump_if(t, CC_B);
            }
            Op::JumpIfNotGt(t) => {
                self.a.ucomisd(y, x);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfNotGe(t) => {
                self.a.ucomisd(y, x);
                self.jump_if(t, CC_B);
            }
            Op::JumpIfLtLK(i, k, t) => {
                let local = self.local_src(i, 1);
                let c = self.constant(number(self.proto, k).to_bits(), 0);
                self.a.ucomisd(c, local);
                self.jump_if(t, CC_A);
            }
            Op::JumpIfNotLtLK(i, k, t) => {
                let local = self.local_src(i, 1);
                let c = self.constant(number(self.proto, k).to_bits(), 0);
                self.a.ucomisd(c, local);
                self.jump_if(t, CC_BE);
            }
            Op::JumpIfLtLL(p, q, t) => {
                let a = self.local_src(p, 0);
                let b = self.local_src(q, 1);
                self.a.ucomisd(b, a);
                self.jump_if(t, CC_A);
            }
            Op::JumpIfNotLtLL(p, q, t) => {
                let a = self.local_src(p, 0);
                let b = self.local_src(q, 1);
                self.a.ucomisd(b, a);
                self.jump_if(t, CC_BE);
            }
            Op::Return => {
                self.a.movapd_rr(0, x);
                self.a.mov_rr(RSP, RBP);
                self.a.pop_reg(RBP);
                self.a.ret();
            }
            Op::Call(argc) => self.call(d, argc as i32),
            _ => unreachable!("the planners admit only the operations above"),
        }
    }

    /// A call of the function's own body with `argc` arguments, which are the top operands. The callee
    /// and `this` below them take no registers, and the result replaces them.
    fn call(&mut self, d: i32, argc: i32) {
        let base = d - argc - 2;
        for k in 0..base {
            self.a.movsd_store(self.operand_slot(k), reg(k));
        }
        for j in 0..argc {
            self.a.movapd_rr(j as u8, reg(d - argc + j));
        }
        let body = self
            .body
            .expect("calls are only planned in a function region");
        self.a.call_label(body);
        self.a.movapd_rr(reg(base), 0);
        for k in 0..base {
            self.a.movsd_load(reg(k), self.operand_slot(k));
        }
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
            Home::Mem(m) => self.a.movsd_store(m, src),
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

    /// The register that holds the number `bits`: its hoisted register, or `scratch`, which is loaded
    /// with it.
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
            Some(n) => emit_mod_const(self.a, n, src, dst, slow, join),
            None => {
                self.a.jmp(slow);
                self.a.bind(join);
            }
        }
        self.slow.push((slow, join, dst, src, c));
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
}
