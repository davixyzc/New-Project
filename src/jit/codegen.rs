//! Translates one function's bytecode into x86-64 machine code.
//!
//! The operand stack has a known depth before each instruction (`depths`), so every operand
//! has a fixed address in the frame: local `i` is slot `i`, and the operand at depth `d` is
//! slot `nlocals + d`. Native code reads and writes those slots directly for the common cases
//! (numbers, plain locals, jumps). Every other instruction calls into `rt`, which runs it
//! through the interpreter's own code.
//!
//! Native code never updates the stack's length. A helper sets the length from the depth it
//! is given before it runs, so the interpreter always sees a consistent stack. Slots above
//! that depth are dead, and native code only leaves plain values in them, so forgetting them
//! never skips a drop.

use std::cell::Cell;
use std::rc::Rc;

use super::region;
use super::rt::{self, STATUS_JUMP};
use super::types::{number_slots, Numbers};
use super::x64::*;
use crate::bytecode::{Op, Proto};
use crate::value::Value;

/// Tags of `Value`, the discriminants `#[repr(u8)]` gives its variants.
const TAG_UNDEF: u8 = 0;
const TAG_NULL: u8 = 1;
pub(super) const TAG_BOOL: u8 = 2;
pub(super) const TAG_NUM: u8 = 3;
const TAG_STR: u8 = 4;
const TAG_ARR: u8 = 5;
const TAG_FUNC: u8 = 7;
const TAG_NATIVE: u8 = 8;
/// Bit `n` is set when tag `n` owns a reference (see `Value::owns_ref`).
const OWNING: u32 = (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7) | (1 << 9);

/// Where native code finds the parts of the VM it reads and writes: the frame stack and the value stack, both
/// `repr(C)` (see `vm`), and the fields of a frame. Each offset is from the start of the VM, or of the frame.
struct VmLayout {
    frames_buf: i32,
    frames_len: i32,
    frames_cap: i32,
    stack_buf: i32,
    stack_len: i32,
    stack_cap: i32,
    frame_size: i32,
    f_closure: i32,
    f_ip: i32,
    f_base: i32,
    f_handlers: i32,
    f_ctor: i32,
    f_session: i32,
}

fn vm_layout() -> VmLayout {
    use crate::vm::{Frame, FrameStack, ValueStack, Vm};
    let (frames, stack) = (
        std::mem::offset_of!(Vm, frames),
        std::mem::offset_of!(Vm, stack),
    );
    VmLayout {
        frames_buf: (frames + std::mem::offset_of!(FrameStack, buf)) as i32,
        frames_len: (frames + std::mem::offset_of!(FrameStack, len)) as i32,
        frames_cap: (frames + std::mem::offset_of!(FrameStack, cap)) as i32,
        stack_buf: (stack + std::mem::offset_of!(ValueStack, buf)) as i32,
        stack_len: (stack + std::mem::offset_of!(ValueStack, len)) as i32,
        stack_cap: (stack + std::mem::offset_of!(ValueStack, cap)) as i32,
        frame_size: std::mem::size_of::<Frame>() as i32,
        f_closure: std::mem::offset_of!(Frame, closure) as i32,
        f_ip: std::mem::offset_of!(Frame, ip) as i32,
        f_base: std::mem::offset_of!(Frame, base) as i32,
        f_handlers: std::mem::offset_of!(Frame, handlers) as i32,
        f_ctor: std::mem::offset_of!(Frame, ctor) as i32,
        f_session: std::mem::offset_of!(Frame, session) as i32,
    }
}

/// The machine code of a function, and what the VM needs to run it.
pub(super) struct Compiled {
    pub(super) code: Vec<u8>,
    /// The offset of each instruction's code, with one extra entry for the end.
    pub(super) offsets: Vec<usize>,
    /// The stack slots a frame needs: its locals and its deepest operand stack.
    pub(super) frame_len: usize,
    /// The offset of the wrapper of the function region, and what the region needs, if the function
    /// has one.
    pub(super) function: Option<(region::Entries<usize>, region::Function)>,
}

/// Compiles `proto`.
pub(super) fn compile(proto: &Proto) -> Option<Compiled> {
    let (depth, deepest) = depths(proto)?;
    let types = number_slots(proto, &depth, deepest, 0);
    // A function region sees its parameters as numbers, which only its own wrapper guarantees.
    let plan = region::plan_function(
        proto,
        &depth,
        &number_slots(proto, &depth, deepest, proto.nparams as usize),
    );
    let mut gen = Gen::new(proto, &depth, types);
    gen.prologue();
    gen.body();
    gen.epilogue();
    let entries = plan
        .as_ref()
        .map(|lp| region::emit_function(&mut gen.a, lp, proto, &depth));
    let offsets = gen.ops.iter().map(|&l| gen.a.offset(l)).collect();
    let function = match (plan, entries) {
        (Some(lp), Some(entries)) => lp.function.map(|f| (entries.offsets(&gen.a), f)),
        _ => None,
    };
    let code = gen.a.finish()?;
    Some(Compiled {
        code,
        offsets,
        frame_len: proto.nlocals as usize + deepest as usize,
        function,
    })
}

/// The operand depth before each instruction (-1 when it cannot run), and the deepest one.
/// `None` if the depths disagree at a join, which would mean the bytecode is malformed.
pub(super) fn depths(proto: &Proto) -> Option<(Vec<i32>, i32)> {
    let n = proto.code.len();
    if n == 0 {
        return None;
    }
    let mut depth = vec![-1i32; n];
    let mut deepest = 0;
    depth[0] = 0;
    let mut work = vec![0usize];
    while let Some(ip) = work.pop() {
        for (next, d) in successors(proto.code[ip], ip, depth[ip]) {
            if next >= n || d < 0 {
                return None;
            }
            deepest = deepest.max(d);
            if depth[next] < 0 {
                depth[next] = d;
                work.push(next);
            } else if depth[next] != d {
                return None;
            }
        }
    }
    Some((depth, deepest))
}

/// The instructions that can run after `op` at `ip` when `d` operands are on the stack, each
/// with the depth it is reached at.
pub(super) fn successors(op: Op, ip: usize, d: i32) -> Vec<(usize, i32)> {
    let next = ip + 1;
    let at = |t: u32| t as usize;
    match op {
        Op::Jump(t) => vec![(at(t), d)],
        Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => vec![(at(t), d - 1), (next, d - 1)],
        Op::JumpIfNotLt(t) | Op::JumpIfNotLe(t) | Op::JumpIfNotGt(t) | Op::JumpIfNotGe(t) => {
            vec![(at(t), d - 2), (next, d - 2)]
        }
        Op::JumpIfLtLK(_, _, t)
        | Op::JumpIfLtLL(_, _, t)
        | Op::JumpIfNotLtLK(_, _, t)
        | Op::JumpIfNotLtLL(_, _, t) => vec![(at(t), d), (next, d)],
        // The value stays on the stack when the jump is taken, and is popped when it is not.
        Op::AndJump(t) | Op::OrJump(t) | Op::NullishJump(t) => vec![(at(t), d), (next, d - 1)],
        // A handler resumes with the stack as it was, plus the exception.
        Op::PushHandler(t) => vec![(next, d), (at(t), d + 1)],
        Op::Return | Op::Throw => vec![],
        _ => vec![(next, d + delta(op))],
    }
}

/// How much an instruction that falls through changes the operand depth.
pub(super) fn delta(op: Op) -> i32 {
    match op {
        Op::Const(_)
        | Op::Undef
        | Op::Null
        | Op::True
        | Op::False
        | Op::This
        | Op::Dup
        | Op::GetLocal(_)
        | Op::GetBoxed(_)
        | Op::GetLocalProp(..)
        | Op::GetSuper(_)
        | Op::GetSuperCtor
        | Op::AddLK(..)
        | Op::SubLK(..)
        | Op::MulLK(..)
        | Op::ModLK(..)
        | Op::GetUpval(_)
        | Op::GetGlobal(_)
        | Op::GetGlobalOrUndef(_)
        | Op::Closure(_)
        | Op::NewArray
        | Op::NewObject(_) => 1,
        Op::Dup2 => 2,
        Op::Pop
        | Op::PopToLocal(_)
        | Op::AddToLocal(_)
        | Op::ConcatLocal(_)
        | Op::SetProp(_)
        | Op::GetIndex
        | Op::GetIndex2Local(_)
        | Op::SetIndexLocal(_)
        | Op::ArrayPush
        | Op::ArrayExtend
        | Op::InitProp(_)
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Mod
        | Op::Pow
        | Op::BitAnd
        | Op::BitOr
        | Op::BitXor
        | Op::Shl
        | Op::Shr
        | Op::UShr
        | Op::In
        | Op::InstanceOf
        | Op::DeleteIndex
        | Op::Eq
        | Op::Ne
        | Op::StrictEq
        | Op::StrictNe
        | Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge => -1,
        Op::SetIndex | Op::SetIndex2Local(_) | Op::CallSpread | Op::NewSpread => -2,
        Op::Call(argc) | Op::New(argc) => -(argc as i32) - 1,
        Op::Swap
        | Op::SetLocal(_)
        | Op::NewCell(_)
        | Op::SetBoxed(_)
        | Op::BoxParam(_)
        | Op::Rebox(_)
        | Op::IncLocal(_)
        | Op::DecLocal(_)
        | Op::SetUpval(_)
        | Op::SetGlobal(_)
        | Op::ConstAssign(_)
        | Op::GetProp(_)
        | Op::GetIndexLocal(_)
        | Op::Neg
        | Op::Not
        | Op::BitNot
        | Op::ToNumber
        | Op::TypeOf
        | Op::ToStr
        | Op::ToKeys
        | Op::ToItems
        | Op::DeleteProp(_)
        | Op::PushHandler(_)
        | Op::PopHandler => 0,
        // Jumps, `Return` and `Throw` are described by `successors`.
        Op::Jump(_)
        | Op::JumpIfFalse(_)
        | Op::JumpIfTrue(_)
        | Op::JumpIfNotLt(_)
        | Op::JumpIfNotLe(_)
        | Op::JumpIfNotGt(_)
        | Op::JumpIfNotGe(_)
        | Op::JumpIfLtLK(..)
        | Op::JumpIfLtLL(..)
        | Op::JumpIfNotLtLK(..)
        | Op::JumpIfNotLtLL(..)
        | Op::AndJump(_)
        | Op::OrJump(_)
        | Op::NullishJump(_)
        | Op::Return
        | Op::Throw => 0,
    }
}

/// The jump target of a conditional instruction. Its helper reports whether to take it.
fn jump_target(op: Op) -> Option<u32> {
    match op {
        Op::JumpIfFalse(t)
        | Op::JumpIfTrue(t)
        | Op::JumpIfNotLt(t)
        | Op::JumpIfNotLe(t)
        | Op::JumpIfNotGt(t)
        | Op::JumpIfNotGe(t)
        | Op::JumpIfLtLK(_, _, t)
        | Op::JumpIfLtLL(_, _, t)
        | Op::JumpIfNotLtLK(_, _, t)
        | Op::JumpIfNotLtLL(_, _, t)
        | Op::AndJump(t)
        | Op::OrJump(t)
        | Op::NullishJump(t) => Some(t),
        _ => None,
    }
}

/// The xmm registers that can hold local numbers. xmm0 and xmm1 stay free as scratch registers.
const CACHE_REGS: std::ops::Range<u8> = 2..10;

/// The locals that a fast path reads or writes as a number.
pub(super) fn local_uses(op: Op) -> Vec<u32> {
    match op {
        Op::GetLocal(i)
        | Op::PopToLocal(i)
        | Op::SetLocal(i)
        | Op::IncLocal(i)
        | Op::DecLocal(i)
        | Op::AddToLocal(i) => vec![i],
        Op::AddLK(s, _) | Op::SubLK(s, _) | Op::MulLK(s, _) | Op::ModLK(s, _) => vec![s],
        Op::JumpIfLtLK(s, _, _) | Op::JumpIfNotLtLK(s, _, _) => vec![s],
        Op::JumpIfLtLL(p, q, _) | Op::JumpIfNotLtLL(p, q, _) => vec![p, q],
        _ => Vec::new(),
    }
}

/// Picks the locals whose numbers stay in xmm registers while the code runs. A loop's counter and
/// accumulator are read and written on every iteration, and a trip through the frame adds the
/// latency of a store and a load to each step. Only locals used inside loops are chosen, the most
/// used first. A cached register always holds the payload of its slot, so it is only read when the
/// tag says number, and it is reloaded whenever the slot is written by other code.
fn cached_locals(proto: &Proto) -> Vec<Option<u8>> {
    let n = proto.code.len();
    // How many loops each instruction is in. A jump back from `ip` to `t` makes `t..=ip` a loop.
    let mut loops = vec![0u64; n];
    for (ip, &op) in proto.code.iter().enumerate() {
        for (t, _) in successors(op, ip, 0) {
            if t < n && t <= ip {
                for count in &mut loops[t..=ip] {
                    *count += 1;
                }
            }
        }
    }
    let mut uses = vec![0u64; proto.nlocals as usize];
    for (ip, &op) in proto.code.iter().enumerate() {
        if loops[ip] == 0 {
            continue;
        }
        for i in local_uses(op) {
            if let Some(count) = uses.get_mut(i as usize) {
                *count += loops[ip];
            }
        }
    }
    let mut order: Vec<usize> = (0..uses.len()).filter(|&i| uses[i] > 0).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(uses[i]), i));
    let mut cache = vec![None; uses.len()];
    for (reg, i) in CACHE_REGS.zip(order) {
        cache[i] = Some(reg);
    }
    cache
}

/// The reciprocal `m` and shift `s` with `n / c == (n * m) >> s` for every `n` in 0..=2^31 and every
/// `c >= 2`: `s = 32 + ceil(log2 c)` and `m = ceil(2^s / c)`. Then `m` is below 2^33, so `n * m` fits in 64 bits.
pub(super) fn magic(c: u32) -> (u64, u8) {
    let k = 32 - (c - 1).leading_zeros();
    let s = 32 + k;
    let m = ((1u128 << s) + c as u128 - 1) / c as u128;
    (m as u64, s as u8)
}

/// The divisor of `x % c` as an integer, when the reciprocal of `c` gives the quotient: an integer
/// from 2 to i32::MAX.
pub(super) fn int_divisor(c: f64) -> Option<u32> {
    (c >= 2.0 && c <= i32::MAX as f64 && c.fract() == 0.0).then_some(c as u32)
}

/// `x % c` for an integer divisor `c`: x is in `src` and the result goes to `dst`, which may be `src`.
/// xmm0 is scratch. The remainder takes the sign of x, sign bit included, so -0 stays -0. Jumps to
/// `slow` when x is not an integer in the 32-bit range, where the reciprocal is not exact. Binds
/// `join` after the code, which is where the result is ready.
pub(super) fn emit_mod_const(a: &mut Asm, c: u32, src: u8, dst: u8, slow: Label, join: Label) {
    let (m, s) = magic(c);
    a.cvttsd2si(RAX, src);
    a.cvtsi2sd(0, RAX);
    // The integer converted back must equal x. A NaN compares unordered, which the parity check catches.
    a.ucomisd(0, src);
    a.jcc(CC_NE, slow);
    a.jcc(CC_P, slow);
    // rcx = x and r8 = |x|, which is at most 2^31: `neg` sets the sign flag when x is positive, and then
    // cmovs takes x back. r9 = |x| - q * c, where q = (|x| * m) >> s is |x| / c.
    a.movsxd(RCX, RAX);
    a.mov_rr(R8, RCX);
    a.neg64(R8);
    a.cmovs(R8, RCX);
    a.mov_rr(R9, R8);
    a.mov_imm64(R10, m);
    a.imul64(R8, R10);
    a.shr64_imm(R8, s);
    a.imul64_imm(R8, R8, c as i32);
    a.sub64(R9, R8);
    // The sign bit of x, which a negative remainder takes, and which -0 needs.
    a.movmskpd(RDX, src);
    a.shl64_imm(RDX, 63);
    a.movq_to_xmm(0, RDX);
    // cvtsi2sd keeps the destination's old upper bits, so zeroing it first breaks a false dependency.
    a.xorpd(dst, dst);
    a.cvtsi2sd64(dst, R9);
    a.xorpd(dst, 0);
    a.bind(join);
}

/// The instructions that jumps go back to, which are the first instruction of each loop.
fn loop_headers(proto: &Proto) -> Vec<usize> {
    let mut heads = Vec::new();
    for (ip, &op) in proto.code.iter().enumerate() {
        for (t, _) in successors(op, ip, 0) {
            if t <= ip && !heads.contains(&t) {
                heads.push(t);
            }
        }
    }
    heads
}

/// An argument of a value helper: the VM, an immediate, or the address of a value.
#[derive(Clone, Copy)]
enum Arg {
    Vm,
    Imm(u64),
    Addr(Mem),
}

/// What a guarded fast path does when its checks fail.
enum Slow {
    /// The instruction through its helper, which handles every case.
    Op,
    /// A call of a value helper with these arguments.
    Direct(usize, Vec<Arg>),
    /// The concatenation helper, and the instruction's helper when that declines.
    Concat,
}

/// What the fast path of an instruction needs.
enum Fast {
    /// There is none: the instruction calls its helper.
    Generic,
    /// The fast path is all the instruction needs.
    Pure,
    /// The checks branch to the instruction's helper when they fail.
    Guarded,
    /// The checks branch to a value helper, with these arguments, when they fail.
    Direct(usize, Vec<Arg>),
    /// The checks branch to the concatenation helper, and then to the instruction's helper when it declines.
    Concat,
}

struct Gen<'a> {
    proto: &'a Proto,
    depth: &'a [i32],
    nlocals: i32,
    a: Asm,
    /// The label at the start of each instruction. The last one marks the end of the code.
    ops: Vec<Label>,
    /// Out-of-line slow paths: where each starts, the instruction it belongs to, and what it does.
    slow: Vec<(Label, usize, Slow)>,
    /// Leaves the function with an exception pending.
    threw: Label,
    /// Leaves the function after it returned.
    done: Label,
    /// The xmm register that holds each local's number, for the locals used in loops.
    cache: Vec<Option<u8>>,
    /// The slots known to hold numbers before each instruction (see `types`).
    types: Numbers,
    /// The instruction being translated.
    cur: usize,
    /// The loops that run as regions, each with the label its entry is called through.
    regions: Vec<(region::Region, Label)>,
    /// For each instruction that a region is entered at, the region.
    entry_at: Vec<Option<usize>>,
}

impl<'a> Gen<'a> {
    fn new(proto: &'a Proto, depth: &'a [i32], types: Numbers) -> Gen<'a> {
        let mut a = Asm::default();
        let ops = (0..=proto.code.len()).map(|_| a.label()).collect();
        let threw = a.label();
        let done = a.label();
        let mut regions = Vec::new();
        let mut entry_at = vec![None; proto.code.len()];
        for header in loop_headers(proto) {
            if let Some(lp) = region::plan(proto, depth, &types, header) {
                // A region is entered where the generic code reaches it, so each entry needs one
                // region. The first region to claim an entry keeps it.
                for &e in &lp.entries {
                    if entry_at[e].is_none() {
                        entry_at[e] = Some(regions.len());
                    }
                }
                regions.push((lp, a.label()));
            }
        }
        Gen {
            proto,
            depth,
            nlocals: proto.nlocals as i32,
            a,
            ops,
            slow: Vec::new(),
            threw,
            done,
            cache: cached_locals(proto),
            types,
            cur: 0,
            regions,
            entry_at,
        }
    }

    /// The frame slot at `index`.
    fn slot(&self, index: i32) -> Mem {
        Mem {
            base: R12,
            disp: 16 * index,
        }
    }

    /// Local variable `i`.
    fn local(&self, i: u32) -> Mem {
        self.slot(i as i32)
    }

    /// The operand at depth `d`.
    fn temp(&self, d: i32) -> Mem {
        self.slot(self.nlocals + d)
    }

    fn target(&self, t: u32) -> Label {
        self.ops[t as usize]
    }

    fn num_const(&self, k: u32) -> Option<f64> {
        match self.proto.consts.get(k as usize) {
            Some(Value::Num(x)) => Some(*x),
            _ => None,
        }
    }

    // ----- shared sequences -------------------------------------------------

    /// Jumps to `slow` unless the value at `m` has the tag `tag`. A number the analysis already
    /// proves is not checked again.
    fn check_tag(&mut self, m: Mem, tag: u8, slow: Label) {
        if tag == TAG_NUM && self.known_number(m) {
            return;
        }
        self.a.cmp8_imm(m, tag);
        self.a.jcc(CC_NE, slow);
    }

    /// Whether the slot at `m` is known to hold a number before the current instruction.
    fn known_number(&self, m: Mem) -> bool {
        let slot = (m.disp / 16) as usize;
        self.types
            .get(self.cur)
            .and_then(|known| known.get(slot))
            .copied()
            .unwrap_or(false)
    }

    /// Jumps to `slow` if the value at `m` owns a reference, which a plain overwrite would leak.
    fn check_plain(&mut self, m: Mem, slow: Label) {
        self.a.movzx_load8(RAX, m);
        self.a.mov_imm32(RCX, OWNING);
        self.a.bt(RCX, RAX);
        self.a.jcc(CC_B, slow);
    }

    /// Copies a whole value. Only for plain values, or for a value that is moved.
    fn copy(&mut self, src: Mem, dst: Mem) {
        self.a.mov_load(RAX, src);
        self.a.mov_load(RCX, src.at(8));
        self.a.mov_store(dst, RAX);
        self.a.mov_store(dst.at(8), RCX);
    }

    /// Stores the double in `x` as a number at `dst`.
    fn store_num(&mut self, dst: Mem, x: u8) {
        self.a.movsd_store(dst.at(8), x);
        self.a.mov8_imm(dst, TAG_NUM);
    }

    /// Stores the boolean in al at `dst`.
    fn store_bool(&mut self, dst: Mem) {
        self.a.mov8_imm(dst, TAG_BOOL);
        self.a.mov_store8(dst.at(1), RAX);
    }

    /// The xmm register that holds local `i`'s number, if it has one.
    fn cached(&self, i: u32) -> Option<u8> {
        self.cache.get(i as usize).copied().flatten()
    }

    /// Copies local `i`'s number into xmm `x`. The local must hold a number.
    fn load_num(&mut self, i: u32, x: u8) {
        match self.cached(i) {
            Some(r) => self.a.movapd_rr(x, r),
            None => {
                let m = self.local(i).at(8);
                self.a.movsd_load(x, m);
            }
        }
    }

    /// Stores the number in xmm `x` as local `i`'s payload, and keeps the register in step.
    fn store_payload(&mut self, i: u32, x: u8) {
        let m = self.local(i).at(8);
        self.a.movsd_store(m, x);
        if let Some(r) = self.cached(i) {
            self.a.movapd_rr(r, x);
        }
    }

    /// Reloads local `i`'s register after its frame slot was written by other code.
    fn reload_local(&mut self, i: u32) {
        if let Some(r) = self.cached(i) {
            let m = self.local(i).at(8);
            self.a.movsd_load(r, m);
        }
    }

    /// Reloads every cached local. Helpers can write any frame slot, and they clobber every xmm
    /// register, so this follows each call and comes first in the function.
    fn reload_all(&mut self) {
        for i in 0..self.cache.len() {
            self.reload_local(i as u32);
        }
    }

    /// After `ucomisd xmm1, xmm0` (which compares y with x): jumps to `t` when `x < y` if
    /// `when_less`, and when `x < y` is false otherwise. NaN makes `x < y` false.
    fn jump_on_less(&mut self, when_less: bool, t: u32) {
        self.a.ucomisd(1, 0);
        let target = self.target(t);
        self.a.jcc(if when_less { CC_A } else { CC_BE }, target);
    }

    /// `++` or `--` on a local that holds a number.
    fn step_local(&mut self, i: u32, sse: u8, slow: Label) -> Fast {
        let x = self.local(i);
        self.check_tag(x, TAG_NUM, slow);
        self.load_num(i, 0);
        self.a.mov_imm64(RAX, 1f64.to_bits());
        self.a.movq_to_xmm(1, RAX);
        self.a.sse(sse, 0, 1);
        self.store_payload(i, 0);
        Fast::Guarded
    }

    /// `local % c` for an integer divisor `c` (see `int_divisor`). A local that is not an integer
    /// takes the slow path, which the exact check of `emit_mod_const` sends it to.
    fn mod_local(&mut self, i: u32, c: u32, d: i32, slow: Label) {
        let x = self.local(i);
        let dst = self.temp(d);
        self.check_tag(x, TAG_NUM, slow);
        self.load_num(i, 1);
        let join = self.a.label();
        emit_mod_const(&mut self.a, c, 1, 1, slow, join);
        self.store_num(dst, 1);
    }

    // ----- helper calls -----------------------------------------------------

    /// Leaves for the helper's status: jumps when it reports one, and exits when it threw.
    /// Loads the frame address the helper returned, since the stack may have moved.
    fn after_helper(&mut self, jump: Option<u32>) {
        self.a.mov_rr(R12, RDX);
        self.reload_all();
        if let Some(t) = jump {
            self.a.cmp32_imm8(RAX, STATUS_JUMP as u8);
            let target = self.target(t);
            self.a.jcc(CC_E, target);
        }
        self.a.test32(RAX, RAX);
        let threw = self.threw;
        self.a.jcc(CC_NE, threw);
    }

    /// Runs the instruction at `ip` through its helper, with `d` operands on the stack.
    fn op_helper(&mut self, ip: usize, d: i32) {
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RSI, ip as u32);
        self.a.mov_imm32(RDX, d as u32);
        self.a.mov_imm64(RAX, rt::op as *const () as usize as u64);
        self.a.call_reg(RAX);
        let jump = jump_target(self.proto.code[ip]);
        self.after_helper(jump);
    }

    /// Calls a value helper, with its arguments in rdi, rsi, rdx and rcx.
    fn direct_call(&mut self, func: usize, args: &[Arg]) {
        const REGS: [u8; 4] = [RDI, RSI, RDX, RCX];
        for (&reg, arg) in REGS.iter().zip(args) {
            match *arg {
                Arg::Vm => self.a.mov_rr(reg, RBX),
                Arg::Imm(v) => self.a.mov_imm64(reg, v),
                Arg::Addr(m) => self.a.lea(reg, m),
            }
        }
        self.a.mov_imm64(RAX, func as u64);
        self.a.call_reg(RAX);
        self.reload_all();
    }

    /// Runs a call or construct (`flags` is `rt::CALL_NEW` and/or `rt::CALL_SPREAD`).
    fn call_helper(&mut self, ip: usize, d: i32, argc: u32, flags: u32) {
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RSI, ip as u32);
        self.a.mov_imm32(RDX, d as u32);
        self.a.mov_imm32(RCX, argc);
        self.a.mov_imm32(R8, flags);
        self.a.mov_imm64(RAX, rt::call as *const () as usize as u64);
        self.a.call_reg(RAX);
        self.after_helper(None);
    }

    /// One step of a call with one argument. When the callee is the built-in at `def`, `helper` runs the call in
    /// place, and the call is done when it returns true. A helper that declines takes `generic`. Any other callee
    /// goes on to `next`.
    fn builtin_call1(
        &mut self,
        callee: Mem,
        def: u64,
        helper: usize,
        next: Label,
        generic: Label,
        done: Label,
    ) {
        self.a.mov_load(RAX, callee.at(8));
        self.a.mov_imm64(RCX, def);
        self.a.cmp_rr64(RAX, RCX);
        self.a.jcc(CC_NE, next);
        self.a.lea(RDI, callee);
        self.a.mov_imm64(RAX, helper as u64);
        self.a.call_reg(RAX);
        // The helper clobbers the xmm registers that hold cached locals, so they are reloaded before any use.
        self.reload_all();
        self.a.test8(RAX, RAX);
        self.a.jcc(CC_NE, done);
        self.a.jmp(generic);
    }

    /// A call of a compiled closure, which native code makes itself. The callee is a closure whose code is compiled,
    /// it takes exactly the arguments of this call and no rest parameter, and the frame stack and the value stack
    /// have room for its frame. The arguments are already in place, at the top of this frame, and they become the
    /// callee's parameters. Any other call jumps to `generic`, which calls the helper.
    fn closure_call(&mut self, ip: usize, d: i32, argc: u32, generic: Label, ran: Label) {
        let argc = argc as i32;
        let nlocals = self.proto.nlocals as i32;
        let callee = self.temp(d - argc - 2);
        // The callee's frame starts at its first argument, which is `base` bytes above this frame's base.
        let base = 16 * (nlocals + d - argc);
        let proto_field = |field: usize| Mem {
            base: RDX,
            disp: field as i32,
        };
        let l = vm_layout();
        let (frames_buf, frames_len, frames_cap) = (l.frames_buf, l.frames_len, l.frames_cap);
        let (stack_buf, stack_cap) = (l.stack_buf, l.stack_cap);
        let (frame_size, f_closure, f_ip, f_base) = (l.frame_size, l.f_closure, l.f_ip, l.f_base);
        let (f_handlers, f_ctor, f_session) = (l.f_handlers, l.f_ctor, l.f_session);

        // The callee is a closure. Its allocation holds the closure after the counts, and the closure holds its
        // prototype the same way (see `RC_VALUE`).
        self.check_tag(callee, TAG_FUNC, generic);
        self.a.mov_load(RCX, callee.at(8));
        self.a.add64_imm32(RCX, super::RC_VALUE);
        let proto_of_closure = std::mem::offset_of!(crate::value::Closure, proto) as i32;
        self.a.mov_load(
            RDX,
            Mem {
                base: RCX,
                disp: proto_of_closure,
            },
        );
        self.a.add64_imm32(RDX, super::RC_VALUE);
        // The code must be compiled, and the function must take the arguments of this call and no rest parameter.
        self.a
            .mov_load(R8, proto_field(std::mem::offset_of!(Proto, call_entry)));
        self.a.test64(R8, R8);
        self.a.jcc(CC_E, generic);
        self.a
            .mov32_load(R9, proto_field(std::mem::offset_of!(Proto, nparams)));
        self.a.cmp64_imm32(R9, argc);
        self.a.jcc(CC_NE, generic);
        self.a
            .movzx_load8(R9, proto_field(std::mem::offset_of!(Proto, has_rest)));
        self.a.test32(R9, R9);
        self.a.jcc(CC_NE, generic);
        // The frame stack has room for one more frame.
        self.a.mov_load(
            R10,
            Mem {
                base: RBX,
                disp: frames_len,
            },
        );
        self.a.cmp64_imm32(R10, crate::vm::MAX_FRAMES as i32);
        self.a.jcc(CC_AE, generic);
        self.a.mov_load(
            R11,
            Mem {
                base: RBX,
                disp: frames_cap,
            },
        );
        self.a.cmp_rr64(R10, R11);
        self.a.jcc(CC_AE, generic);
        // The value stack has room for the callee's whole frame, from its first argument on.
        self.a.lea(
            R9,
            Mem {
                base: R12,
                disp: base,
            },
        );
        self.a.mov_load(
            R11,
            proto_field(std::mem::offset_of!(Proto, call_frame_len)),
        );
        self.a.shl64_imm(R11, 4);
        self.a.add64(R11, R9);
        self.a.mov_load(
            RDI,
            Mem {
                base: RBX,
                disp: stack_buf,
            },
        );
        self.a.mov_load(
            RSI,
            Mem {
                base: RBX,
                disp: stack_cap,
            },
        );
        self.a.shl64_imm(RSI, 4);
        self.a.add64(RSI, RDI);
        self.a.cmp_rr64(R11, RSI);
        self.a.jcc(CC_A, generic);
        // The locals that the arguments do not fill start as undefined: their tags are written here.
        self.a.lea(
            RSI,
            Mem {
                base: R12,
                disp: 16 * (nlocals + d),
            },
        );
        self.a
            .mov32_load(RDI, proto_field(std::mem::offset_of!(Proto, nlocals)));
        self.a.shl64_imm(RDI, 4);
        self.a.add64(RDI, R9);
        let (fill, filled) = (self.a.label(), self.a.label());
        self.a.bind(fill);
        self.a.cmp_rr64(RSI, RDI);
        self.a.jcc(CC_AE, filled);
        self.a.mov8_imm(Mem { base: RSI, disp: 0 }, TAG_UNDEF);
        self.a.add64_imm32(RSI, 16);
        self.a.jmp(fill);
        self.a.bind(filled);
        // The caller's ip is saved, and the frame goes after the last one. Every field of it is written.
        self.a.mov_load(
            R10,
            Mem {
                base: RBX,
                disp: frames_len,
            },
        );
        self.a.imul64_imm(R10, R10, frame_size);
        self.a.mov_load(
            R11,
            Mem {
                base: RBX,
                disp: frames_buf,
            },
        );
        self.a.add64(R11, R10);
        self.a.mov_imm64(RSI, (ip + 1) as u64);
        self.a.mov_store(
            Mem {
                base: R11,
                disp: f_ip - frame_size,
            },
            RSI,
        );
        self.a.mov_store(
            Mem {
                base: R11,
                disp: f_closure,
            },
            RCX,
        );
        self.a.mov_imm64(RSI, 0);
        self.a.mov_store(
            Mem {
                base: R11,
                disp: f_ip,
            },
            RSI,
        );
        self.a.mov_store(
            Mem {
                base: R11,
                disp: f_handlers,
            },
            RSI,
        );
        self.a.mov_imm64(R10, 0);
        self.a.mov_store8(
            Mem {
                base: R11,
                disp: f_ctor,
            },
            R10,
        );
        self.a.mov_store8(
            Mem {
                base: R11,
                disp: f_session,
            },
            R10,
        );
        self.a.mov_rr(R10, R12);
        self.a.mov_load(
            RSI,
            Mem {
                base: RBX,
                disp: stack_buf,
            },
        );
        self.a.sub64(R10, RSI);
        self.a.shr64_imm(R10, 4);
        self.a.add64_imm32(R10, nlocals + d - argc);
        self.a.mov_store(
            Mem {
                base: R11,
                disp: f_base,
            },
            R10,
        );
        self.a.mov_load(
            R10,
            Mem {
                base: RBX,
                disp: frames_len,
            },
        );
        self.a.add64_imm8(R10, 1);
        self.a.mov_store(
            Mem {
                base: RBX,
                disp: frames_len,
            },
            R10,
        );
        // The callee runs from its first instruction, with its frame at its first argument.
        self.a.lea(
            RSI,
            Mem {
                base: R12,
                disp: base,
            },
        );
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RDX, 0);
        self.a.mov_rr(RAX, R8);
        self.a.call_reg(RAX);
        self.a.jmp(ran);
    }

    /// A plain call. `rt::call_plain` pushes the callee's frame when it can, and then native code
    /// runs the callee's code itself. Otherwise the call has already run when the helper returns.
    fn plain_call(&mut self, ip: usize, d: i32, argc: u32) {
        let (generic, done, ran, closure_path) = (
            self.a.label(),
            self.a.label(),
            self.a.label(),
            self.a.label(),
        );
        if argc == 1 {
            // Some built-ins run in place when called with one argument: `push` on an array (`rt::push_call`),
            // `charCodeAt` on a string (`rt::char_code_call`) and `String` on a number (`rt::string_call`). Any
            // other callee takes the generic path below, and so does a call that a helper declines.
            let callee = self.temp(d - 3);
            let (not_push, not_char_code) = (self.a.label(), self.a.label());
            let push = crate::builtins::array::push_def() as *const _ as usize as u64;
            let char_code = crate::builtins::string::char_code_at_def() as *const _ as usize as u64;
            let string = crate::builtins::global::string_def() as *const _ as usize as u64;
            self.check_tag(callee, TAG_NATIVE, closure_path);
            let push_helper = rt::push_call as *const () as usize;
            let char_code_helper = rt::char_code_call as *const () as usize;
            let string_helper = rt::string_call as *const () as usize;
            self.builtin_call1(callee, push, push_helper, not_push, generic, done);
            self.a.bind(not_push);
            self.builtin_call1(
                callee,
                char_code,
                char_code_helper,
                not_char_code,
                generic,
                done,
            );
            self.a.bind(not_char_code);
            self.builtin_call1(callee, string, string_helper, closure_path, generic, done);
        }
        self.a.bind(closure_path);
        if super::rc_layout_ok() {
            self.closure_call(ip, d, argc, generic, ran);
        }
        self.a.bind(generic);
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RSI, ip as u32);
        self.a.mov_imm32(RDX, d as u32);
        self.a.mov_imm32(RCX, argc);
        self.a
            .mov_imm64(RAX, rt::call_plain as *const () as usize as u64);
        self.a.call_reg(RAX);
        // rax is the callee's code, or the status of a call that has run already (0 or 1). rdx is
        // this frame, which may have moved.
        self.a.cmp64_imm8(RAX, 1);
        self.a.jcc(CC_BE, ran);
        // The callee's frame starts above this frame's locals and the operands below its arguments.
        self.a.mov_rr(R12, RDX);
        self.a.lea(RSI, self.slot(self.nlocals + d - argc as i32));
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RDX, 0);
        self.a.call_reg(RAX);
        // Either way, rax is the status and rdx is this frame.
        self.a.bind(ran);
        self.a.test32(RAX, RAX);
        let threw = self.threw;
        self.a.jcc(CC_NE, threw);
        self.a.mov_rr(R12, RDX);
        self.a.bind(done);
        self.reload_all();
    }

    /// Runs the region entered at instruction `ip`, then continues at the exit it left by. When a
    /// guard failed the region did not run, and the instruction at `ip` runs here instead.
    fn enter_region(&mut self, entry: Label, ip: usize, exits: &[usize]) {
        self.a.mov_imm32(RDX, ip as u32);
        self.a.call_label(entry);
        self.reload_all();
        let generic = self.a.label();
        // The region returns -1 when it did not run, which is NOT_ENTERED (u64::MAX).
        self.a.cmp64_imm8(RAX, 0xFF);
        self.a.jcc(CC_E, generic);
        for &t in exits {
            self.a.cmp64_imm32(RAX, t as i32);
            let target = self.target(t as u32);
            self.a.jcc(CC_E, target);
        }
        self.a.ud2();
        self.a.bind(generic);
    }

    /// Returns from the function, through its helper, which pops the frame.
    /// Returns from the function without `rt::ret` when that helper would only pop the frame and move the result
    /// down. The frame must be a plain one (not a constructor, not a callback's, with no `try`), and another frame
    /// must lie below it. The slots the frame drops give up their references first: a reference that is not the last
    /// one is released in place, and the last one is dropped by `rt::drop_slot`. Any other return jumps to `slow`
    /// before anything has changed.
    fn native_return(&mut self, d: i32, slow: Label) {
        let l = vm_layout();
        let (frames_buf, frames_len, stack_buf, stack_len) =
            (l.frames_buf, l.frames_len, l.stack_buf, l.stack_len);
        let (frame_size, f_base) = (l.frame_size, l.f_base);
        let nlocals = self.proto.nlocals as i32;
        let done = self.done;
        // The result is the operand on top. The slots below it are the callee (slot -2), `this` (slot -1), the
        // locals and the other operands, and the frame drops all of them.
        let released = -2..nlocals + d - 1;
        let result = Mem {
            base: R12,
            disp: 16 * (nlocals + d - 1),
        };

        // The frame on top is `frames[len - 1]`, and the call returns to the one below it.
        self.a.mov_load(
            R10,
            Mem {
                base: RBX,
                disp: frames_len,
            },
        );
        self.a.cmp64_imm8(R10, 2);
        self.a.jcc(CC_B, slow);
        self.a.imul64_imm(R11, R10, frame_size);
        self.a.mov_load(
            RCX,
            Mem {
                base: RBX,
                disp: frames_buf,
            },
        );
        self.a.add64(R11, RCX);
        // `R11` is one past the top frame, so `top` addresses a field of that frame.
        let top = |field: i32| Mem {
            base: R11,
            disp: field - frame_size,
        };
        self.a.cmp8_imm(top(l.f_ctor), 0);
        self.a.jcc(CC_NE, slow);
        self.a.cmp8_imm(top(l.f_session), 0);
        self.a.jcc(CC_NE, slow);
        self.a.mov_load(RCX, top(l.f_handlers));
        self.a.test64(RCX, RCX);
        self.a.jcc(CC_NE, slow);

        // Each dropped slot releases its reference. The count is the first word of the `Rc` allocation (see
        // `rc_layout_ok`). The last reference goes to `rt::drop_slot`, which frees the value.
        for s in released {
            let (skip, dead) = (self.a.label(), self.a.label());
            let m = Mem {
                base: R12,
                disp: 16 * s,
            };
            self.a.movzx_load8(RAX, m);
            self.a.mov_imm32(RCX, OWNING);
            self.a.bt(RCX, RAX);
            self.a.jcc(CC_AE, skip);
            self.a.mov_load(RCX, m.at(8));
            self.a.mov_load(RAX, Mem { base: RCX, disp: 0 });
            self.a.cmp64_imm8(RAX, 1);
            self.a.jcc(CC_BE, dead);
            self.a.add64_imm8(RAX, -1);
            self.a.mov_store(Mem { base: RCX, disp: 0 }, RAX);
            self.a.jmp(skip);
            self.a.bind(dead);
            self.a.lea(RDI, m);
            self.a
                .mov_imm64(RAX, rt::drop_slot as *const () as usize as u64);
            self.a.call_reg(RAX);
            self.a.bind(skip);
        }

        // The frame is popped. The value stack ends below the callee slot, and the result takes that slot. The call
        // returns to the frame below, whose base is the caller's frame pointer (as `rt::ret` returns it).
        self.a.mov_load(
            R10,
            Mem {
                base: RBX,
                disp: frames_len,
            },
        );
        self.a.imul64_imm(R11, R10, frame_size);
        self.a.mov_load(
            RCX,
            Mem {
                base: RBX,
                disp: frames_buf,
            },
        );
        self.a.add64(R11, RCX);
        self.a.mov_load(RSI, top(f_base));
        self.a.lea(
            RCX,
            Mem {
                base: RSI,
                disp: -1,
            },
        );
        self.a.mov_store(
            Mem {
                base: RBX,
                disp: stack_len,
            },
            RCX,
        );
        self.a.mov_load(
            RDX,
            Mem {
                base: R11,
                disp: f_base - 2 * frame_size,
            },
        );
        self.a.shl64_imm(RDX, 4);
        self.a.mov_load(
            RAX,
            Mem {
                base: RBX,
                disp: stack_buf,
            },
        );
        self.a.add64(RDX, RAX);
        self.a.add64_imm8(R10, -1);
        self.a.mov_store(
            Mem {
                base: RBX,
                disp: frames_len,
            },
            R10,
        );
        self.a.mov_load(RAX, result);
        self.a.mov_load(RCX, result.at(8));
        self.a.mov_store(
            Mem {
                base: R12,
                disp: -32,
            },
            RAX,
        );
        self.a.mov_store(
            Mem {
                base: R12,
                disp: -24,
            },
            RCX,
        );
        self.a.jmp(done);
    }

    fn return_helper(&mut self, ip: usize, d: i32) {
        let slow = self.a.label();
        let nlocals = self.proto.nlocals as i32;
        // A return of a plain frame runs natively when `native_return` covers its slots. The helper runs the rest.
        if d >= 1 && nlocals + d < 16 && super::rc_layout_ok() {
            self.native_return(d, slow);
        }
        self.a.bind(slow);
        self.a.mov_rr(RDI, RBX);
        self.a.mov_imm32(RSI, ip as u32);
        self.a.mov_imm32(RDX, d as u32);
        self.a.mov_imm64(RAX, rt::ret as *const () as usize as u64);
        self.a.call_reg(RAX);
        self.a.test32(RAX, RAX);
        let threw = self.threw;
        self.a.jcc(CC_NE, threw);
        let done = self.done;
        self.a.jmp(done);
    }

    // ----- the function ------------------------------------------------------

    /// Saves the callee-saved registers this code uses, keeps the VM in rbx and the frame in r12,
    /// and enters at the instruction the caller asked for (in rdx).
    fn prologue(&mut self) {
        self.a.push_reg(RBX);
        self.a.push_reg(R12);
        // Two pushes and this keep rsp 16-byte aligned for the helper calls.
        self.a.sub_rsp(8);
        self.a.mov_rr(RBX, RDI);
        self.a.mov_rr(R12, RSI);
        self.reload_all();
        // rdx is the instruction to start at. Zero starts at the first one, which a direct call enters.
        let first = self.ops[0];
        self.a.test64(RDX, RDX);
        self.a.jcc(CC_E, first);
        self.a.jmp_reg(RDX);
    }

    fn epilogue(&mut self) {
        let (done, threw) = (self.done, self.threw);
        self.a.bind(done);
        self.a.mov_imm32(RAX, 0);
        self.leave();
        self.a.bind(threw);
        self.a.mov_imm32(RAX, 1);
        self.leave();
    }

    fn leave(&mut self) {
        self.a.add_rsp(8);
        self.a.pop_reg(R12);
        self.a.pop_reg(RBX);
        self.a.ret();
    }

    fn body(&mut self) {
        let n = self.proto.code.len();
        for ip in 0..n {
            let start = self.ops[ip];
            self.a.bind(start);
            let d = self.depth[ip];
            if d < 0 {
                self.a.ud2();
                continue;
            }
            if let Some(r) = self.entry_at[ip] {
                let (exits, entry) = (self.regions[r].0.exits.clone(), self.regions[r].1);
                self.enter_region(entry, ip, &exits);
            }
            let op = self.proto.code[ip];
            self.cur = ip;
            match op {
                Op::Jump(t) => {
                    let target = self.target(t);
                    self.a.jmp(target);
                }
                Op::Return => self.return_helper(ip, d),
                Op::Call(argc) => self.plain_call(ip, d, argc),
                Op::New(argc) => self.call_helper(ip, d, argc, rt::CALL_NEW),
                Op::CallSpread => self.call_helper(ip, d, 0, rt::CALL_SPREAD),
                Op::NewSpread => self.call_helper(ip, d, 0, rt::CALL_SPREAD | rt::CALL_NEW),
                _ => {
                    let slow = self.a.label();
                    match self.fast(op, d, slow) {
                        Fast::Generic => self.op_helper(ip, d),
                        Fast::Pure => {}
                        Fast::Guarded => self.slow.push((slow, ip, Slow::Op)),
                        Fast::Concat => self.slow.push((slow, ip, Slow::Concat)),
                        Fast::Direct(func, args) => {
                            self.slow.push((slow, ip, Slow::Direct(func, args)))
                        }
                    }
                }
            }
        }
        let end = self.ops[n];
        self.a.bind(end);
        for (entry, ip, slow) in std::mem::take(&mut self.slow) {
            self.a.bind(entry);
            match slow {
                Slow::Op => {
                    let d = self.depth[ip];
                    self.op_helper(ip, d);
                }
                Slow::Direct(func, args) => self.direct_call(func, &args),
                Slow::Concat => {
                    let d = self.depth[ip];
                    let (x, y) = (self.temp(d - 2), self.temp(d - 1));
                    let next = self.ops[ip + 1];
                    let concat = rt::concat_top as *const () as usize;
                    self.direct_call(concat, &[Arg::Addr(x), Arg::Addr(y)]);
                    self.a.test8(RAX, RAX);
                    self.a.jcc(CC_NE, next);
                    self.op_helper(ip, d);
                }
            }
            let next = self.ops[ip + 1];
            self.a.jmp(next);
        }
        for (lp, entry) in std::mem::take(&mut self.regions) {
            region::emit(&mut self.a, &lp, entry, self.proto, self.depth);
        }
    }

    /// Emits the fast path of `op`, with `d` operands on the stack. Guarded paths jump to `slow`
    /// when an operand is not the kind they handle. Nothing is written before that check.
    fn fast(&mut self, op: Op, d: i32, slow: Label) -> Fast {
        match op {
            Op::Const(k) => {
                let t = self.temp(d);
                match self.num_const(k) {
                    Some(x) => {
                        self.a.mov_imm64(RAX, x.to_bits());
                        self.a.mov_store(t.at(8), RAX);
                        self.a.mov8_imm(t, TAG_NUM);
                    }
                    None => {
                        // Not a number (a string): clone the constant, which is stored in the prototype.
                        let addr = &self.proto.consts[k as usize] as *const Value as usize;
                        let clone = rt::clone_into as *const () as usize;
                        self.direct_call(clone, &[Arg::Imm(addr as u64), Arg::Addr(t)]);
                    }
                }
                Fast::Pure
            }
            Op::Undef | Op::Null => {
                let tag = if op == Op::Undef { TAG_UNDEF } else { TAG_NULL };
                let t = self.temp(d);
                self.a.mov8_imm(t, tag);
                Fast::Pure
            }
            Op::True | Op::False => {
                let t = self.temp(d);
                self.a.mov8_imm(t, TAG_BOOL);
                self.a.mov8_imm(t.at(1), u8::from(op == Op::True));
                Fast::Pure
            }
            Op::Pop => {
                let top = self.temp(d - 1);
                self.check_plain(top, slow);
                let drop = rt::drop_value as *const () as usize;
                Fast::Direct(drop, vec![Arg::Addr(top)])
            }
            Op::GetLocal(i) => {
                let (src, dst) = (self.local(i), self.temp(d));
                self.check_plain(src, slow);
                self.copy(src, dst);
                let clone = rt::clone_into as *const () as usize;
                Fast::Direct(clone, vec![Arg::Addr(src), Arg::Addr(dst)])
            }
            Op::SetLocal(i) => {
                // Copies the top value, so both it and the old local must own nothing.
                let (src, dst) = (self.temp(d - 1), self.local(i));
                self.check_plain(src, slow);
                self.check_plain(dst, slow);
                self.copy(src, dst);
                self.reload_local(i);
                let assign = rt::assign_clone as *const () as usize;
                Fast::Direct(assign, vec![Arg::Addr(src), Arg::Addr(dst)])
            }
            Op::PopToLocal(i) => {
                // Moves the top value, so only the old local must own nothing.
                let (src, dst) = (self.temp(d - 1), self.local(i));
                self.check_plain(dst, slow);
                self.copy(src, dst);
                self.reload_local(i);
                let move_ = rt::move_into as *const () as usize;
                Fast::Direct(move_, vec![Arg::Addr(src), Arg::Addr(dst)])
            }
            Op::GetUpval(i) => {
                let dst = self.temp(d);
                let upval = rt::upval_into as *const () as usize;
                self.direct_call(upval, &[Arg::Vm, Arg::Imm(u64::from(i)), Arg::Addr(dst)]);
                Fast::Pure
            }
            Op::GetProp(i) => {
                // Replaces an object or array by one of its properties. Other values take the slow path.
                let name = &self.proto.names[i as usize] as *const Rc<str> as usize;
                let hint = &self.proto.hints[i as usize] as *const Cell<u32> as usize;
                let top = self.temp(d - 1);
                let get = rt::get_prop_top as *const () as usize;
                let args = [Arg::Addr(top), Arg::Imm(name as u64), Arg::Imm(hint as u64)];
                let (lookup, done) = (self.a.label(), self.a.label());
                if &*self.proto.names[i as usize] == "length" {
                    // The length of a string is read here, in place. Any other value takes the lookup.
                    self.check_tag(top, TAG_STR, lookup);
                    self.a.lea(RDI, top);
                    self.a
                        .mov_imm64(RAX, rt::str_length_top as *const () as usize as u64);
                    self.a.call_reg(RAX);
                    self.reload_all();
                    self.a.jmp(done);
                }
                self.a.bind(lookup);
                self.direct_call(get, &args);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                self.a.bind(done);
                Fast::Guarded
            }
            Op::NewObject(n) => {
                let dst = self.temp(d);
                let new_object = rt::new_object as *const () as usize;
                self.direct_call(new_object, &[Arg::Imm(u64::from(n)), Arg::Addr(dst)]);
                Fast::Pure
            }
            Op::InitProp(i) => {
                let name = &self.proto.names[i as usize] as *const Rc<str> as usize;
                let (obj, val) = (self.temp(d - 2), self.temp(d - 1));
                let init = rt::init_prop as *const () as usize;
                self.direct_call(
                    init,
                    &[Arg::Addr(obj), Arg::Imm(name as u64), Arg::Addr(val)],
                );
                Fast::Pure
            }
            Op::GetLocalProp(s, n) => {
                // Objects are read here. Any other value takes the slow path, which reports the error.
                let name = &self.proto.names[n as usize] as *const Rc<str> as usize;
                let (src, dst) = (self.local(s), self.temp(d));
                let get = rt::get_prop_into as *const () as usize;
                let hint = &self.proto.hints[n as usize] as *const Cell<u32> as usize;
                let args = [
                    Arg::Addr(src),
                    Arg::Imm(name as u64),
                    Arg::Addr(dst),
                    Arg::Imm(hint as u64),
                ];
                let (lookup, done) = (self.a.label(), self.a.label());
                if &*self.proto.names[n as usize] == "push" {
                    // The `push` of an array is the built-in, so it is written here, without a lookup.
                    self.check_tag(src, TAG_ARR, lookup);
                    let push = crate::builtins::array::push_def() as *const _ as usize as u64;
                    self.a.mov8_imm(dst, TAG_NATIVE);
                    self.a.mov_imm64(RAX, push);
                    self.a.mov_store(dst.at(8), RAX);
                    self.a.jmp(done);
                }
                if &*self.proto.names[n as usize] == "charCodeAt" {
                    // The `charCodeAt` of a string is the built-in, so it is written here, without a lookup.
                    self.check_tag(src, TAG_STR, lookup);
                    let char_code =
                        crate::builtins::string::char_code_at_def() as *const _ as usize as u64;
                    self.a.mov8_imm(dst, TAG_NATIVE);
                    self.a.mov_imm64(RAX, char_code);
                    self.a.mov_store(dst.at(8), RAX);
                    self.a.jmp(done);
                }
                self.a.bind(lookup);
                self.direct_call(get, &args);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                self.a.bind(done);
                Fast::Guarded
            }
            Op::GetIndex => {
                // Replaces an array and a number in range by the element. Other operands take the slow path.
                let (obj, key) = (self.temp(d - 2), self.temp(d - 1));
                let get = rt::get_index_top as *const () as usize;
                self.direct_call(get, &[Arg::Addr(obj), Arg::Addr(key)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::SetIndex => {
                // Replaces an array, a number and a value by the value, of which the element keeps a copy.
                let (obj, key, val) = (self.temp(d - 3), self.temp(d - 2), self.temp(d - 1));
                let set = rt::set_index_top as *const () as usize;
                self.direct_call(set, &[Arg::Addr(obj), Arg::Addr(key), Arg::Addr(val)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::GetIndexLocal(i) => {
                // Replaces the key by the element of an array or string in a local. Other operands take the slow path.
                let (obj, key) = (self.local(i), self.temp(d - 1));
                let get = rt::get_index_local as *const () as usize;
                self.direct_call(get, &[Arg::Addr(obj), Arg::Addr(key)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::GetIndex2Local(i) => {
                // Replaces the two keys by an element of a row of an array in a local. Other operands take the slow path.
                let (obj, k1, k2) = (self.local(i), self.temp(d - 2), self.temp(d - 1));
                let get = rt::get_index2_local as *const () as usize;
                self.direct_call(get, &[Arg::Addr(obj), Arg::Addr(k1), Arg::Addr(k2)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::SetIndexLocal(i) => {
                // Replaces the key and the value by the value, of which the element keeps a copy.
                let (obj, key, val) = (self.local(i), self.temp(d - 2), self.temp(d - 1));
                let set = rt::set_index_local as *const () as usize;
                self.direct_call(set, &[Arg::Addr(obj), Arg::Addr(key), Arg::Addr(val)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::SetIndex2Local(i) => {
                // Replaces the two keys and the value by the value, of which the element keeps a copy.
                let (obj, k1, k2, val) = (
                    self.local(i),
                    self.temp(d - 3),
                    self.temp(d - 2),
                    self.temp(d - 1),
                );
                let set = rt::set_index2_local as *const () as usize;
                self.direct_call(
                    set,
                    &[Arg::Addr(obj), Arg::Addr(k1), Arg::Addr(k2), Arg::Addr(val)],
                );
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::GetGlobal(i) => {
                // Pushes a global. A name that is not defined takes the slow path, which reports it.
                let name = &self.proto.names[i as usize] as *const Rc<str> as usize;
                let hint = &self.proto.global_hints[i as usize] as *const Cell<u32> as usize;
                let dst = self.temp(d);
                let get = rt::get_global_into as *const () as usize;
                let args = [
                    Arg::Vm,
                    Arg::Imm(name as u64),
                    Arg::Addr(dst),
                    Arg::Imm(hint as u64),
                ];
                self.direct_call(get, &args);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::SetProp(i) => {
                // Replaces an object and a value by the value, which the property keeps a copy of. Other operands
                // take the slow path.
                let name = &self.proto.names[i as usize] as *const Rc<str> as usize;
                let hint = &self.proto.hints[i as usize] as *const Cell<u32> as usize;
                let (obj, val) = (self.temp(d - 2), self.temp(d - 1));
                let set = rt::set_prop_top as *const () as usize;
                let args = [
                    Arg::Addr(obj),
                    Arg::Imm(name as u64),
                    Arg::Addr(val),
                    Arg::Imm(hint as u64),
                ];
                self.direct_call(set, &args);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::This if !self.proto.is_arrow => {
                // `this` sits just below the arguments of the call, in the slot before the frame's first local.
                let (src, dst) = (self.slot(-1), self.temp(d));
                self.check_plain(src, slow);
                self.copy(src, dst);
                let clone = rt::clone_into as *const () as usize;
                Fast::Direct(clone, vec![Arg::Addr(src), Arg::Addr(dst)])
            }
            Op::Dup => {
                // Copies the top value. A plain value is copied here, and any other is cloned by a helper.
                let (src, dst) = (self.temp(d - 1), self.temp(d));
                self.check_plain(src, slow);
                self.copy(src, dst);
                let clone = rt::clone_into as *const () as usize;
                Fast::Direct(clone, vec![Arg::Addr(src), Arg::Addr(dst)])
            }
            Op::Mod => {
                // Replaces two numbers by their remainder. Other operands take the slow path.
                let (a, b) = (self.temp(d - 2), self.temp(d - 1));
                let rem = rt::mod_top as *const () as usize;
                self.direct_call(rem, &[Arg::Addr(a), Arg::Addr(b)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
                Fast::Guarded
            }
            Op::GetBoxed(i) => {
                let (src, dst) = (self.local(i), self.temp(d));
                let boxed = rt::boxed_into as *const () as usize;
                self.direct_call(boxed, &[Arg::Addr(src), Arg::Addr(dst)]);
                Fast::Pure
            }
            Op::IncLocal(i) => self.step_local(i, SSE_ADD, slow),
            Op::DecLocal(i) => self.step_local(i, SSE_SUB, slow),
            Op::AddToLocal(i) => {
                let (rhs, x) = (self.temp(d - 1), self.local(i));
                self.check_tag(rhs, TAG_NUM, slow);
                self.check_tag(x, TAG_NUM, slow);
                self.load_num(i, 0);
                self.a.movsd_load(1, rhs.at(8));
                self.a.sse(SSE_ADD, 0, 1);
                self.store_payload(i, 0);
                Fast::Guarded
            }
            Op::ConcatLocal(i) => {
                // `x += e` on numbers adds in place, and its result is the value left on the stack. The local
                // must hold a number itself, not a cell, since the payload is written into it.
                let (old, rhs, x) = (self.temp(d - 2), self.temp(d - 1), self.local(i));
                self.check_tag(old, TAG_NUM, slow);
                self.check_tag(rhs, TAG_NUM, slow);
                self.check_tag(x, TAG_NUM, slow);
                self.a.movsd_load(0, old.at(8));
                self.a.movsd_load(1, rhs.at(8));
                self.a.sse(SSE_ADD, 0, 1);
                self.store_num(old, 0);
                self.store_payload(i, 0);
                Fast::Guarded
            }
            Op::AddLK(s, k) | Op::SubLK(s, k) | Op::MulLK(s, k) => {
                let Some(c) = self.num_const(k) else {
                    return Fast::Generic;
                };
                let sse = match op {
                    Op::AddLK(..) => SSE_ADD,
                    Op::SubLK(..) => SSE_SUB,
                    _ => SSE_MUL,
                };
                let (x, dst) = (self.local(s), self.temp(d));
                self.check_tag(x, TAG_NUM, slow);
                self.load_num(s, 0);
                self.a.mov_imm64(RAX, c.to_bits());
                self.a.movq_to_xmm(1, RAX);
                self.a.sse(sse, 0, 1);
                self.store_num(dst, 0);
                Fast::Guarded
            }
            Op::ModLK(s, k) => {
                let c = match self.num_const(k).and_then(int_divisor) {
                    Some(c) => c,
                    None => return Fast::Generic,
                };
                self.mod_local(s, c, d, slow);
                Fast::Guarded
            }
            Op::Add | Op::Sub | Op::Mul | Op::Div => {
                let sse = match op {
                    Op::Add => SSE_ADD,
                    Op::Sub => SSE_SUB,
                    Op::Mul => SSE_MUL,
                    _ => SSE_DIV,
                };
                let (x, y) = (self.temp(d - 2), self.temp(d - 1));
                self.check_tag(x, TAG_NUM, slow);
                self.check_tag(y, TAG_NUM, slow);
                self.a.movsd_load(0, x.at(8));
                self.a.movsd_load(1, y.at(8));
                self.a.sse(sse, 0, 1);
                self.store_num(x, 0);
                // Only addition has another meaning, which is the concatenation of strings.
                if op == Op::Add {
                    Fast::Concat
                } else {
                    Fast::Guarded
                }
            }
            Op::StrictEq | Op::StrictNe | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                let (x, y) = (self.temp(d - 2), self.temp(d - 1));
                self.check_tag(x, TAG_NUM, slow);
                self.check_tag(y, TAG_NUM, slow);
                self.a.movsd_load(0, x.at(8));
                self.a.movsd_load(1, y.at(8));
                // An unordered (NaN) comparison sets ZF, PF and CF, so the ordered tests below
                // come out false, and equality also checks the parity flag.
                match op {
                    Op::Lt => {
                        self.a.ucomisd(1, 0);
                        self.a.setcc(CC_A, RAX);
                    }
                    Op::Le => {
                        self.a.ucomisd(1, 0);
                        self.a.setcc(CC_AE, RAX);
                    }
                    Op::Gt => {
                        self.a.ucomisd(0, 1);
                        self.a.setcc(CC_A, RAX);
                    }
                    Op::Ge => {
                        self.a.ucomisd(0, 1);
                        self.a.setcc(CC_AE, RAX);
                    }
                    Op::StrictEq => {
                        self.a.ucomisd(0, 1);
                        self.a.setcc(CC_E, RAX);
                        self.a.setcc(CC_NP, RCX);
                        self.a.and8(RAX, RCX);
                    }
                    _ => {
                        self.a.ucomisd(0, 1);
                        self.a.setcc(CC_NE, RAX);
                        self.a.setcc(CC_P, RCX);
                        self.a.or8(RAX, RCX);
                    }
                }
                self.store_bool(x);
                if matches!(op, Op::StrictEq | Op::StrictNe) {
                    // Any other pair of values takes the helper, which compares strings too.
                    let eq = rt::strict_eq_into as *const () as usize;
                    let negate = u64::from(op == Op::StrictNe);
                    return Fast::Direct(eq, vec![Arg::Addr(x), Arg::Addr(y), Arg::Imm(negate)]);
                }
                Fast::Guarded
            }
            Op::Neg => {
                let x = self.temp(d - 1);
                self.check_tag(x, TAG_NUM, slow);
                self.a.mov_load(RAX, x.at(8));
                self.a.btc_imm(RAX, 63);
                self.a.mov_store(x.at(8), RAX);
                Fast::Guarded
            }
            Op::Not => {
                let x = self.temp(d - 1);
                self.check_tag(x, TAG_BOOL, slow);
                self.a.movzx_load8(RAX, x.at(1));
                self.a.xor32_imm8(RAX, 1);
                self.a.mov_store8(x.at(1), RAX);
                Fast::Guarded
            }
            Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                let x = self.temp(d - 1);
                self.check_tag(x, TAG_BOOL, slow);
                self.a.movzx_load8(RAX, x.at(1));
                self.a.test32(RAX, RAX);
                let cc = match op {
                    Op::JumpIfFalse(_) => CC_E,
                    _ => CC_NE,
                };
                let target = self.target(t);
                self.a.jcc(cc, target);
                Fast::Guarded
            }
            Op::JumpIfNotLt(t) | Op::JumpIfNotLe(t) | Op::JumpIfNotGt(t) | Op::JumpIfNotGe(t) => {
                let (x, y) = (self.temp(d - 2), self.temp(d - 1));
                self.check_tag(x, TAG_NUM, slow);
                self.check_tag(y, TAG_NUM, slow);
                self.a.movsd_load(0, x.at(8));
                self.a.movsd_load(1, y.at(8));
                // Jumps when the comparison is false. NaN makes every test false, so it jumps too.
                let (first, second, cc) = match op {
                    Op::JumpIfNotLt(_) => (1, 0, CC_BE),
                    Op::JumpIfNotLe(_) => (1, 0, CC_B),
                    Op::JumpIfNotGt(_) => (0, 1, CC_BE),
                    _ => (0, 1, CC_B),
                };
                self.a.ucomisd(first, second);
                let target = self.target(t);
                self.a.jcc(cc, target);
                Fast::Guarded
            }
            Op::JumpIfLtLK(s, k, t) | Op::JumpIfNotLtLK(s, k, t) => {
                let Some(c) = self.num_const(k) else {
                    return Fast::Generic;
                };
                let x = self.local(s);
                self.check_tag(x, TAG_NUM, slow);
                self.load_num(s, 0);
                self.a.mov_imm64(RAX, c.to_bits());
                self.a.movq_to_xmm(1, RAX);
                self.jump_on_less(matches!(op, Op::JumpIfLtLK(..)), t);
                Fast::Guarded
            }
            Op::JumpIfLtLL(p, q, t) | Op::JumpIfNotLtLL(p, q, t) => {
                let (x, y) = (self.local(p), self.local(q));
                self.check_tag(x, TAG_NUM, slow);
                self.check_tag(y, TAG_NUM, slow);
                self.load_num(p, 0);
                self.load_num(q, 1);
                self.jump_on_less(matches!(op, Op::JumpIfLtLL(..)), t);
                Fast::Guarded
            }
            _ => Fast::Generic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Proto;
    use std::rc::Rc;

    #[test]
    fn tags_match_the_value_layout() {
        // Native code reads a value's tag from its first byte, so the constants must match the layout.
        let tag = |v: &Value| unsafe { *(v as *const Value as *const u8) };
        assert_eq!(tag(&Value::Undef), TAG_UNDEF);
        assert_eq!(tag(&Value::Bool(true)), TAG_BOOL);
        assert_eq!(tag(&Value::Num(0.0)), TAG_NUM);
        assert_eq!(tag(&Value::arr(Vec::new())), TAG_ARR);
        assert_eq!(
            tag(&Value::Native(crate::builtins::array::push_def())),
            TAG_NATIVE
        );
    }

    /// The tag byte native code tests must be the variant order `Value` declares.
    #[test]
    fn tags_match_value_layout() {
        fn tag(v: &Value) -> u8 {
            // SAFETY: `#[repr(u8)]` puts the discriminant in the first byte of the value.
            unsafe { *(v as *const Value as *const u8) }
        }
        fn noop(_: &mut crate::vm::Vm, _: Value, _: &[Value]) -> Result<Value, Value> {
            Ok(Value::Undef)
        }
        static NOOP: crate::value::NativeDef = crate::value::NativeDef {
            name: "noop",
            func: noop,
        };
        assert_eq!(tag(&Value::Undef), TAG_UNDEF);
        assert_eq!(tag(&Value::Null), TAG_NULL);
        assert_eq!(tag(&Value::Bool(true)), TAG_BOOL);
        assert_eq!(tag(&Value::Num(1.0)), TAG_NUM);
        assert_eq!(tag(&Value::str("s")), 4);
        assert_eq!(tag(&Value::arr(Vec::new())), 5);
        assert_eq!(tag(&Value::obj()), 6);
        assert_eq!(tag(&Value::Native(&NOOP)), 8);
        assert_eq!(
            tag(&Value::Cell(Rc::new(std::cell::RefCell::new(Value::Undef)))),
            9
        );
        let owning = [4u8, 5, 6, 7, 9];
        for t in 0..10u8 {
            assert_eq!(OWNING >> t & 1 == 1, owning.contains(&t), "tag {t}");
        }
    }

    #[test]
    fn depths_follow_the_operand_stack() {
        let proto = Proto {
            name: String::new(),
            file: Rc::from(""),
            // `local = 1 + 2` followed by a conditional jump on the sum.
            code: vec![
                Op::Const(0),
                Op::Const(1),
                Op::Add,
                Op::PopToLocal(0),
                Op::GetLocal(0),
                Op::JumpIfFalse(6),
                Op::Undef,
                Op::Return,
            ],
            lines: vec![0; 8],
            consts: vec![Value::Num(1.0), Value::Num(2.0)],
            names: Vec::new(),
            hints: Vec::new(),
            global_hints: Vec::new(),
            call_entry: Default::default(),
            call_frame_len: Default::default(),
            protos: Vec::new(),
            upvals: Vec::new(),
            nparams: 0,
            has_rest: false,
            nlocals: 1,
            is_arrow: false,
            native: Default::default(),
        };
        let (depth, deepest) = depths(&proto).unwrap();
        assert_eq!(depth, [0, 1, 2, 1, 0, 1, 0, 1]);
        assert_eq!(deepest, 2);
    }

    #[test]
    fn loop_locals_get_registers_by_use() {
        let proto = Proto {
            name: String::new(),
            file: Rc::from(""),
            // A loop that counts local 1 and adds to local 0, with local 2 outside it.
            code: vec![
                Op::JumpIfNotLtLK(1, 0, 6),
                Op::IncLocal(1),
                Op::AddLK(0, 0),
                Op::PopToLocal(0),
                Op::Jump(0),
                Op::Undef,
                Op::Return,
            ],
            lines: vec![0; 7],
            consts: vec![Value::Num(0.0)],
            names: Vec::new(),
            hints: Vec::new(),
            global_hints: Vec::new(),
            call_entry: Default::default(),
            call_frame_len: Default::default(),
            protos: Vec::new(),
            upvals: Vec::new(),
            nparams: 0,
            has_rest: false,
            nlocals: 3,
            is_arrow: false,
            native: Default::default(),
        };
        // Locals 0 and 1 are each used twice in the loop, so they get the first two registers.
        assert_eq!(cached_locals(&proto), [Some(2), Some(3), None]);
    }
}
