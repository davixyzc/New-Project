//! Bytecode instructions and compiled function prototypes.
//!
//! The VM is a stack machine. Each function frame owns a window of the value stack:
//! `[callee, this, arg0 .. argN, locals .., temporaries ..]`. Locals that are captured
//! by closures are stored as heap cells (`Value::Cell`) and accessed via `*Boxed` ops.

use std::rc::Rc;

use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    // constants and literals
    Const(u32),
    Undef,
    Null,
    True,
    False,
    This,
    // stack manipulation
    Pop,
    Dup,
    Swap,
    Dup2,
    // locals (slot index into the frame window)
    GetLocal(u32),
    SetLocal(u32),
    NewCell(u32),
    GetBoxed(u32),
    SetBoxed(u32),
    BoxParam(u32),
    Rebox(u32),
    /// `x += rhs` on a local. Operands are [old value of x, rhs]; leaves the new value.
    ConcatLocal(u32),
    /// `x++` / `x--` on a plain local whose value is not used.
    IncLocal(u32),
    DecLocal(u32),
    // captured variables
    GetUpval(u32),
    SetUpval(u32),
    // globals (operand: constant index of the name)
    GetGlobal(u32),
    /// Like `GetGlobal`, but yields `undefined` instead of throwing (used by `typeof`).
    GetGlobalOrUndef(u32),
    SetGlobal(u32),
    ConstAssign(u32),
    // properties
    GetProp(u32),
    SetProp(u32),
    GetIndex,
    SetIndex,
    // operators
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    Neg,
    Not,
    ToNumber,
    TypeOf,
    ToStr,
    // control flow (operand: absolute instruction index)
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// Fused comparison and jump: pops two operands and jumps when the comparison is false.
    JumpIfNotLt(u32),
    JumpIfNotLe(u32),
    JumpIfNotGt(u32),
    JumpIfNotGe(u32),
    AndJump(u32),
    OrJump(u32),
    NullishJump(u32),
    // calls
    Call(u32),
    New(u32),
    Return,
    // object construction
    Closure(u32),
    NewArray,
    ArrayPush,
    /// Pops an iterable and appends its items to the array below it (spread in literals).
    ArrayExtend,
    /// Calls with arguments taken from an array on top of the stack (spread in calls).
    CallSpread,
    NewSpread,
    NewObject,
    InitProp(u32),
    ToKeys,
    // exceptions
    Throw,
    PushHandler(u32),
    PopHandler,
}

/// Describes where a closure finds one captured variable when it is created.
#[derive(Debug, Clone, Copy)]
pub struct UpvalDesc {
    /// True: the cell lives in a local slot of the enclosing frame.
    /// False: it is one of the enclosing closure's own upvalues.
    pub from_local: bool,
    pub index: u32,
}

/// A compiled function. Immutable once built and shared through `Rc`.
pub struct Proto {
    pub name: String,
    pub file: Rc<str>,
    pub code: Vec<Op>,
    /// Source line of each instruction, parallel to `code`.
    pub lines: Vec<u32>,
    pub consts: Vec<Value>,
    pub protos: Vec<Rc<Proto>>,
    pub upvals: Vec<UpvalDesc>,
    pub nparams: u32,
    pub has_rest: bool,
    pub nlocals: u32,
    pub is_arrow: bool,
}
