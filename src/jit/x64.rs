//! A small x86-64 assembler with the instructions the baseline compiler uses, nothing more.
//!
//! Registers are numbered as in the encodings (`RAX` is 0, `R12` is 12). Memory operands are
//! always `[base + disp32]`, so the only special case is the SIB byte that `rsp` and `r12` need.

pub const RAX: u8 = 0;
pub const RCX: u8 = 1;
pub const RDX: u8 = 2;
pub const RBX: u8 = 3;
pub const RSP: u8 = 4;
pub const RSI: u8 = 6;
pub const RDI: u8 = 7;
pub const R8: u8 = 8;
pub const R12: u8 = 12;

/// Condition codes: the low nibble of `Jcc` and `SETcc`.
pub const CC_B: u8 = 0x2;
pub const CC_AE: u8 = 0x3;
pub const CC_E: u8 = 0x4;
pub const CC_NE: u8 = 0x5;
pub const CC_BE: u8 = 0x6;
pub const CC_A: u8 = 0x7;
pub const CC_NS: u8 = 0x9;
pub const CC_P: u8 = 0xA;
pub const CC_NP: u8 = 0xB;

/// The second opcode byte of the scalar double operations (`F2 0F xx`).
pub const SSE_ADD: u8 = 0x58;
pub const SSE_MUL: u8 = 0x59;
pub const SSE_SUB: u8 = 0x5C;
pub const SSE_DIV: u8 = 0x5E;

/// A memory operand, `[base + disp]`.
#[derive(Clone, Copy)]
pub struct Mem {
    pub base: u8,
    pub disp: i32,
}

impl Mem {
    /// The operand `extra` bytes further on.
    pub fn at(self, extra: i32) -> Mem {
        Mem {
            base: self.base,
            disp: self.disp + extra,
        }
    }
}

/// The r/m operand of an instruction: a register, or memory.
enum Rm {
    Reg(u8),
    Mem(Mem),
}

/// A position in the code. It can be used before it is bound.
#[derive(Clone, Copy)]
pub struct Label(usize);

#[derive(Default)]
pub struct Asm {
    code: Vec<u8>,
    /// The position of each label, once bound.
    bound: Vec<Option<usize>>,
    /// Places that hold a 32-bit jump displacement, and the label each one jumps to.
    fixups: Vec<(usize, Label)>,
}

impl Asm {
    pub fn label(&mut self) -> Label {
        self.bound.push(None);
        Label(self.bound.len() - 1)
    }

    pub fn bind(&mut self, label: Label) {
        self.bound[label.0] = Some(self.code.len());
    }

    /// The position a bound label refers to.
    pub fn offset(&self, label: Label) -> usize {
        self.bound[label.0].expect("label is bound")
    }

    /// The finished code with every jump resolved. `None` if a jump goes to an unbound label.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        for (at, label) in std::mem::take(&mut self.fixups) {
            let target = self.bound[label.0]?;
            let rel = target as isize - (at as isize + 4);
            self.code[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        Some(self.code)
    }

    fn rel32(&mut self, label: Label) {
        self.fixups.push((self.code.len(), label));
        self.code.extend_from_slice(&[0; 4]);
    }

    /// Emits an instruction: an optional prefix, REX when a bit of it is needed, the opcode,
    /// then ModRM and, for memory, SIB and the displacement.
    fn op(&mut self, prefix: Option<u8>, wide: bool, opcode: &[u8], reg: u8, rm: Rm) {
        let base = match rm {
            Rm::Reg(r) => r,
            Rm::Mem(m) => m.base,
        };
        if let Some(p) = prefix {
            self.code.push(p);
        }
        let rex = 0x40 | (u8::from(wide) << 3) | ((reg >> 3) << 2) | (base >> 3);
        if rex != 0x40 {
            self.code.push(rex);
        }
        self.code.extend_from_slice(opcode);
        match rm {
            Rm::Reg(r) => self.code.push(0xC0 | ((reg & 7) << 3) | (r & 7)),
            Rm::Mem(m) => {
                // Mod 10: a 32-bit displacement follows, so no base needs the rbp/r13 special case.
                self.code.push(0x80 | ((reg & 7) << 3) | (m.base & 7));
                if m.base & 7 == 4 {
                    self.code.push(0x24);
                }
                self.code.extend_from_slice(&m.disp.to_le_bytes());
            }
        }
    }

    // ----- moves ------------------------------------------------------------

    /// `mov dst, [m]` (64-bit).
    pub fn mov_load(&mut self, dst: u8, m: Mem) {
        self.op(None, true, &[0x8B], dst, Rm::Mem(m));
    }

    /// `mov [m], src` (64-bit).
    pub fn mov_store(&mut self, m: Mem, src: u8) {
        self.op(None, true, &[0x89], src, Rm::Mem(m));
    }

    /// `movzx dst32, byte [m]`.
    pub fn movzx_load8(&mut self, dst: u8, m: Mem) {
        self.op(None, false, &[0x0F, 0xB6], dst, Rm::Mem(m));
    }

    /// `mov byte [m], src8`, for `src` in al, cl, dl or bl.
    pub fn mov_store8(&mut self, m: Mem, src: u8) {
        self.op(None, false, &[0x88], src, Rm::Mem(m));
    }

    /// `mov byte [m], imm8`.
    pub fn mov8_imm(&mut self, m: Mem, imm: u8) {
        self.op(None, false, &[0xC6], 0, Rm::Mem(m));
        self.code.push(imm);
    }

    /// `cmp byte [m], imm8`.
    pub fn cmp8_imm(&mut self, m: Mem, imm: u8) {
        self.op(None, false, &[0x80], 7, Rm::Mem(m));
        self.code.push(imm);
    }

    /// `mov dst, imm64`.
    pub fn mov_imm64(&mut self, dst: u8, imm: u64) {
        self.code.push(0x48 | (dst >> 3));
        self.code.push(0xB8 | (dst & 7));
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// `lea dst, [m]`: the address of a frame slot.
    pub fn lea(&mut self, dst: u8, m: Mem) {
        self.op(None, true, &[0x8D], dst, Rm::Mem(m));
    }

    /// `mov dst32, imm32`, which also clears the upper half of the 64-bit register.
    pub fn mov_imm32(&mut self, dst: u8, imm: u32) {
        if dst >= 8 {
            self.code.push(0x41);
        }
        self.code.push(0xB8 | (dst & 7));
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// `mov dst, src` (64-bit).
    pub fn mov_rr(&mut self, dst: u8, src: u8) {
        self.op(None, true, &[0x89], src, Rm::Reg(dst));
    }

    /// `mov dst32, src32`.
    pub fn mov_rr32(&mut self, dst: u8, src: u8) {
        self.op(None, false, &[0x89], src, Rm::Reg(dst));
    }

    // ----- scalar doubles ---------------------------------------------------

    /// `movsd x, [m]`.
    pub fn movsd_load(&mut self, x: u8, m: Mem) {
        self.op(Some(0xF2), false, &[0x0F, 0x10], x, Rm::Mem(m));
    }

    /// `movsd [m], x`.
    pub fn movsd_store(&mut self, m: Mem, x: u8) {
        self.op(Some(0xF2), false, &[0x0F, 0x11], x, Rm::Mem(m));
    }

    /// `movsd dst, src` between two xmm registers.
    pub fn movsd_rr(&mut self, dst: u8, src: u8) {
        self.op(Some(0xF2), false, &[0x0F, 0x10], dst, Rm::Reg(src));
    }

    /// `addsd`, `mulsd`, `subsd` or `divsd` (`op` is one of the `SSE_` constants).
    pub fn sse(&mut self, op: u8, dst: u8, src: u8) {
        self.op(Some(0xF2), false, &[0x0F, op], dst, Rm::Reg(src));
    }

    /// `ucomisd a, b`: sets the flags as for comparing `a` with `b`. Unordered operands (NaN)
    /// set ZF, PF and CF together.
    pub fn ucomisd(&mut self, a: u8, b: u8) {
        self.op(Some(0x66), false, &[0x0F, 0x2E], a, Rm::Reg(b));
    }

    /// `cvttsd2si dst32, src`: truncates to a 32-bit integer (0x80000000 when out of range).
    pub fn cvttsd2si(&mut self, dst: u8, src: u8) {
        self.op(Some(0xF2), false, &[0x0F, 0x2C], dst, Rm::Reg(src));
    }

    /// `cvtsi2sd dst, src32`.
    pub fn cvtsi2sd(&mut self, dst: u8, src: u8) {
        self.op(Some(0xF2), false, &[0x0F, 0x2A], dst, Rm::Reg(src));
    }

    /// `movq xmm, src64`: moves the bits of a general register into an xmm register.
    pub fn movq_to_xmm(&mut self, dst: u8, src: u8) {
        self.op(Some(0x66), true, &[0x0F, 0x6E], dst, Rm::Reg(src));
    }

    // ----- integers and flags -----------------------------------------------

    /// `cdq`: sign-extends eax into edx.
    pub fn cdq(&mut self) {
        self.code.push(0x99);
    }

    /// `idiv r32`: edx:eax divided by `r`; quotient in eax, remainder in edx.
    pub fn idiv32(&mut self, r: u8) {
        self.op(None, false, &[0xF7], 7, Rm::Reg(r));
    }

    /// `test a32, b32`.
    pub fn test32(&mut self, a: u8, b: u8) {
        self.op(None, false, &[0x85], b, Rm::Reg(a));
    }

    /// `test a8, b8`, for a register whose low byte holds a result (al, cl, dl or bl).
    pub fn test8(&mut self, a: u8, b: u8) {
        self.op(None, false, &[0x84], b, Rm::Reg(a));
    }

    /// `cmp r32, imm8`.
    pub fn cmp32_imm8(&mut self, r: u8, imm: u8) {
        self.op(None, false, &[0x83], 7, Rm::Reg(r));
        self.code.push(imm);
    }

    /// `xor r32, imm8`.
    pub fn xor32_imm8(&mut self, r: u8, imm: u8) {
        self.op(None, false, &[0x83], 6, Rm::Reg(r));
        self.code.push(imm);
    }

    /// `setcc r8`, for r8 in al, cl, dl or bl.
    pub fn setcc(&mut self, cc: u8, r: u8) {
        self.op(None, false, &[0x0F, 0x90 | cc], 0, Rm::Reg(r));
    }

    /// `and dst8, src8` (al, cl, dl or bl).
    pub fn and8(&mut self, dst: u8, src: u8) {
        self.op(None, false, &[0x20], src, Rm::Reg(dst));
    }

    /// `or dst8, src8` (al, cl, dl or bl).
    pub fn or8(&mut self, dst: u8, src: u8) {
        self.op(None, false, &[0x08], src, Rm::Reg(dst));
    }

    /// `btc r64, bit`: flips one bit, here the sign bit of a double.
    pub fn btc_imm(&mut self, r: u8, bit: u8) {
        self.op(None, true, &[0x0F, 0xBA], 7, Rm::Reg(r));
        self.code.push(bit);
    }

    /// `bt mask32, bit32`: sets CF to bit `bit` of `mask`.
    pub fn bt(&mut self, mask: u8, bit: u8) {
        self.op(None, false, &[0x0F, 0xA3], bit, Rm::Reg(mask));
    }

    // ----- control flow -----------------------------------------------------

    pub fn jmp(&mut self, label: Label) {
        self.code.push(0xE9);
        self.rel32(label);
    }

    pub fn jcc(&mut self, cc: u8, label: Label) {
        self.code.extend_from_slice(&[0x0F, 0x80 | cc]);
        self.rel32(label);
    }

    /// `call r64`.
    pub fn call_reg(&mut self, r: u8) {
        self.op(None, false, &[0xFF], 2, Rm::Reg(r));
    }

    /// `jmp r64`.
    pub fn jmp_reg(&mut self, r: u8) {
        self.op(None, false, &[0xFF], 4, Rm::Reg(r));
    }

    pub fn push_reg(&mut self, r: u8) {
        if r >= 8 {
            self.code.push(0x41);
        }
        self.code.push(0x50 | (r & 7));
    }

    pub fn pop_reg(&mut self, r: u8) {
        if r >= 8 {
            self.code.push(0x41);
        }
        self.code.push(0x58 | (r & 7));
    }

    /// `add rsp, imm8`.
    pub fn add_rsp(&mut self, imm: u8) {
        self.op(None, true, &[0x83], 0, Rm::Reg(RSP));
        self.code.push(imm);
    }

    /// `sub rsp, imm8`.
    pub fn sub_rsp(&mut self, imm: u8) {
        self.op(None, true, &[0x83], 5, Rm::Reg(RSP));
        self.code.push(imm);
    }

    pub fn ret(&mut self) {
        self.code.push(0xC3);
    }

    /// `ud2`: traps. Used for instructions that cannot run.
    pub fn ud2(&mut self) {
        self.code.extend_from_slice(&[0x0F, 0x0B]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(f: impl FnOnce(&mut Asm)) -> Vec<u8> {
        let mut a = Asm::default();
        f(&mut a);
        a.code
    }

    fn frame(disp: i32) -> Mem {
        Mem { base: R12, disp }
    }

    #[test]
    fn frame_accesses_use_sib_and_disp32() {
        assert_eq!(
            encode(|a| a.movzx_load8(RAX, frame(0x10))),
            [0x41, 0x0F, 0xB6, 0x84, 0x24, 0x10, 0, 0, 0]
        );
        assert_eq!(
            encode(|a| a.mov_load(RAX, frame(8))),
            [0x49, 0x8B, 0x84, 0x24, 8, 0, 0, 0]
        );
        assert_eq!(
            encode(|a| a.mov_store(frame(8), RCX)),
            [0x49, 0x89, 0x8C, 0x24, 8, 0, 0, 0]
        );
        assert_eq!(
            encode(|a| a.movsd_load(0, frame(8))),
            [0xF2, 0x41, 0x0F, 0x10, 0x84, 0x24, 8, 0, 0, 0]
        );
        assert_eq!(
            encode(|a| a.cmp8_imm(frame(0), 3)),
            [0x41, 0x80, 0xBC, 0x24, 0, 0, 0, 0, 3]
        );
        assert_eq!(
            encode(|a| a.mov8_imm(frame(0), 3)),
            [0x41, 0xC6, 0x84, 0x24, 0, 0, 0, 0, 3]
        );
        assert_eq!(
            encode(|a| a.mov_store8(frame(1), RAX)),
            [0x41, 0x88, 0x84, 0x24, 1, 0, 0, 0]
        );
    }

    #[test]
    fn register_forms() {
        assert_eq!(encode(|a| a.ucomisd(1, 0)), [0x66, 0x0F, 0x2E, 0xC8]);
        assert_eq!(encode(|a| a.cvttsd2si(RAX, 0)), [0xF2, 0x0F, 0x2C, 0xC0]);
        assert_eq!(encode(|a| a.cvtsi2sd(1, RAX)), [0xF2, 0x0F, 0x2A, 0xC8]);
        assert_eq!(
            encode(|a| a.movq_to_xmm(1, RAX)),
            [0x66, 0x48, 0x0F, 0x6E, 0xC8]
        );
        assert_eq!(encode(|a| a.idiv32(RCX)), [0xF7, 0xF9]);
        assert_eq!(encode(|a| a.cdq()), [0x99]);
        assert_eq!(encode(|a| a.mov_rr32(R8, RAX)), [0x41, 0x89, 0xC0]);
        assert_eq!(encode(|a| a.test32(R8, R8)), [0x45, 0x85, 0xC0]);
        assert_eq!(encode(|a| a.btc_imm(RAX, 63)), [0x48, 0x0F, 0xBA, 0xF8, 63]);
        assert_eq!(encode(|a| a.bt(RCX, RAX)), [0x0F, 0xA3, 0xC1]);
        assert_eq!(encode(|a| a.setcc(CC_E, RAX)), [0x0F, 0x94, 0xC0]);
        assert_eq!(encode(|a| a.setcc(CC_NP, RCX)), [0x0F, 0x9B, 0xC1]);
        assert_eq!(encode(|a| a.and8(RAX, RCX)), [0x20, 0xC8]);
        assert_eq!(encode(|a| a.cmp32_imm8(RAX, 2)), [0x83, 0xF8, 2]);
        assert_eq!(encode(|a| a.xor32_imm8(RAX, 1)), [0x83, 0xF0, 1]);
        assert_eq!(encode(|a| a.mov_imm32(R8, 5)), [0x41, 0xB8, 5, 0, 0, 0]);
    }

    #[test]
    fn prologue_registers_and_calls() {
        assert_eq!(encode(|a| a.mov_rr(RBX, RDI)), [0x48, 0x89, 0xFB]);
        assert_eq!(encode(|a| a.mov_rr(RDI, RBX)), [0x48, 0x89, 0xDF]);
        assert_eq!(encode(|a| a.mov_rr(R12, RSI)), [0x49, 0x89, 0xF4]);
        assert_eq!(encode(|a| a.mov_rr(R12, RDX)), [0x49, 0x89, 0xD4]);
        assert_eq!(encode(|a| a.push_reg(R12)), [0x41, 0x54]);
        assert_eq!(encode(|a| a.pop_reg(R12)), [0x41, 0x5C]);
        assert_eq!(encode(|a| a.push_reg(RBX)), [0x53]);
        assert_eq!(encode(|a| a.pop_reg(RBX)), [0x5B]);
        assert_eq!(encode(|a| a.sub_rsp(8)), [0x48, 0x83, 0xEC, 8]);
        assert_eq!(encode(|a| a.add_rsp(8)), [0x48, 0x83, 0xC4, 8]);
        assert_eq!(encode(|a| a.call_reg(RAX)), [0xFF, 0xD0]);
        assert_eq!(encode(|a| a.jmp_reg(RDX)), [0xFF, 0xE2]);
        assert_eq!(encode(|a| a.ret()), [0xC3]);
    }

    #[test]
    fn jumps_resolve_forward_and_backward() {
        let mut a = Asm::default();
        let top = a.label();
        let end = a.label();
        a.bind(top);
        a.jcc(CC_NE, end);
        a.jmp(top);
        a.bind(end);
        let code = a.finish().unwrap();
        // jne +5 skips the 5-byte jmp; the jmp at 6 ends at 11, so it goes back 11 to the top.
        assert_eq!(code, [0x0F, 0x85, 5, 0, 0, 0, 0xE9, 0xF5, 0xFF, 0xFF, 0xFF]);
    }
}
