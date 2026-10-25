//! Native code for functions. The first time a function runs, its bytecode is translated to
//! x86-64 machine code that works directly on the interpreter's value stack (see `codegen`).
//! Instructions that are not plain numbers, locals or jumps call back into the VM (see `rt`),
//! so every instruction has one definition. The interpreter stays as the fallback. It runs
//! on platforms without native code, and everywhere when `FLY_JIT=0` is set.

mod codegen;
mod mem;
mod rt;
mod x64;

use std::cell::OnceCell;
use std::sync::OnceLock;

use crate::bytecode::Proto;
use crate::value::Value;
use crate::vm::Vm;
use mem::ExecMem;

/// Native code needs x86-64 and an `mmap` that can make pages executable.
const SUPPORTED: bool = cfg!(all(
    target_arch = "x86_64",
    any(target_os = "linux", target_os = "macos")
));

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
}

impl Native {
    /// The stack slots a frame of this function needs.
    pub fn frame_len(&self) -> usize {
        self.frame_len
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

/// Compiles a function to native code. `None` when native code is disabled or unavailable.
pub fn compile(proto: &Proto) -> Option<Box<Native>> {
    if !enabled() {
        return None;
    }
    let (code, offsets, frame_len) = codegen::compile(proto)?;
    let mem = ExecMem::new(&code)?;
    Some(Box::new(Native {
        mem,
        offsets,
        frame_len,
    }))
}
