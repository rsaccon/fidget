//! Infrastructure for compiling down to native machine code
use dynasmrt::{
    aarch64::Assembler, dynasm, AssemblyOffset, DynasmApi, DynasmLabelApi,
    ExecutableBuffer,
};

use crate::{
    asm::AsmOp,
    eval::{
        Choice, EvalSeed, FloatEval, FloatFunc, FloatSliceEval, FloatSliceFunc,
        Interval, IntervalEval, IntervalFunc,
    },
    tape::Tape,
};

/// Number of registers available when executing natively
///
/// We can use registers v8-v15 (callee saved) and v16-v31 (caller saved)
pub const REGISTER_LIMIT: u8 = 24;

/// Offset before the first useable register
const OFFSET: u8 = 8;

/// Register written to by `CopyImm`
///
/// `IMM_REG` is selected to avoid scratch registers used by other
/// functions, e.g. interval mul / min / max
const IMM_REG: u8 = 6;

/// Converts from a tape-local register to an AArch64 register
///
/// Tape-local registers are in the range `0..REGISTER_LIMIT`, while ARM
/// registers have an offset (based on calling convention).
///
/// This uses `wrapping_add` to support immediates, which are loaded into an ARM
/// register below `OFFSET` (which is "negative" from the perspective of this
/// function).
fn reg(r: u8) -> u32 {
    let out = r.wrapping_add(OFFSET) as u32;
    assert!(out < 32);
    out
}

const CHOICE_LEFT: u32 = Choice::Left as u32;
const CHOICE_RIGHT: u32 = Choice::Right as u32;
const CHOICE_BOTH: u32 = Choice::Both as u32;

trait AssemblerT {
    fn init() -> Self;
    fn build_load(&mut self, dst_reg: u8, src_mem: u32);
    fn build_store(&mut self, dst_mem: u32, src_reg: u8);

    /// Copies the given input to `out_reg`
    fn build_input(&mut self, out_reg: u8, src_arg: u8);
    fn build_copy(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_neg(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_abs(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_recip(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_sqrt(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_square(&mut self, out_reg: u8, lhs_reg: u8);
    fn build_add(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8);
    fn build_sub(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8);
    fn build_mul(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8);
    fn build_max(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8);
    fn build_min(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8);

    /// Loads an immediate into a register, returning that register
    fn load_imm(&mut self, imm: f32) -> u8;

    fn finalize(self, out_reg: u8) -> (ExecutableBuffer, AssemblyOffset);
}

struct FloatAssembler(AssemblerData<f32>);

struct AssemblerData<T> {
    ops: Assembler,
    shape_fn: AssemblyOffset,

    /// Current offset of the stack pointer, in bytes
    mem_offset: usize,

    _p: std::marker::PhantomData<*const T>,
}

impl<T> AssemblerData<T> {
    fn check_stack(&mut self, mem_slot: u32) -> u32 {
        assert!(mem_slot >= REGISTER_LIMIT as u32);
        let mem = (mem_slot as usize - REGISTER_LIMIT as usize)
            * std::mem::size_of::<T>();

        if mem > self.mem_offset {
            // Round up to the nearest multiple of 16 bytes, for alignment
            let mem_aligned = ((mem + 15) / 16) * 16;
            let addr = u32::try_from(mem_aligned - self.mem_offset).unwrap();
            dynasm!(self.ops
                ; sub sp, sp, #(addr)
            );
            self.mem_offset = mem_aligned;
        }
        // Return the offset of the given slot, computed based on the new stack
        // pointer location in memory.
        u32::try_from(self.mem_offset - mem).unwrap()
    }
}

impl AssemblerT for FloatAssembler {
    fn init() -> Self {
        let mut ops = dynasmrt::aarch64::Assembler::new().unwrap();
        dynasm!(ops
            ; -> shape_fn:
        );
        let shape_fn = ops.offset();

        dynasm!(ops
            // Preserve frame and link register
            ; stp   x29, x30, [sp, #-16]!
            // Preserve sp
            ; mov   x29, sp
            // Preserve callee-saved floating-point registers
            ; stp   d8, d9, [sp, #-16]!
            ; stp   d10, d11, [sp, #-16]!
            ; stp   d12, d13, [sp, #-16]!
            ; stp   d14, d15, [sp, #-16]!
        );

        Self(AssemblerData {
            ops,
            shape_fn,
            mem_offset: 0,
            _p: std::marker::PhantomData,
        })
    }
    /// Reads from `src_mem` to `dst_reg`
    fn build_load(&mut self, dst_reg: u8, src_mem: u32) {
        assert!(dst_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(src_mem);
        assert!(sp_offset <= 16384);
        dynasm!(self.0.ops ; ldr S(reg(dst_reg)), [sp, #(sp_offset)])
    }
    /// Writes from `src_reg` to `dst_mem`
    fn build_store(&mut self, dst_mem: u32, src_reg: u8) {
        assert!(src_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(dst_mem);
        assert!(sp_offset <= 16384);
        dynasm!(self.0.ops ; str S(reg(src_reg)), [sp, #(sp_offset)])
    }
    /// Copies the given input to `out_reg`
    fn build_input(&mut self, out_reg: u8, src_arg: u8) {
        dynasm!(self.0.ops ; fmov S(reg(out_reg)), S(src_arg as u32));
    }
    fn build_copy(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fmov S(reg(out_reg)), S(reg(lhs_reg)))
    }
    fn build_neg(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fneg S(reg(out_reg)), S(reg(lhs_reg)))
    }
    fn build_abs(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fabs S(reg(out_reg)), S(reg(lhs_reg)))
    }
    fn build_recip(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops
            ; fmov s7, #1.0
            ; fdiv S(reg(out_reg)), s7, S(reg(lhs_reg))
        )
    }
    fn build_sqrt(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fsqrt S(reg(out_reg)), S(reg(lhs_reg)))
    }
    fn build_square(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fmul S(reg(out_reg)), S(reg(lhs_reg)), S(reg(lhs_reg)))
    }
    fn build_add(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; fadd S(reg(out_reg)), S(reg(lhs_reg)), S(reg(rhs_reg))
        )
    }
    fn build_sub(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; fsub S(reg(out_reg)), S(reg(lhs_reg)), S(reg(rhs_reg))
        )
    }
    fn build_mul(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; fmul S(reg(out_reg)), S(reg(lhs_reg)), S(reg(rhs_reg))
        )
    }
    fn build_max(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; fmax S(reg(out_reg)), S(reg(lhs_reg)), S(reg(rhs_reg))
        )
    }
    fn build_min(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; fmin S(reg(out_reg)), S(reg(lhs_reg)), S(reg(rhs_reg))
        )
    }

    /// Loads an immediate into register S4, using W9 as an intermediary
    fn load_imm(&mut self, imm: f32) -> u8 {
        let imm_u32 = imm.to_bits();
        dynasm!(self.0.ops
            ; movz w9, #(imm_u32 >> 16), lsl 16
            ; movk w9, #(imm_u32)
            ; fmov S(IMM_REG as u32), w9
        );
        IMM_REG.wrapping_sub(OFFSET)
    }

    fn finalize(mut self, out_reg: u8) -> (ExecutableBuffer, AssemblyOffset) {
        dynasm!(self.0.ops
            // Prepare our return value
            ; fmov  s0, S(reg(out_reg))
            // Restore stack space used for spills
            ; add   sp, sp, #(self.0.mem_offset as u32)
            // Restore callee-saved floating-point registers
            ; ldp   d14, d15, [sp], #16
            ; ldp   d12, d13, [sp], #16
            ; ldp   d10, d11, [sp], #16
            ; ldp   d8, d9, [sp], #16
            // Restore frame and link register
            ; ldp   x29, x30, [sp], #16
            ; ret
        );

        (self.0.ops.finalize().unwrap(), self.0.shape_fn)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Alright, here's the plan.
///
/// We're calling a function of the form
/// ```
/// # type IntervalFn =
/// extern "C" fn([f32; 2], [f32; 2], [f32; 2], *mut u8) -> [f32; 2];
/// ```
///
/// The first three arguments are `x`, `y`, and `z` intervals.  They come packed
/// into `s0-5`, and we shuffle them into SIMD registers `V0.2S`, `V1.2S`, and
/// `V2.2s` respectively.
///
/// The last argument is a pointer to the `choices` array, which is populated
/// by `min` and `max` opcodes.  It comes in the `x0` register, which is
/// unchanged by our function.
///
/// During evaluation, each SIMD register stores an interval.  `s[0]` is the
/// lower bound of the interval and `s[1]` is the upper bound.
///
/// The input tape must be planned with a <= 24 register limit.  We use hardware
/// `V8.2S` through `V32.2S` to store our tape registers, and put everything
/// else on the stack.
///
/// `V4.2S` through `V7.2S` are used for scratch values within a single opcode
/// (e.g. storing intermediate values when calculating `min` or `max`).
///
/// In general, expect to use `v4` and `v5` for intermediate (float) values,
/// and `[x,w]15` for intermediate integer values.  These are all caller-saved,
/// so we can trash them at will.
struct IntervalAssembler(AssemblerData<[f32; 2]>);

impl AssemblerT for IntervalAssembler {
    fn init() -> Self {
        let mut ops = dynasmrt::aarch64::Assembler::new().unwrap();
        dynasm!(ops
            ; -> shape_fn:
        );
        let shape_fn = ops.offset();

        dynasm!(ops
            // Preserve frame and link register
            ; stp   x29, x30, [sp, #-16]!
            // Preserve sp
            ; mov   x29, sp
            // Preserve callee-saved floating-point registers
            ; stp   d8, d9, [sp, #-16]!
            ; stp   d10, d11, [sp, #-16]!
            ; stp   d12, d13, [sp, #-16]!
            ; stp   d14, d15, [sp, #-16]!

            // Arguments are passed in S0-5; collect them into V0-1
            ; mov v0.s[1], v1.s[0]
            ; mov v1.s[0], v2.s[0]
            ; mov v1.s[1], v3.s[0]
            ; mov v2.s[0], v4.s[0]
            ; mov v2.s[1], v5.s[0]
        );

        Self(AssemblerData {
            ops,
            shape_fn,
            mem_offset: 0,
            _p: std::marker::PhantomData,
        })
    }
    /// Reads from `src_mem` to `dst_reg`
    fn build_load(&mut self, dst_reg: u8, src_mem: u32) {
        assert!(dst_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(src_mem);
        assert!(sp_offset <= 32768);
        dynasm!(self.0.ops ; ldr D(reg(dst_reg)), [sp, #(sp_offset)])
    }
    /// Writes from `src_reg` to `dst_mem`
    fn build_store(&mut self, dst_mem: u32, src_reg: u8) {
        assert!(src_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(dst_mem);
        assert!(sp_offset <= 32768);
        dynasm!(self.0.ops ; str D(reg(src_reg)), [sp, #(sp_offset)])
    }
    /// Copies the given input to `out_reg`
    fn build_input(&mut self, out_reg: u8, src_arg: u8) {
        dynasm!(self.0.ops ; fmov D(reg(out_reg)), D(src_arg as u32));
    }
    fn build_copy(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fmov D(reg(out_reg)), D(reg(lhs_reg)))
    }
    fn build_neg(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops
            ; fneg V(reg(out_reg)).s2, V(reg(lhs_reg)).s2
            ; rev64 V(reg(out_reg)).s2, V(reg(out_reg)).s2
        )
    }
    fn build_abs(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops
            // Store lhs < 0.0 in x15
            ; fcmle v4.s2, V(reg(lhs_reg)).s2, #0.0
            ; fmov x15, d4

            // Store abs(lhs) in V(reg(out_reg))
            ; fabs V(reg(out_reg)).s2, V(reg(lhs_reg)).s2

            // Check whether lhs.upper < 0
            ; tst x15, #0x1_0000_0000
            ; b.ne #24 // -> upper_lz

            // Check whether lhs.lower < 0
            ; tst x15, #0x1

            // otherwise, we're good; return the original
            ; b.eq #20 // -> end

            // if lhs.lower < 0, then the output is
            //  [0.0, max(abs(lower, upper))]
            ; movi d4, #0
            ; fmaxnmv s4, V(reg(out_reg)).s4
            ; fmov D(reg(out_reg)), d4
            // Fall through to do the swap

            // <- upper_lz
            // if upper < 0
            //   return [-upper, -lower]
            ; rev64 V(reg(out_reg)).s2, V(reg(out_reg)).s2

            // <- end
        )
    }
    fn build_recip(&mut self, out_reg: u8, lhs_reg: u8) {
        let nan_u32 = f32::NAN.to_bits();
        dynasm!(self.0.ops
            // Check whether lhs.lower > 0.0
            ; fcmgt s4, S(reg(lhs_reg)), 0.0
            ; fmov w15, s4
            ; tst w15, #0x1
            ; b.ne #40 // -> okay

            // Check whether lhs.upper < 0.0
            ; mov s4, V(reg(lhs_reg)).s[1]
            ; fcmlt s4, s4, 0.0
            ; fmov w15, s4
            ; tst w15, #0x1
            ; b.ne #20 // -> okay

            // Bad case: the division spans 0, so return NaN
            ; movz w15, #(nan_u32 >> 16), lsl 16
            ; movk w15, #(nan_u32)
            ; dup V(reg(out_reg)).s2, w15
            ; b #20 // -> end

            // <- okay
            ; fmov s4, #1.0
            ; dup v4.s2, v4.s[0]
            ; fdiv V(reg(out_reg)).s2, v4.s2, V(reg(lhs_reg)).s2
            ; rev64 V(reg(out_reg)).s2, V(reg(out_reg)).s2

            // <- end
        )
    }
    fn build_sqrt(&mut self, out_reg: u8, lhs_reg: u8) {
        let nan_u32 = f32::NAN.to_bits();
        dynasm!(self.0.ops
            // Store lhs <= 0.0 in x8
            ; fcmle v4.s2, V(reg(lhs_reg)).s2, #0.0
            ; fmov x15, d4

            // Check whether lhs.upper < 0
            ; tst x15, #0x1_0000_0000
            ; b.ne #40 // -> upper_lz

            ; tst x15, #0x1
            ; b.ne #12 // -> lower_lz

            // Happy path
            ; fsqrt V(reg(out_reg)).s2, V(reg(lhs_reg)).s2
            ; b #36 // -> end

            // <- lower_lz
            ; mov v4.s[0], V(reg(lhs_reg)).s[1]
            ; fsqrt s4, s4
            ; movi D(reg(out_reg)), #0
            ; mov V(reg(out_reg)).s[1], v4.s[0]
            ; b #16

            // <- upper_lz
            ; movz w9, #(nan_u32 >> 16), lsl 16
            ; movk w9, #(nan_u32)
            ; dup V(reg(out_reg)).s2, w9

            // <- end
        )
    }
    fn build_square(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops
            // Store lhs <= 0.0 in x15
            ; fcmle v4.s2, V(reg(lhs_reg)).s2, #0.0
            ; fmov x15, d4
            ; fmul V(reg(out_reg)).s2, V(reg(lhs_reg)).s2, V(reg(lhs_reg)).s2

            // Check whether lhs.upper <= 0.0
            ; tst x15, #0x1_0000_0000
            ; b.ne #28 // -> swap

            // Test whether lhs.lower <= 0.0
            ; tst x15, #0x1
            ; b.eq #24 // -> end

            // If the input interval straddles 0, then the
            // output is [0, max(lower**2, upper**2)]
            ; fmaxnmv s4, V(reg(out_reg)).s4
            ; movi D(reg(out_reg)), #0
            ; mov V(reg(out_reg)).s[1], v4.s[0]
            ; b #8 // -> end

            // <- swap
            ; rev64 V(reg(out_reg)).s2, V(reg(out_reg)).s2

            // <- end
        )
    }
    fn build_add(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fadd V(reg(out_reg)).s2, V(reg(lhs_reg)).s2, V(reg(rhs_reg)).s2)
    }
    fn build_sub(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            ; rev64 v4.s2, V(reg(rhs_reg)).s2
            ; fsub V(reg(out_reg)).s2, V(reg(lhs_reg)).s2, v4.s2
        )
    }
    fn build_mul(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            // Set up v4 to contain
            //  [lhs.upper, lhs.lower, lhs.lower, lhs.upper]
            // and v5 to contain
            //  [rhs.upper, rhs.lower, rhs.upper, rhs.upper]
            //
            // Multiplying them out will hit all four possible
            // combinations; then we extract the min and max
            // with vector-reducing operations
            ; rev64 v4.s2, V(reg(lhs_reg)).s2
            ; mov v4.d[1], V(reg(lhs_reg)).d[0]
            ; dup v5.d2, V(reg(rhs_reg)).d[0]

            ; fmul v4.s4, v4.s4, v5.s4
            ; fminnmv S(reg(out_reg)), v4.s4
            ; fmaxnmv s5, v4.s4
            ; mov V(reg(out_reg)).s[1], v5.s[0]
        )
    }
    fn build_max(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            // Basically the same as MinRegReg
            ; zip2 v4.s2, V(reg(lhs_reg)).s2, V(reg(rhs_reg)).s2
            ; zip1 v5.s2, V(reg(rhs_reg)).s2, V(reg(lhs_reg)).s2
            ; fcmgt v5.s2, v5.s2, v4.s2
            ; fmov x15, d5
            ; ldrb w16, [x0]

            ; tst x15, #0x1_0000_0000
            ; b.ne #24 // -> lhs

            ; tst x15, #0x1
            ; b.eq #28 // -> both

            // LHS < RHS
            ; fmov D(reg(out_reg)), D(reg(rhs_reg))
            ; orr w16, w16, #CHOICE_RIGHT
            ; b #24 // -> end

            // <- lhs (when RHS < LHS)
            ; fmov D(reg(out_reg)), D(reg(lhs_reg))
            ; orr w16, w16, #CHOICE_LEFT
            ; b #12 // -> end

            // <- both
            ; fmax V(reg(out_reg)).s2, V(reg(lhs_reg)).s2, V(reg(rhs_reg)).s2
            ; orr w16, w16, #CHOICE_BOTH

            // <- end
            ; strb w16, [x0], #1 // post-increment
        )
    }
    fn build_min(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops
            //  if lhs.upper < rhs.lower
            //      *choices++ |= CHOICE_LEFT
            //      out = lhs
            //  elif rhs.upper < lhs.lower
            //      *choices++ |= CHOICE_RIGHT
            //      out = rhs
            //  else
            //      *choices++ |= CHOICE_BOTH
            //      out = fmin(lhs, rhs)

            // v4 = [lhs.upper, rhs.upper]
            // v5 = [rhs.lower, lhs.lower]
            // This lets us do two comparisons simultaneously
            ; zip2 v4.s2, V(reg(lhs_reg)).s2, V(reg(rhs_reg)).s2
            ; zip1 v5.s2, V(reg(rhs_reg)).s2, V(reg(lhs_reg)).s2
            ; fcmgt v5.s2, v5.s2, v4.s2
            ; fmov x15, d5
            ; ldrb w16, [x0]

            ; tst x15, #0x1_0000_0000
            ; b.ne #24 // -> rhs

            ; tst x15, #0x1
            ; b.eq #28 // -> both

            // Fallthrough: LHS < RHS
            ; fmov D(reg(out_reg)), D(reg(lhs_reg))
            ; orr w16, w16, #CHOICE_LEFT
            ; b #24 // -> end

            // <- rhs (for when RHS < LHS)
            ; fmov D(reg(out_reg)), D(reg(rhs_reg))
            ; orr w16, w16, #CHOICE_RIGHT
            ; b #12

            // <- both
            ; fmin V(reg(out_reg)).s2, V(reg(lhs_reg)).s2, V(reg(rhs_reg)).s2
            ; orr w16, w16, #CHOICE_BOTH

            // <- end
            ; strb w16, [x0], #1 // post-increment
        )
    }

    /// Loads an immediate into register S4, using W9 as an intermediary
    fn load_imm(&mut self, imm: f32) -> u8 {
        let imm_u32 = imm.to_bits();
        dynasm!(self.0.ops
            ; movz w15, #(imm_u32 >> 16), lsl 16
            ; movk w15, #(imm_u32)
            ; dup V(IMM_REG as u32).s2, w15
        );
        IMM_REG.wrapping_sub(OFFSET)
    }

    fn finalize(mut self, out_reg: u8) -> (ExecutableBuffer, AssemblyOffset) {
        dynasm!(self.0.ops
            // Prepare our return value
            ; mov  s0, V(reg(out_reg)).s[0]
            ; mov  s1, V(reg(out_reg)).s[1]
            // Restore stack space used for spills
            ; add   sp, sp, #(self.0.mem_offset as u32)
            // Restore callee-saved floating-point registers
            ; ldp   d14, d15, [sp], #16
            ; ldp   d12, d13, [sp], #16
            ; ldp   d10, d11, [sp], #16
            ; ldp   d8, d9, [sp], #16
            // Restore frame and link register
            ; ldp   x29, x30, [sp], #16
            ; ret
        );

        (self.0.ops.finalize().unwrap(), self.0.shape_fn)
    }
}

struct VecAssembler(AssemblerData<[f32; 4]>);
impl AssemblerT for VecAssembler {
    fn init() -> Self {
        let mut ops = dynasmrt::aarch64::Assembler::new().unwrap();
        dynasm!(ops
            ; -> shape_fn:
        );
        let shape_fn = ops.offset();

        dynasm!(ops
            // Preserve frame and link register
            ; stp   x29, x30, [sp, #-16]!
            // Preserve sp
            ; mov   x29, sp
            // Preserve callee-saved floating-point registers
            ; stp   d8, d9, [sp, #-16]!
            ; stp   d10, d11, [sp, #-16]!
            ; stp   d12, d13, [sp, #-16]!
            ; stp   d14, d15, [sp, #-16]!

            // We're actually loading two f32s, but we can pretend they're
            // doubles in order to move 64 bits at a time
            ; ldp d0, d1, [x0]
            ; mov v0.d[1], v1.d[0]
            ; ldp d1, d2, [x1]
            ; mov v1.d[1], v2.d[0]
            ; ldp d2, d3, [x2]
            ; mov v2.d[1], v3.d[0]

            ; fmov v8.s4, #1.0
            ; fmov v9.s4, #1.0
            ; fmov v10.s4, #1.0
            ; fmov v11.s4, #1.0
            ; fmov v12.s4, #1.0
            ; fmov v13.s4, #1.0
            ; fmov v14.s4, #1.0
            ; fmov v15.s4, #1.0
            ; fmov v16.s4, #1.0
            ; fmov v17.s4, #1.0
            ; fmov v18.s4, #1.0
            ; fmov v19.s4, #1.0
            ; fmov v20.s4, #1.0
            ; fmov v21.s4, #1.0
            ; fmov v22.s4, #1.0
            ; fmov v23.s4, #1.0
            ; fmov v24.s4, #1.0
            ; fmov v25.s4, #1.0
            ; fmov v26.s4, #1.0
            ; fmov v27.s4, #1.0
            ; fmov v28.s4, #1.0
            ; fmov v29.s4, #1.0
            ; fmov v30.s4, #1.0
            ; fmov v31.s4, #1.0
        );

        Self(AssemblerData {
            ops,
            shape_fn,
            mem_offset: 0,
            _p: std::marker::PhantomData,
        })
    }
    /// Reads from `src_mem` to `dst_reg`, using D4 as an intermediary
    fn build_load(&mut self, dst_reg: u8, src_mem: u32) {
        assert!(dst_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(src_mem);
        if sp_offset >= 512 {
            assert!(sp_offset < 4096);
            dynasm!(self.0.ops
                ; add x9, sp, #(sp_offset)
                ; ldp D(reg(dst_reg)), d4, [x9]
                ; mov V(reg(dst_reg)).d[1], v4.d[0]
            )
        } else {
            dynasm!(self.0.ops
                ; ldp D(reg(dst_reg)), d4, [sp, #(sp_offset)]
                ; mov V(reg(dst_reg)).d[1], v4.d[0]
            )
        }
    }

    /// Writes from `src_reg` to `dst_mem`, using D4 as an intermediary
    fn build_store(&mut self, dst_mem: u32, src_reg: u8) {
        assert!(src_reg < REGISTER_LIMIT);
        let sp_offset = self.0.check_stack(dst_mem);
        if sp_offset >= 512 {
            assert!(sp_offset < 4096);
            dynasm!(self.0.ops
                ; add x9, sp, #(sp_offset)
                ; mov v4.d[0], V(reg(src_reg)).d[1]
                ; stp D(reg(src_reg)), d4, [x9]
            )
        } else {
            dynasm!(self.0.ops
                ; mov v4.d[0], V(reg(src_reg)).d[1]
                ; stp D(reg(src_reg)), d4, [sp, #(sp_offset)]
            )
        }
    }
    /// Copies the given input to `out_reg`
    fn build_input(&mut self, out_reg: u8, src_arg: u8) {
        dynasm!(self.0.ops ; mov V(reg(out_reg)).b16, V(src_arg as u32).b16);
    }
    fn build_copy(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; mov V(reg(out_reg)).b16, V(reg(lhs_reg)).b16)
    }
    fn build_neg(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fneg V(reg(out_reg)).s4, V(reg(lhs_reg)).s4)
    }
    fn build_abs(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fabs V(reg(out_reg)).s4, V(reg(lhs_reg)).s4)
    }
    fn build_recip(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops
            ; fmov s7, #1.0
            ; dup v7.s4, v7.s[0]
            ; fdiv V(reg(out_reg)).s4, v7.s4, V(reg(lhs_reg)).s4
        )
    }
    fn build_sqrt(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fsqrt V(reg(out_reg)).s4, V(reg(lhs_reg)).s4)
    }
    fn build_square(&mut self, out_reg: u8, lhs_reg: u8) {
        dynasm!(self.0.ops ; fmul V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(lhs_reg)).s4)
    }
    fn build_add(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fadd V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(rhs_reg)).s4)
    }
    fn build_sub(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fsub V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(rhs_reg)).s4)
    }
    fn build_mul(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fmul V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(rhs_reg)).s4)
    }
    fn build_max(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fmax V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(rhs_reg)).s4)
    }
    fn build_min(&mut self, out_reg: u8, lhs_reg: u8, rhs_reg: u8) {
        dynasm!(self.0.ops ; fmin V(reg(out_reg)).s4, V(reg(lhs_reg)).s4, V(reg(rhs_reg)).s4)
    }

    /// Loads an immediate into register V4, using W9 as an intermediary
    fn load_imm(&mut self, imm: f32) -> u8 {
        let imm_u32 = imm.to_bits();
        dynasm!(self.0.ops
            ; movz w9, #(imm_u32 >> 16), lsl 16
            ; movk w9, #(imm_u32)
            ; dup V(IMM_REG as u32).s4, w9
        );
        IMM_REG.wrapping_sub(OFFSET)
    }

    fn finalize(mut self, out_reg: u8) -> (ExecutableBuffer, AssemblyOffset) {
        dynasm!(self.0.ops
            // Prepare our return value, writing to the pointer in x3
            // It's fine to overwrite X at this point in V0, since we're not
            // using it anymore.
            ; mov v0.d[0], V(reg(out_reg)).d[1]
            ; stp D(reg(out_reg)), d0, [x3]

            // Restore stack space used for spills
            ; add   sp, sp, #(self.0.mem_offset as u32)
            // Restore callee-saved floating-point registers
            ; ldp   d14, d15, [sp], #16
            ; ldp   d12, d13, [sp], #16
            ; ldp   d10, d11, [sp], #16
            ; ldp   d8, d9, [sp], #16
            // Restore frame and link register
            ; ldp   x29, x30, [sp], #16
            ; ret
        );

        (self.0.ops.finalize().unwrap(), self.0.shape_fn)
    }
}

////////////////////////////////////////////////////////////////////////////////

fn build_asm_fn<A: AssemblerT>(
    i: impl Iterator<Item = AsmOp>,
) -> (ExecutableBuffer, *const u8) {
    let mut asm = A::init();

    for op in i {
        use AsmOp::*;
        match op {
            Load(reg, mem) => {
                asm.build_load(reg, mem);
            }
            Store(reg, mem) => {
                asm.build_store(mem, reg);
            }
            Input(out, i) => {
                asm.build_input(out, i);
            }
            NegReg(out, arg) => {
                asm.build_neg(out, arg);
            }
            AbsReg(out, arg) => {
                asm.build_abs(out, arg);
            }
            RecipReg(out, arg) => {
                asm.build_recip(out, arg);
            }
            SqrtReg(out, arg) => {
                asm.build_sqrt(out, arg);
            }
            CopyReg(out, arg) => {
                asm.build_copy(out, arg);
            }
            SquareReg(out, arg) => {
                asm.build_square(out, arg);
            }
            AddRegReg(out, lhs, rhs) => {
                asm.build_add(out, lhs, rhs);
            }
            MulRegReg(out, lhs, rhs) => {
                asm.build_mul(out, lhs, rhs);
            }
            SubRegReg(out, lhs, rhs) => {
                asm.build_sub(out, lhs, rhs);
            }
            MinRegReg(out, lhs, rhs) => {
                asm.build_min(out, lhs, rhs);
            }
            MaxRegReg(out, lhs, rhs) => {
                asm.build_max(out, lhs, rhs);
            }
            AddRegImm(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_add(out, arg, reg);
            }
            MulRegImm(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_mul(out, arg, reg);
            }
            SubImmReg(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_sub(out, reg, arg);
            }
            SubRegImm(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_sub(out, arg, reg);
            }
            MinRegImm(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_min(out, arg, reg);
            }
            MaxRegImm(out, arg, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_max(out, arg, reg);
            }
            CopyImm(out, imm) => {
                let reg = asm.load_imm(imm);
                asm.build_copy(out, reg);
            }
        }
    }

    let (buf, shape_fn) = asm.finalize(0);
    let fn_pointer = buf.ptr(shape_fn);
    (buf, fn_pointer)
}

////////////////////////////////////////////////////////////////////////////////

/// Handle owning a JIT-compiled float function
pub struct JitFloatFunc {
    _buf: dynasmrt::ExecutableBuffer,
    fn_pointer: *const u8,
}

impl<'a> FloatFunc<'a> for JitFloatFunc {
    type Evaluator = JitFloatEval<'a>;

    /// Returns an evaluator, bound to the lifetime of the `JitFloatFunc`
    fn get_evaluator(&self) -> Self::Evaluator {
        JitFloatEval {
            fn_float: unsafe { std::mem::transmute(self.fn_pointer) },
            _p: std::marker::PhantomData,
        }
    }
}

impl JitFloatFunc {
    pub fn from_tape(t: &Tape) -> JitFloatFunc {
        let (buf, fn_pointer) = build_asm_fn::<FloatAssembler>(t.iter_asm());
        JitFloatFunc {
            _buf: buf,
            fn_pointer,
        }
    }
}

/// Handle owning a JIT-compiled interval function
///
/// This handle additionally borrows the input `Tape`, which allows us to
/// compute simpler tapes based on interval evaluation results.
pub struct JitIntervalFunc<'a> {
    _buf: dynasmrt::ExecutableBuffer,
    fn_pointer: *const u8,
    choice_count: usize,
    tape: &'a Tape,
}
unsafe impl Sync for JitIntervalFunc<'_> {}

impl<'a> JitIntervalFunc<'a> {
    pub fn from_tape(t: &'a Tape) -> Self {
        let (buf, fn_pointer) = build_asm_fn::<IntervalAssembler>(t.iter_asm());
        JitIntervalFunc {
            choice_count: t.choice_count(),
            tape: t,
            _buf: buf,
            fn_pointer,
        }
    }
}

impl<'a> IntervalFunc<'a> for JitIntervalFunc<'a> {
    type Evaluator = JitIntervalEval<'a>;

    /// Returns an evaluator, bound to the lifetime of the
    /// `JitIntervalFunc`
    fn get_evaluator(&self) -> JitIntervalEval<'a> {
        JitIntervalEval {
            fn_interval: unsafe { std::mem::transmute(self.fn_pointer) },
            choices: vec![Choice::Both; self.choice_count],
            choices_raw: vec![0u8; self.choice_count],
            tape: self.tape,
            _p: std::marker::PhantomData,
        }
    }
}

pub enum JitEvalSeed {}
impl<'a> EvalSeed<'a> for JitEvalSeed {
    type IntervalFunc = JitIntervalFunc<'a>;
    type FloatSliceFunc = JitVecFunc;
    fn from_tape_i(t: &Tape) -> JitIntervalFunc {
        JitIntervalFunc::from_tape(t)
    }
    fn from_tape_s(t: &Tape) -> JitVecFunc {
        JitVecFunc::from_tape(t)
    }
}

/// Handle owning a JIT-compiled vectorized (4x) float function
pub struct JitVecFunc {
    _buf: dynasmrt::ExecutableBuffer,
    fn_pointer: *const u8,
}

impl<'a> FloatSliceFunc<'a> for JitVecFunc {
    type Evaluator = JitVecEval<'a>;

    /// Returns an evaluator, bound to the lifetime of the `JitVecFunc`
    fn get_evaluator(&self) -> Self::Evaluator {
        JitVecEval {
            fn_vec: unsafe { std::mem::transmute(self.fn_pointer) },
            _p: std::marker::PhantomData,
        }
    }
}

impl JitVecFunc {
    pub fn from_tape(t: &Tape) -> JitVecFunc {
        let (buf, fn_pointer) = build_asm_fn::<VecAssembler>(t.iter_asm());
        JitVecFunc {
            _buf: buf,
            fn_pointer,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Evaluator for a JIT-compiled function taking `f32` values
///
/// The lifetime of this `struct` is bound to an `JitFloatFunc`, which owns
/// the underlying executable memory.
pub struct JitFloatEval<'asm> {
    fn_float: unsafe extern "C" fn(f32, f32, f32) -> f32,
    _p: std::marker::PhantomData<&'asm ()>,
}

impl<'a> FloatEval<'a> for JitFloatEval<'a> {
    fn eval_f(&mut self, x: f32, y: f32, z: f32) -> f32 {
        unsafe { (self.fn_float)(x, y, z) }
    }
}

/// Evaluator for a JIT-compiled function taking `[f32; 2]` intervals
///
/// The lifetime of this `struct` is bound to an `JitIntervalFunc`, which
/// owns the underlying executable memory.
pub struct JitIntervalEval<'asm> {
    fn_interval: unsafe extern "C" fn(
        [f32; 2], // X
        [f32; 2], // Y
        [f32; 2], // Z
        *mut u8,  // choices
    ) -> [f32; 2],
    choices_raw: Vec<u8>,
    choices: Vec<Choice>,
    tape: &'asm Tape,
    _p: std::marker::PhantomData<&'asm ()>,
}

impl<'a> IntervalEval<'a> for JitIntervalEval<'a> {
    fn reset_choices(&mut self) {
        self.choices_raw.fill(0);
    }

    fn load_choices(&mut self) {
        for (out, c) in self.choices.iter_mut().zip(self.choices_raw.iter()) {
            *out = match c {
                0 => Choice::Unknown,
                1 => Choice::Left,
                2 => Choice::Right,
                3 => Choice::Both,
                _ => panic!("invalid choice {}", c),
            }
        }
    }

    /// Evaluates an interval
    fn eval_i_inner<I: Into<Interval>>(
        &mut self,
        x: I,
        y: I,
        z: I,
    ) -> Interval {
        let x: Interval = x.into();
        let y: Interval = y.into();
        let z: Interval = z.into();
        let out = unsafe {
            (self.fn_interval)(
                [x.lower(), x.upper()],
                [y.lower(), y.upper()],
                [z.lower(), z.upper()],
                self.choices_raw.as_mut_ptr(),
            )
        };
        Interval::new(out[0], out[1])
    }

    /// Returns a simplified tape based on `self.choices`
    ///
    /// The choices array should have been calculated during the last interval
    /// evaluation.
    fn simplify(&self) -> Tape {
        self.tape.simplify(&self.choices)
    }
}

/// Evaluator for a JIT-compiled function taking `[f32; 4]` SIMD values
///
/// The lifetime of this `struct` is bound to an `JitVecFunc`, which owns
/// the underlying executable memory.
pub struct JitVecEval<'asm> {
    fn_vec: unsafe extern "C" fn(*const f32, *const f32, *const f32, *mut f32),
    _p: std::marker::PhantomData<&'asm ()>,
}

impl<'a> JitVecEval<'a> {
    fn eval_v(&mut self, x: [f32; 4], y: [f32; 4], z: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0; 4];
        unsafe {
            (self.fn_vec)(x.as_ptr(), y.as_ptr(), z.as_ptr(), out.as_mut_ptr())
        }
        out
    }
}

impl<'a> FloatSliceEval<'a> for JitVecEval<'a> {
    fn eval_s(&mut self, xs: &[f32], ys: &[f32], zs: &[f32], out: &mut [f32]) {
        for i in 0.. {
            let i = i * 4;
            let mut x = [0.0; 4];
            let mut y = [0.0; 4];
            let mut z = [0.0; 4];
            for j in 0..4 {
                x[j] = match xs.get(i + j) {
                    Some(x) => *x,
                    None => return,
                };
                y[j] = match ys.get(i + j) {
                    Some(y) => *y,
                    None => return,
                };
                z[j] = match zs.get(i + j) {
                    Some(z) => *z,
                    None => return,
                };
            }
            let v = self.eval_v(x, y, z);
            for j in 0..4 {
                match out.get_mut(i + j) {
                    Some(o) => *o = v[j],
                    None => return,
                }
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;

    #[test]
    fn test_dynasm() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();
        let two = ctx.constant(2.5);
        let y2 = ctx.mul(y, two).unwrap();
        let sum = ctx.add(x, y2).unwrap();

        let tape = ctx.get_tape(sum, REGISTER_LIMIT);
        let jit = JitFloatFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_f(1.0, 2.0, 0.0), 6.0);
    }

    #[test]
    fn test_interval() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();

        let tape = ctx.get_tape(x, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [2.0, 3.0]), [0.0, 1.0].into());
        assert_eq!(eval.eval_i_xy([1.0, 5.0], [2.0, 3.0]), [1.0, 5.0].into());

        let tape = ctx.get_tape(y, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [2.0, 3.0]), [2.0, 3.0].into());
        assert_eq!(eval.eval_i_xy([1.0, 5.0], [4.0, 5.0]), [4.0, 5.0].into());
    }

    #[test]
    fn test_i_abs() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let abs_x = ctx.abs(x).unwrap();

        let tape = ctx.get_tape(abs_x, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [0.0, 1.0].into());
        assert_eq!(eval.eval_i_x([1.0, 5.0]), [1.0, 5.0].into());
        assert_eq!(eval.eval_i_x([-2.0, 5.0]), [0.0, 5.0].into());
        assert_eq!(eval.eval_i_x([-6.0, 5.0]), [0.0, 6.0].into());
        assert_eq!(eval.eval_i_x([-6.0, -1.0]), [1.0, 6.0].into());

        let y = ctx.y();
        let abs_y = ctx.abs(y).unwrap();
        let sum = ctx.add(abs_x, abs_y).unwrap();
        let tape = ctx.get_tape(sum, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [0.0, 1.0]), [0.0, 2.0].into());
        assert_eq!(eval.eval_i_xy([1.0, 5.0], [-2.0, 3.0]), [1.0, 8.0].into());
        assert_eq!(eval.eval_i_xy([1.0, 5.0], [-4.0, 3.0]), [1.0, 9.0].into());
    }

    #[test]
    fn test_i_sqrt() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let sqrt_x = ctx.sqrt(x).unwrap();

        let tape = ctx.get_tape(sqrt_x, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [0.0, 1.0].into());
        assert_eq!(eval.eval_i_x([0.0, 4.0]), [0.0, 2.0].into());
        assert_eq!(eval.eval_i_x([-2.0, 4.0]), [0.0, 2.0].into());
        let nanan = eval.eval_i_x([-2.0, -1.0]);
        assert!(nanan.lower().is_nan());
        assert!(nanan.upper().is_nan());
    }

    #[test]
    fn test_i_square() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let sqrt_x = ctx.square(x).unwrap();

        let tape = ctx.get_tape(sqrt_x, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [0.0, 1.0].into());
        assert_eq!(eval.eval_i_x([0.0, 4.0]), [0.0, 16.0].into());
        assert_eq!(eval.eval_i_x([2.0, 4.0]), [4.0, 16.0].into());
        assert_eq!(eval.eval_i_x([-2.0, 4.0]), [0.0, 16.0].into());
        assert_eq!(eval.eval_i_x([-6.0, -2.0]), [4.0, 36.0].into());
        assert_eq!(eval.eval_i_x([-6.0, 1.0]), [0.0, 36.0].into());
    }

    #[test]
    fn test_i_mul() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();
        let mul = ctx.mul(x, y).unwrap();

        let tape = ctx.get_tape(mul, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [0.0, 1.0]), [0.0, 1.0].into());
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [0.0, 2.0]), [0.0, 2.0].into());
        assert_eq!(eval.eval_i_xy([-2.0, 1.0], [0.0, 1.0]), [-2.0, 1.0].into());
        assert_eq!(
            eval.eval_i_xy([-2.0, -1.0], [-5.0, -4.0]),
            [4.0, 10.0].into()
        );
        assert_eq!(
            eval.eval_i_xy([-3.0, -1.0], [-2.0, 6.0]),
            [-18.0, 6.0].into()
        );
    }

    #[test]
    fn test_i_mul_imm() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let two = ctx.constant(2.0);
        let mul = ctx.mul(x, two).unwrap();
        let tape = ctx.get_tape(mul, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [0.0, 2.0].into());
        assert_eq!(eval.eval_i_x([1.0, 2.0]), [2.0, 4.0].into());

        let neg_three = ctx.constant(-3.0);
        let mul = ctx.mul(x, neg_three).unwrap();
        let tape = ctx.get_tape(mul, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [-3.0, 0.0].into());
        assert_eq!(eval.eval_i_x([1.0, 2.0]), [-6.0, -3.0].into());
    }

    #[test]
    fn test_i_sub() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();
        let sub = ctx.sub(x, y).unwrap();

        let tape = ctx.get_tape(sub, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [0.0, 1.0]), [-1.0, 1.0].into());
        assert_eq!(eval.eval_i_xy([0.0, 1.0], [0.0, 2.0]), [-2.0, 1.0].into());
        assert_eq!(eval.eval_i_xy([-2.0, 1.0], [0.0, 1.0]), [-3.0, 1.0].into());
        assert_eq!(
            eval.eval_i_xy([-2.0, -1.0], [-5.0, -4.0]),
            [2.0, 4.0].into()
        );
        assert_eq!(
            eval.eval_i_xy([-3.0, -1.0], [-2.0, 6.0]),
            [-9.0, 1.0].into()
        );
    }

    #[test]
    fn test_i_sub_imm() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let two = ctx.constant(2.0);
        let sub = ctx.sub(x, two).unwrap();
        let tape = ctx.get_tape(sub, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [-2.0, -1.0].into());
        assert_eq!(eval.eval_i_x([1.0, 2.0]), [-1.0, 0.0].into());

        let neg_three = ctx.constant(-3.0);
        let sub = ctx.sub(neg_three, x).unwrap();
        let tape = ctx.get_tape(sub, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(eval.eval_i_x([0.0, 1.0]), [-4.0, -3.0].into());
        assert_eq!(eval.eval_i_x([1.0, 2.0]), [-5.0, -4.0].into());
    }

    #[test]
    fn test_i_recip() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let recip = ctx.recip(x).unwrap();
        let tape = ctx.get_tape(recip, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();

        let nanan = eval.eval_i_x([0.0, 1.0]);
        assert!(nanan.lower().is_nan());
        assert!(nanan.upper().is_nan());

        let nanan = eval.eval_i_x([-1.0, 0.0]);
        assert!(nanan.lower().is_nan());
        assert!(nanan.upper().is_nan());

        let nanan = eval.eval_i_x([-2.0, 3.0]);
        assert!(nanan.lower().is_nan());
        assert!(nanan.upper().is_nan());

        assert_eq!(eval.eval_i_x([-2.0, -1.0]), [-1.0, -0.5].into());
        assert_eq!(eval.eval_i_x([1.0, 2.0]), [0.5, 1.0].into());
    }

    #[test]
    fn test_i_min() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();
        let min = ctx.min(x, y).unwrap();

        let tape = ctx.get_tape(min, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_i([0.0, 1.0], [0.5, 1.5], [0.0; 2]),
            [0.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Both]);

        assert_eq!(
            eval.eval_i([0.0, 1.0], [2.0, 3.0], [0.0; 2]),
            [0.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 1.0], [0.0; 2]),
            [0.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Right]);
    }

    #[test]
    fn test_i_min_imm() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let one = ctx.constant(1.0);
        let min = ctx.min(x, one).unwrap();

        let tape = ctx.get_tape(min, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_i([0.0, 1.0], [0.0; 2], [0.0; 2]),
            [0.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Both]);

        assert_eq!(
            eval.eval_i([-1.0, 0.0], [0.0; 2], [0.0; 2]),
            [-1.0, 0.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0; 2], [0.0; 2]),
            [1.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Right]);
    }

    #[test]
    fn test_i_max() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();
        let max = ctx.max(x, y).unwrap();

        let tape = ctx.get_tape(max, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_i([0.0, 1.0], [0.5, 1.5], [0.0; 2],),
            [0.5, 1.5].into()
        );
        assert_eq!(eval.choices, vec![Choice::Both]);

        assert_eq!(
            eval.eval_i([0.0, 1.0], [2.0, 3.0], [0.0; 2]),
            [2.0, 3.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Right]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 1.0], [0.0; 2],),
            [2.0, 3.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left]);

        let z = ctx.z();
        let max_xy_z = ctx.max(max, z).unwrap();
        let tape = ctx.get_tape(max_xy_z, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 1.0], [4.0, 5.0]),
            [4.0, 5.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left, Choice::Right]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 1.0], [1.0, 4.0]),
            [2.0, 4.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left, Choice::Both]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 1.0], [1.0, 1.5]),
            [2.0, 3.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left, Choice::Left]);
    }

    #[test]
    fn test_i_max_imm() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let one = ctx.constant(1.0);
        let max = ctx.max(x, one).unwrap();

        let tape = ctx.get_tape(max, REGISTER_LIMIT);
        let jit = JitIntervalFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_i([0.0, 2.0], [0.0, 0.0], [0.0, 0.0]),
            [1.0, 2.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Both]);

        assert_eq!(
            eval.eval_i([-1.0, 0.0], [0.0, 0.0], [0.0, 0.0]),
            [1.0, 1.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Right]);

        assert_eq!(
            eval.eval_i([2.0, 3.0], [0.0, 0.0], [0.0, 0.0]),
            [2.0, 3.0].into()
        );
        assert_eq!(eval.choices, vec![Choice::Left]);
    }

    #[test]
    fn test_vectorized() {
        let mut ctx = Context::new();
        let x = ctx.x();
        let y = ctx.y();

        let tape = ctx.get_tape(x, REGISTER_LIMIT);
        let jit = JitVecFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_v(
                [0.0, 1.0, 2.0, 3.0],
                [3.0, 2.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 100.0]
            ),
            [0.0, 1.0, 2.0, 3.0]
        );

        let two = ctx.constant(2.0);
        let mul = ctx.mul(y, two).unwrap();
        let tape = ctx.get_tape(mul, REGISTER_LIMIT);
        let jit = JitVecFunc::from_tape(&tape);
        let mut eval = jit.get_evaluator();
        assert_eq!(
            eval.eval_v(
                [0.0, 1.0, 2.0, 3.0],
                [3.0, 2.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 100.0]
            ),
            [6.0, 4.0, 2.0, 0.0]
        );
    }
}
