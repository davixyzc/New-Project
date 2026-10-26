//! Bytecode instructions and compiled function prototypes.
//!
//! The VM is a stack machine. Each function frame owns a window of the value stack:
//! `[callee, this, arg0 .. argN, locals .., temporaries ..]`. Locals that are captured
//! by closures are stored as heap cells (`Value::Cell`) and accessed via `*Boxed` ops.
//! All other locals are read and written in place. Common instruction pairs are fused
//! into superinstructions, such as `AddLK` (`local + constant`).

use std::cell::Cell;
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
    /// Pops the top of the stack into a plain local.
    PopToLocal(u32),
    NewCell(u32),
    GetBoxed(u32),
    SetBoxed(u32),
    BoxParam(u32),
    Rebox(u32),
    /// `x += rhs` on a plain local where `rhs` cannot write to `x`. Pops `rhs` and stores
    /// `x + rhs` in place, without pushing a result.
    AddToLocal(u32),
    /// `x += rhs` in any other case. Operands are [old value of x, rhs]; leaves the new value.
    ConcatLocal(u32),
    /// `x++` / `x--` on a plain local whose value is not used.
    IncLocal(u32),
    DecLocal(u32),
    /// `local.name` read directly from a plain local (operands: slot, name index).
    GetLocalProp(u32, u32),
    // superinstructions: a plain local and a constant (operands: slot, constant index)
    AddLK(u32, u32),
    SubLK(u32, u32),
    MulLK(u32, u32),
    ModLK(u32, u32),
    // captured variables
    GetUpval(u32),
    SetUpval(u32),
    // globals (operand: name index)
    GetGlobal(u32),
    /// Like `GetGlobal`, but yields `undefined` instead of throwing (used by `typeof`).
    GetGlobalOrUndef(u32),
    SetGlobal(u32),
    ConstAssign(u32),
    // properties (operand: name index)
    GetProp(u32),
    SetProp(u32),
    GetIndex,
    SetIndex,
    // indexing of a plain local (operand: slot). The array is read in place, not copied, and the keys are
    // numbers or other values pushed by the instruction's operands. Each falls back to the generic path.
    /// `local[key]`: pops the key, and pushes the element.
    GetIndexLocal(u32),
    /// `local[k1][k2]`: pops both keys, and pushes the element.
    GetIndex2Local(u32),
    /// `local[key] = value`: pops the value and the key, and pushes the value.
    SetIndexLocal(u32),
    /// `local[k1][k2] = value`: pops the value and both keys, and pushes the value.
    SetIndex2Local(u32),
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
    // bitwise operators, on 32-bit integers
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    BitNot,
    /// `key in object`: pops the object and the key, and pushes whether the object has the key.
    In,
    // control flow (operand: absolute instruction index)
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// Fused comparison and jump: pops two operands and jumps when the comparison is false.
    JumpIfNotLt(u32),
    JumpIfNotLe(u32),
    JumpIfNotGt(u32),
    JumpIfNotGe(u32),
    /// `local < constant` (operands: slot, constant index, target) and `local < local`
    /// (operands: slot, slot, target), each jumping when the test is true.
    JumpIfLtLK(u32, u32, u32),
    JumpIfLtLL(u32, u32, u32),
    /// The same two tests, jumping when they are false.
    JumpIfNotLtLK(u32, u32, u32),
    JumpIfNotLtLL(u32, u32, u32),
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
    /// Pushes an empty object; the operand is a capacity hint for its property list.
    NewObject(u32),
    InitProp(u32),
    ToKeys,
    /// Replaces a Map or a Set by the items that `for...of` walks (see `collection::items_of`). An array or a string
    /// stays as it is, and any other value that is not iterable throws.
    ToItems,
    /// `super.name`: the property `name` on the prototype of the object that the running method was defined on.
    GetSuper(u32),
    /// `super(...)`: pushes the superclass of the running constructor. The call gets the same `this`.
    GetSuperCtor,
    /// `value instanceof ctor`: pops the constructor and the value, and pushes whether the value is an instance.
    InstanceOf,
    /// `delete obj.name`: pops the object, and pushes whether the property is gone.
    DeleteProp(u32),
    /// `delete obj[key]`: pops the key and the object, and pushes whether the property is gone.
    DeleteIndex,
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
    /// Constant values (numbers and string literals).
    pub consts: Vec<Value>,
    /// Property and variable names. Shared as `Rc<str>`, so objects built from literals
    /// reuse these keys instead of copying them.
    pub names: Vec<Rc<str>>,
    /// Where each name was last found in an object, one per name. Native code tries that position first. It
    /// is only a guess: a position that does not hold the same key is skipped.
    pub hints: Vec<Cell<u32>>,
    /// Where each global name was last found, one per name. As with `hints`, it is a guess, checked before use.
    pub global_hints: Vec<Cell<u32>>,
    /// The entry of the compiled code, which native calls jump to. Zero until the function is compiled, and zero
    /// for a function with a numeric entry, whose calls run through the helpers, which try that entry first.
    pub call_entry: Cell<usize>,
    /// The stack slots the compiled frame needs, set together with `call_entry`.
    pub call_frame_len: Cell<usize>,
    pub protos: Vec<Rc<Proto>>,
    pub upvals: Vec<UpvalDesc>,
    pub nparams: u32,
    pub has_rest: bool,
    pub nlocals: u32,
    pub is_arrow: bool,
    /// Native code, compiled on first use.
    pub native: crate::jit::Slot,
}

impl Proto {
    /// The function's native code, if it has any.
    pub fn native(&self) -> Option<&crate::jit::Native> {
        self.native
            .get_or_init(|| {
                let compiled = crate::jit::compile(self);
                if let Some(code) = &compiled {
                    // A function with a numeric entry keeps its calls on the helpers, which try that entry first.
                    if !code.has_numeric_entry() {
                        self.call_entry.set(code.entry() as usize);
                        self.call_frame_len.set(code.frame_len());
                    }
                }
                compiled
            })
            .as_deref()
    }
}
