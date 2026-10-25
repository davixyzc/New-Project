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

use std::rc::Rc;

use super::rt::{self, STATUS_JUMP};
use super::types::{number_slots, Numbers};
use super::x64::*;
use crate::bytecode::{Op, Proto};
use crate::value::Value;

/// Tags of `Value`, the discriminants `#[repr(u8)]` gives its variants.
const TAG_UNDEF: u8 = 0;
const TAG_NULL: u8 = 1;
const TAG_BOOL: u8 = 2;
const TAG_NUM: u8 = 3;
/// Bit `n` is set when tag `n` owns a reference (see `Value::owns_ref`).
const OWNING: u32 = (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7) | (1 << 9);

/// Compiles `proto`. Returns the machine code, the offset of each instruction's code (with one
/// extra entry for the end) and the number of stack slots a frame needs.
pub fn compile(proto: &Proto) -> Option<(Vec<u8>, Vec<usize>, usize)> {
    let (depth, deepest) = depths(proto)?;
    let types = number_slots(proto, &depth, deepest);
    let mut gen = Gen::new(proto, &depth, types);
    gen.prologue();
    gen.body();
    gen.epilogue();
    let offsets = gen.ops.iter().map(|&l| gen.a.offset(l)).collect();
    let bytes = gen.a.finish()?;
    Some((bytes, offsets, proto.nlocals as usize + deepest as usize))
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
        | Op::ArrayPush
        | Op::ArrayExtend
        | Op::InitProp(_)
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Mod
        | Op::Pow
        | Op::Eq
        | Op::Ne
        | Op::StrictEq
        | Op::StrictNe
        | Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge => -1,
        Op::SetIndex | Op::CallSpread | Op::NewSpread => -2,
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
        | Op::Neg
        | Op::Not
        | Op::ToNumber
        | Op::TypeOf
        | Op::ToStr
        | Op::ToKeys
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
fn local_uses(op: Op) -> Vec<u32> {
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
}

impl<'a> Gen<'a> {
    fn new(proto: &'a Proto, depth: &'a [i32], types: Numbers) -> Gen<'a> {
        let mut a = Asm::default();
        let ops = (0..=proto.code.len()).map(|_| a.label()).collect();
        let threw = a.label();
        let done = a.label();
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

    /// `local % c` for a positive integer `c`, by integer division. The division is exact only
    /// when the local is an integer, so anything else (NaN, fractions, huge values) takes the
    /// slow path. A zero result keeps the sign of the dividend, so -4 % 2 is -0.
    fn mod_local(&mut self, i: u32, c: i32, d: i32, slow: Label) {
        let x = self.local(i);
        let dst = self.temp(d);
        let (zero, nonzero, store) = (self.a.label(), self.a.label(), self.a.label());
        self.check_tag(x, TAG_NUM, slow);
        self.load_num(i, 0);
        self.a.cvttsd2si(RAX, 0);
        self.a.cvtsi2sd(1, RAX);
        // The integer converted back must equal x. A NaN compares unordered, which the parity check catches.
        self.a.ucomisd(1, 0);
        self.a.jcc(CC_NE, slow);
        self.a.jcc(CC_P, slow);
        self.a.test32(RAX, RAX);
        self.a.jcc(CC_E, zero);
        self.a.mov_rr32(R8, RAX);
        self.a.mov_imm32(RCX, c as u32);
        self.a.cdq();
        self.a.idiv32(RCX);
        self.a.test32(RDX, RDX);
        self.a.jcc(CC_NE, nonzero);
        self.a.test32(R8, R8);
        self.a.jcc(CC_NS, nonzero);
        // A zero remainder from a negative dividend is -0.
        self.a.mov_imm64(RAX, 1 << 63);
        self.a.movq_to_xmm(1, RAX);
        self.a.jmp(store);
        self.a.bind(nonzero);
        self.a.cvtsi2sd(1, RDX);
        self.a.jmp(store);
        // 0 % c is x itself, so -0 stays -0.
        self.a.bind(zero);
        self.a.movsd_rr(1, 0);
        self.a.bind(store);
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

    /// Calls a value helper, with its arguments in rdi, rsi and rdx.
    fn direct_call(&mut self, func: usize, args: &[Arg]) {
        const REGS: [u8; 3] = [RDI, RSI, RDX];
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

    /// Returns from the function, through its helper, which pops the frame.
    fn return_helper(&mut self, ip: usize, d: i32) {
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
            let op = self.proto.code[ip];
            self.cur = ip;
            match op {
                Op::Jump(t) => {
                    let target = self.target(t);
                    self.a.jmp(target);
                }
                Op::Return => self.return_helper(ip, d),
                Op::Call(argc) => self.call_helper(ip, d, argc, 0),
                Op::New(argc) => self.call_helper(ip, d, argc, rt::CALL_NEW),
                Op::CallSpread => self.call_helper(ip, d, 0, rt::CALL_SPREAD),
                Op::NewSpread => self.call_helper(ip, d, 0, rt::CALL_SPREAD | rt::CALL_NEW),
                _ => {
                    let slow = self.a.label();
                    match self.fast(op, d, slow) {
                        Fast::Generic => self.op_helper(ip, d),
                        Fast::Pure => {}
                        Fast::Guarded => self.slow.push((slow, ip, Slow::Op)),
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
            }
            let next = self.ops[ip + 1];
            self.a.jmp(next);
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
                let top = self.temp(d - 1);
                let get = rt::get_prop_top as *const () as usize;
                self.direct_call(get, &[Arg::Addr(top), Arg::Imm(name as u64)]);
                self.a.test8(RAX, RAX);
                self.a.jcc(CC_E, slow);
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
                let args = [Arg::Addr(src), Arg::Imm(name as u64), Arg::Addr(dst)];
                self.direct_call(get, &args);
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
                let c = match self.num_const(k) {
                    Some(c) if c >= 1.0 && c <= i32::MAX as f64 && c.fract() == 0.0 => c as i32,
                    _ => return Fast::Generic,
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
                Fast::Guarded
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
