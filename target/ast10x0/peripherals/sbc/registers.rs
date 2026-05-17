// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 ECDSA low-level register access (Confined-`unsafe` MMIO façade).
//!
//! The ECDSA/ECC engine has no dedicated PAC block: it is part of the Secure
//! Boot Controller, exposed by `ast1060_pac` as the `secure` peripheral
//! (`secure::RegisterBlock`, base `0x7e6f_2000`). `secure014`/`0b4`/`0bc` are
//! PAC-named; the mode register `0x7c`, the curve-parameter window
//! `0xa00–0xac0`, and the engine scratch-RAM (`SBC_SRAM_BASE`) are not
//! modelled by the PAC and are reached by raw confined offset access here and
//! nowhere else (goal.md P5-OPEN-B).
//!
//! Behavior reproduces the pinned authority
//! `plans/zephyr-reference/ecdsa_aspeed.c @ cfe94dc` (goal.md §1.2). All
//! `unsafe`, raw offsets, and PAC types stay confined below this façade.

use core::marker::PhantomData;
use core::ptr::{read_volatile, write_volatile};

use ast1060_pac as device;

use super::constants::{SBC_SRAM_BASE, RESET_SETTLE_NS, TRIGGER_HOLD_NS};

// Engine MMIO offsets (relative to the SBC `secure` base).
const OFF_MODE: usize = 0x7c; // mode/gate word (PAC-unmodelled)
const PAR_GX: usize = 0xa00; // P-384 domain params source window
const PAR_GY: usize = 0xa40;
const PAR_P: usize = 0xa80;
const PAR_N: usize = 0xac0;

// Engine SRAM offsets (relative to `SBC_SRAM_BASE`).
const SR_GX: usize = 0x2000;
const SR_GY: usize = 0x2040;
const SR_QX: usize = 0x2080;
const SR_QY: usize = 0x20c0;
const SR_P: usize = 0x2100;
const SR_A: usize = 0x2140;
const SR_N: usize = 0x2180;
const SR_R: usize = 0x21c0;
const SR_S: usize = 0x2200;
const SR_M: usize = 0x2240;
const SR_INSTR: usize = 0x23c0;

const SCALAR_LEN: usize = 48; // P-384 / SHA-384: 48-byte (12-word) operands

const STS_DONE: u32 = 1 << 20; // secure014 bit-20: operation complete
const STS_PASS: u32 = 1 << 21; // secure014 bit-21: verification passed

/// Safe wrapper around the AST10x0 ECDSA (SBC) engine register block.
///
/// `Copy`/`Clone` by design: the façade confines `unsafe` and restricts
/// threading (`!Send`/`!Sync`); it does not enforce exclusivity — that is
/// delegated to the caller and the device/op layer above.
#[derive(Copy, Clone)]
pub struct SbcRegisters {
    ptr: *mut device::secure::RegisterBlock,
    _not_send: PhantomData<*mut ()>,
}

impl SbcRegisters {
    /// Create a register accessor from a raw SBC/ECDSA register block pointer.
    ///
    /// # Safety
    /// Caller must ensure `base` points to a valid SBC (`secure`) register
    /// block and that access to the ECDSA engine is serialized.
    pub const unsafe fn new(base: *const device::secure::RegisterBlock) -> Self {
        Self {
            ptr: base as *mut device::secure::RegisterBlock,
            _not_send: PhantomData,
        }
    }

    /// Create a register accessor for the global ECDSA (SBC) instance.
    ///
    /// # Safety
    /// Caller must ensure access to the singleton SBC is coordinated.
    pub const unsafe fn new_global() -> Self {
        // SAFETY: Caller upholds the singleton access contract.
        unsafe { Self::new(device::Secure::ptr()) }
    }

    #[inline]
    pub(crate) fn regs(&self) -> &device::secure::RegisterBlock {
        // SAFETY: Constructor guarantees a valid SBC register block pointer.
        unsafe { &*self.ptr }
    }

    // --- raw confined accessors (PAC-unmodelled regions only) ---

    #[inline]
    unsafe fn sec_rd(&self, off: usize) -> u32 {
        // SAFETY: `off` is within the SBC MMIO region of a valid base
        // (constructor contract); 32-bit aligned by construction.
        unsafe { read_volatile((self.ptr as *const u8).add(off) as *const u32) }
    }

    #[inline]
    unsafe fn sec_wr(&self, off: usize, val: u32) {
        // SAFETY: as `sec_rd`.
        unsafe { write_volatile((self.ptr as *mut u8).add(off) as *mut u32, val) }
    }

    #[inline]
    unsafe fn sram_wr(&self, off: usize, val: u32) {
        // SAFETY: SBC_SRAM_BASE is the engine scratch region (P5-OPEN-A);
        // `off` is a 32-bit-aligned operand slot < 0x2400.
        unsafe { write_volatile((SBC_SRAM_BASE + off) as *mut u32, val) }
    }

    /// Copy one 48-byte domain parameter from the engine MMIO window to SRAM.
    #[inline]
    unsafe fn copy_param(&self, par_off: usize, sram_off: usize) {
        let mut i = 0;
        while i < SCALAR_LEN {
            // SAFETY: confined raw access, see `sec_rd`/`sram_wr`.
            unsafe { self.sram_wr(sram_off + i, self.sec_rd(par_off + i)) };
            i += 4;
        }
    }

    /// Write one 48-byte operand into SRAM as 12 native-endian words.
    ///
    /// Mirrors the authority's `*(uint32_t *)(buf + i)` reinterpret with **no
    /// byte-swap** (goal.md §1.1): operand byte-convention is the caller's.
    #[inline]
    unsafe fn load_operand(&self, buf: &[u8; SCALAR_LEN], sram_off: usize) {
        let mut i = 0;
        while i < SCALAR_LEN {
            let w = u32::from_ne_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
            // SAFETY: confined raw access, see `sram_wr`.
            unsafe { self.sram_wr(sram_off + i, w) };
            i += 4;
        }
    }

    // --- curated, intent-named, safe operations (goal.md §1.2) ---

    /// Run the full pre-trigger + trigger sequence for one P-384 verify.
    ///
    /// Reproduces `zephyr-reference/ecdsa_aspeed.c:54-112` in exact order.
    /// `delay_ns` is the injected cooperative strategy used for the two
    /// authority settle windows (deltas D2). Completion polling and result
    /// decode are the caller's (`verify_is_done`/`verify_passed`).
    ///
    /// HZ1: the trigger is the **literal value `2`** written raw to `0xbc`,
    /// never the PAC `sec_boot_ecceng_trigger_reg` bit-0 setter (that is the
    /// rejected buggy `aspeed-rust` form).
    pub(crate) fn start_verify(
        &self,
        qx: &[u8; SCALAR_LEN],
        qy: &[u8; SCALAR_LEN],
        r: &[u8; SCALAR_LEN],
        s: &[u8; SCALAR_LEN],
        m: &[u8; SCALAR_LEN],
        delay_ns: &mut dyn FnMut(u32),
    ) {
        // SAFETY: all raw/PAC access is confined to this block; pointer
        // validity is the constructor contract; offsets/values are the
        // authority's (goal.md §1.2, cited per step).
        unsafe {
            self.sec_wr(OFF_MODE, 0x0100_f00b); // :54 step 1
            // :57-59 step 2 — reset ECC engine, 1 ms settle (D2)
            self.regs().secure0b4().write(|w| w.bits(0));
            self.regs().secure0b4().write(|w| w.bits(1));
            delay_ns(RESET_SETTLE_NS);
            // :63-80 step 3 — P-384 domain params MMIO → SRAM, a = 0
            self.copy_param(PAR_GX, SR_GX);
            self.copy_param(PAR_GY, SR_GY);
            self.copy_param(PAR_P, SR_P);
            self.copy_param(PAR_N, SR_N);
            let mut i = 0;
            while i < SCALAR_LEN {
                self.sram_wr(SR_A + i, 0);
                i += 4;
            }
            self.sec_wr(OFF_MODE, 0x0300_f00b); // :82 step 4
            // :84-102 step 5 — public key, signature, message digest
            self.load_operand(qx, SR_QX);
            self.load_operand(qy, SR_QY);
            self.load_operand(r, SR_R);
            self.load_operand(s, SR_S);
            self.load_operand(m, SR_M);
            self.sec_wr(OFF_MODE, 0x0); // :104 step 6
            self.sram_wr(SR_INSTR, 1); // :107 step 7 — instruction word
            // :110-112 step 8 — trigger (HZ1: raw 2), 5 ms hold, de-assert
            self.regs().secure0bc().write(|w| w.bits(2));
            delay_ns(TRIGGER_HOLD_NS);
            self.regs().secure0bc().write(|w| w.bits(0));
        }
    }

    /// Engine completed the in-flight operation (`secure014` bit-20).
    #[inline]
    pub(crate) fn verify_is_done(&self) -> bool {
        self.regs().secure014().read().bits() & STS_DONE != 0
    }

    /// Verification passed (`secure014` bit-21). Only meaningful once
    /// [`Self::verify_is_done`] is true (goal.md §1.2 step 10).
    #[inline]
    pub(crate) fn verify_passed(&self) -> bool {
        self.regs().secure014().read().bits() & STS_PASS != 0
    }

    /// Fault-path-only defensive teardown: de-assert the trigger register.
    ///
    /// O8 (goal.md §2.2): the authority performs **no** status-clear on the
    /// reachable valid/invalid paths. This is invoked **only** on the D3
    /// timeout path; it reuses the authority's own trigger-de-assert write
    /// (`ecdsa_aspeed.c:112`) and must never run on a reachable verdict path.
    #[inline]
    pub(crate) fn clear_status(&self) {
        self.regs().secure0bc().write(|w| unsafe { w.bits(0) });
    }
}
