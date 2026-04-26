//! NEON implementation of chunk classification for aarch64.
//!
//! Uses a dual-class split-nibble LUT approach for byte classification.
//! Also provides `prefix_xor_neon` via PMULL (polynomial multiply).
//!
//! Note: `#[target_feature]` is NOT used on these functions so that they
//! can be inlined into `scan_chunk` / `advance_chunk`.  NEON is always
//! available on aarch64 targets, so the attribute is unnecessary.  For the
//! PMULL-dependent `prefix_xor_neon`, compile with `-C target-cpu=native`
//! or `-C target-feature=+aes` to enable the intrinsic.

#![allow(clippy::undocumented_unsafe_blocks)]

use super::ChunkClass;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// Compute prefix-XOR using `vmull_p64` (PMULL crypto extension).
///
/// # Safety
///
/// Requires aarch64 with NEON and AES/PMULL support.
/// Compile with `-C target-cpu=native` or `-C target-feature=+aes`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(crate) unsafe fn prefix_xor_neon(v: u64) -> u64 {
    let r = vmull_p64(v, !0u64);
    vgetq_lane_u64(vreinterpretq_u64_p128(r), 0)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn pack_mask64(m0: uint8x16_t, m1: uint8x16_t, m2: uint8x16_t, m3: uint8x16_t) -> u64 {
    #[repr(align(16))]
    struct Align16([u8; 16]);

    static BIT_MASK_DATA: Align16 = Align16([
        0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40,
        0x80,
    ]);

    let bit_mask = vld1q_u8(BIT_MASK_DATA.0.as_ptr());

    // Force the exact addp chain via inline asm to prevent LLVM from
    // rewriting vpaddq into uzp1/uzp2/orr sequences (which can be slower).
    let result: u64;
    core::arch::asm!(
        "and.16b {a0:v}, {m0:v}, {bm:v}",
        "and.16b {a1:v}, {m1:v}, {bm:v}",
        "and.16b {a2:v}, {m2:v}, {bm:v}",
        "and.16b {a3:v}, {m3:v}, {bm:v}",
        "addp.16b {s0:v}, {a0:v}, {a1:v}",
        "addp.16b {s1:v}, {a2:v}, {a3:v}",
        "addp.16b {s0:v}, {s0:v}, {s1:v}",
        "addp.16b {s0:v}, {s0:v}, {s0:v}",
        "fmov {out}, {s0:d}",
        m0 = in(vreg) m0,
        m1 = in(vreg) m1,
        m2 = in(vreg) m2,
        m3 = in(vreg) m3,
        bm = in(vreg) bit_mask,
        a0 = out(vreg) _,
        a1 = out(vreg) _,
        a2 = out(vreg) _,
        a3 = out(vreg) _,
        s0 = out(vreg) _,
        s1 = out(vreg) _,
        out = out(reg) result,
        options(pure, nomem, nostack),
    );
    result
}

/// Classify 64 input bytes using NEON split-nibble LUTs.
///
/// # Safety
///
/// `buf` must point to at least 64 readable bytes.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(crate) unsafe fn classify_chunk(buf: *const u8) -> ChunkClass {
    #[repr(align(16))]
    struct Align16([u8; 16]);

    static LO_LUT_DATA: Align16 = Align16([
        0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x82, 0x14, 0x01, 0xA8, 0x00,
        0x00,
    ]);
    static HI_LUT_DATA: Align16 = Align16([
        0x80, 0x00, 0x41, 0x02, 0x00, 0x0C, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ]);

    let lo_lut = vld1q_u8(LO_LUT_DATA.0.as_ptr());
    let hi_lut = vld1q_u8(HI_LUT_DATA.0.as_ptr());
    let nibble_mask = vdupq_n_u8(0x0F);
    let bs_val = vdupq_n_u8(0x5C);
    let qt_val = vdupq_n_u8(0x22);
    let op_mask = vdupq_n_u8(0x3F);
    let ws_mask = vdupq_n_u8(0xC0);

    let mut bs = core::mem::MaybeUninit::<[uint8x16_t; 4]>::uninit();
    let mut rq = core::mem::MaybeUninit::<[uint8x16_t; 4]>::uninit();
    let mut ws = core::mem::MaybeUninit::<[uint8x16_t; 4]>::uninit();
    let mut ops = core::mem::MaybeUninit::<[uint8x16_t; 4]>::uninit();

    let bs_ptr = bs.as_mut_ptr() as *mut uint8x16_t;
    let rq_ptr = rq.as_mut_ptr() as *mut uint8x16_t;
    let ws_ptr = ws.as_mut_ptr() as *mut uint8x16_t;
    let ops_ptr = ops.as_mut_ptr() as *mut uint8x16_t;

    for i in 0..4 {
        let v = vld1q_u8(buf.add(i * 16));
        let low_nibble = vandq_u8(v, nibble_mask);
        let hi_nibble = vshrq_n_u8::<4>(v);

        let sig = vandq_u8(
            vqtbl1q_u8(lo_lut, low_nibble),
            vqtbl1q_u8(hi_lut, hi_nibble),
        );

        core::ptr::write(ops_ptr.add(i), vtstq_u8(sig, op_mask));
        core::ptr::write(ws_ptr.add(i), vtstq_u8(sig, ws_mask));
        core::ptr::write(bs_ptr.add(i), vceqq_u8(v, bs_val));
        core::ptr::write(rq_ptr.add(i), vceqq_u8(v, qt_val));
    }

    let bs = bs.assume_init();
    let rq = rq.assume_init();
    let ws = ws.assume_init();
    let ops = ops.assume_init();

    ChunkClass {
        backslash: pack_mask64(bs[0], bs[1], bs[2], bs[3]),
        raw_quote: pack_mask64(rq[0], rq[1], rq[2], rq[3]),
        whitespace: pack_mask64(ws[0], ws[1], ws[2], ws[3]),
        op: pack_mask64(ops[0], ops[1], ops[2], ops[3]),
    }
}
