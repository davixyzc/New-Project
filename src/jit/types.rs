//! Which slots hold numbers before each instruction, so that native code can skip the tag checks
//! of those slots. The analysis is conservative. A slot counts as a number only when every path to
//! the instruction leaves a number in it, and an operation it does not model makes every local
//! unknown.
//!
//! The slots follow the frame layout: slot `i` is local `i`, and slot `nlocals + k` is the operand
//! at depth `k`.

use super::codegen::{delta, successors};
use crate::bytecode::{Op, Proto};
use crate::value::Value;

/// For each instruction, whether each slot holds a number before it. Instructions that cannot run
/// have no entries.
pub type Numbers = Vec<Vec<bool>>;

/// Computes the number slots before each instruction. `depth` and `deepest` come from `depths`.
/// The first `params` locals are known to be numbers on entry, which is the case for a function that
/// native code calls with numbers. Other code passes 0.
pub fn number_slots(proto: &Proto, depth: &[i32], deepest: i32, params: usize) -> Numbers {
    let width = proto.nlocals as usize + deepest.max(0) as usize + 1;
    let mut before: Numbers = vec![Vec::new(); proto.code.len()];
    before[0] = vec![false; width];
    before[0][..params].fill(true);
    let mut work = vec![0];
    while let Some(ip) = work.pop() {
        let (op, d) = (proto.code[ip], depth[ip]);
        let state = before[ip].clone();
        let after = transfer(proto, op, d, &state);
        for (next, next_depth) in successors(op, ip, d) {
            let incoming = match op {
                // A taken jump keeps the value it tested on the stack.
                Op::AndJump(_) | Op::OrJump(_) | Op::NullishJump(_) if next_depth == d => {
                    state.clone()
                }
                // A handler starts with nothing known, since any instruction of its try block may
                // have run.
                Op::PushHandler(_) if next_depth == d + 1 => vec![false; width],
                _ => after.clone(),
            };
            merge(&mut before, &mut work, next, incoming);
        }
    }
    before
}

/// Joins `incoming` into the slots before `ip`. A slot stays a number only if it is one on every
/// path, so a change brings the instruction back to the work list.
fn merge(before: &mut Numbers, work: &mut Vec<usize>, ip: usize, incoming: Vec<bool>) {
    if before[ip].is_empty() {
        before[ip] = incoming;
        work.push(ip);
        return;
    }
    let mut changed = false;
    for (known, &now) in before[ip].iter_mut().zip(&incoming) {
        if *known && !now {
            *known = false;
            changed = true;
        }
    }
    if changed {
        work.push(ip);
    }
}

/// The slots after `op` runs on its fall-through path, given the slots `before` it and the operand
/// depth `d` before it.
fn transfer(proto: &Proto, op: Op, d: i32, before: &[bool]) -> Vec<bool> {
    let nlocals = proto.nlocals as usize;
    let at = |k: i32| nlocals + k.max(0) as usize;
    let number = |k: u32| matches!(proto.consts.get(k as usize), Some(Value::Num(_)));
    let mut s = before.to_vec();
    match op {
        // These only move the stack pointer, and the slots they leave keep their kind.
        Op::Pop
        | Op::Jump(_)
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
        | Op::PushHandler(_)
        | Op::PopHandler
        | Op::Return
        | Op::Throw => {}
        Op::Const(k) => s[at(d)] = number(k),
        Op::Undef | Op::Null | Op::True | Op::False | Op::This => s[at(d)] = false,
        Op::Dup => s[at(d)] = before[at(d - 1)],
        Op::Dup2 => {
            s[at(d)] = before[at(d - 2)];
            s[at(d + 1)] = before[at(d - 1)];
        }
        Op::Swap => s.swap(at(d - 1), at(d - 2)),
        Op::GetLocal(i) => s[at(d)] = before[i as usize],
        Op::SetLocal(i) | Op::PopToLocal(i) => s[i as usize] = before[at(d - 1)],
        Op::IncLocal(i) | Op::DecLocal(i) => s[i as usize] = true,
        Op::AddToLocal(i) => s[i as usize] = before[i as usize] && before[at(d - 1)],
        Op::AddLK(i, k) => s[at(d)] = before[i as usize] && number(k),
        Op::SubLK(_, k) | Op::MulLK(_, k) | Op::ModLK(_, k) => s[at(d)] = number(k),
        // `+` concatenates strings, while the other arithmetic operators always yield a number.
        Op::Add => s[at(d - 2)] = before[at(d - 2)] && before[at(d - 1)],
        Op::Sub | Op::Mul | Op::Div | Op::Mod => s[at(d - 2)] = true,
        Op::Neg | Op::ToNumber => s[at(d - 1)] = true,
        Op::Eq | Op::Ne | Op::StrictEq | Op::StrictNe | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
            s[at(d - 2)] = false
        }
        Op::Not | Op::TypeOf | Op::ToStr => s[at(d - 1)] = false,
        _ => {
            // The operation's result takes the top slot, and anything above the old top is new.
            let nd = d + delta(op);
            for k in d.min(nd - 1).max(0)..nd {
                s[at(k)] = false;
            }
            if !keeps_locals(op) {
                for known in &mut s[..nlocals] {
                    *known = false;
                }
            }
        }
    }
    s
}

/// Operations that cannot write a local slot of this frame. Calls and property access touch
/// other frames and heap objects, and captured variables live in cells.
fn keeps_locals(op: Op) -> bool {
    matches!(
        op,
        Op::Call(_)
            | Op::New(_)
            | Op::CallSpread
            | Op::NewSpread
            | Op::GetGlobal(_)
            | Op::GetGlobalOrUndef(_)
            | Op::SetGlobal(_)
            | Op::GetUpval(_)
            | Op::SetUpval(_)
            | Op::GetProp(_)
            | Op::SetProp(_)
            | Op::GetIndex
            | Op::SetIndex
            | Op::GetIndexLocal(_)
            | Op::GetIndex2Local(_)
            | Op::SetIndexLocal(_)
            | Op::SetIndex2Local(_)
            | Op::GetLocalProp(..)
            | Op::GetBoxed(_)
            | Op::SetBoxed(_)
            | Op::NewArray
            | Op::NewObject(_)
            | Op::ArrayPush
            | Op::ArrayExtend
            | Op::InitProp(_)
            | Op::ToKeys
            | Op::ToItems
            | Op::GetSuper(_)
            | Op::GetSuperCtor
            | Op::InstanceOf
            | Op::DeleteProp(_)
            | Op::DeleteIndex
            | Op::Closure(_)
            | Op::Pow
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::codegen::depths;
    use std::rc::Rc;

    fn proto(code: Vec<Op>, nlocals: u32) -> Proto {
        Proto {
            name: String::new(),
            file: Rc::from(""),
            lines: vec![0; code.len()],
            code,
            consts: vec![Value::Num(0.0)],
            names: Vec::new(),
            hints: Vec::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            nparams: 0,
            has_rest: false,
            nlocals,
            is_arrow: false,
            native: Default::default(),
        }
    }

    fn numbers(p: &Proto) -> Numbers {
        let (depth, deepest) = depths(p).unwrap();
        number_slots(p, &depth, deepest, 0)
    }

    #[test]
    fn loop_counters_stay_numbers() {
        // `sum = 0; i = 0; while (i < 0) { i++; sum += i % 0 }`, with sum and i as locals 0 and 1.
        let p = proto(
            vec![
                Op::Const(0),
                Op::PopToLocal(0),
                Op::Const(0),
                Op::PopToLocal(1),
                Op::JumpIfNotLtLK(1, 0, 9),
                Op::IncLocal(1),
                Op::ModLK(1, 0),
                Op::AddToLocal(0),
                Op::Jump(4),
                Op::Undef,
                Op::Return,
            ],
            2,
        );
        let known = numbers(&p);
        // The loop head sees both locals as numbers on the entry path and on the back edge.
        assert_eq!(known[4][..2], [true, true]);
        // The remainder on the stack is a number, so the add needs no check of it.
        assert!(known[7][2]);
        assert!(known[7][0]);
    }

    #[test]
    fn an_unknown_value_in_a_loop_is_not_a_number() {
        // `sum = undefined; i = 0; while (i < 0) { i++; sum += 1 }`.
        let p = proto(
            vec![
                Op::Undef,
                Op::PopToLocal(0),
                Op::Const(0),
                Op::PopToLocal(1),
                Op::JumpIfNotLtLK(1, 0, 8),
                Op::IncLocal(1),
                Op::AddLK(0, 0),
                Op::PopToLocal(0),
                Op::Jump(4),
                Op::Undef,
                Op::Return,
            ],
            2,
        );
        let known = numbers(&p);
        assert_eq!(known[4][..2], [false, true]);
    }
}
