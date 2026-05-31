// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 SBC public-key engine low-level register access
//! (Confined-`unsafe` MMIO façade).
//!
//! One hardware block (Secure Boot Controller, `ast1060_pac` `secure`
//! peripheral, base `0x7e6f_2000` + SECSRAM `SBC_SRAM_BASE`) hosting two
//! operations: **ECDSA verify** (`start_verify`/`verify_*`) and **RSA modexp**
//! (`start_rsa`/`rsa_*`). The RSA ops are a separate path: only
//! `secure0b0`/`0bc`/`014` + SECSRAM, never the ECDSA reset/mode/param machinery.
//!
//! `secure014`/`0b0`/`0b4`/`0bc` are PAC-named; the ECDSA mode register
//! `0x7c`, its curve-param window `0xa00–0xac0`, and the SECSRAM are not
//! PAC-modelled and use raw confined offset access here and nowhere else.
//! All `unsafe`, raw offsets, and PAC types stay confined below this façade.

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

// --- RSA modexp (operation #2 of the same SBC engine; rsa goal.md §1.2).
// HZ-R1: a SEPARATE path — touches only 0xb0/0xbc/0x14 + SECSRAM, never the
// ECDSA reset/mode/param machinery. All three MMIO regs are PAC-named, so
// RSA needs no raw `sec_wr`; only SECSRAM is off-PAC.
const SR_RSA_EXP: usize = 0x0; // exponent → SECSRAM, byte-reversed
const SR_RSA_MOD: usize = 0x400; // modulus  → +0x400, byte-reversed
const SR_RSA_DATA: usize = 0x800; // input    → +0x800, byte-reversed
const SR_RSA_RESULT: usize = 0x1400; // result ← +0x1400, byte-reversed out
const RSA_SCRATCH: usize = 0x1800; // scratch span zeroed before+after (R3)
pub(crate) const RSA_MAX_LEN: usize = 0x400; // result region / max bytes
pub(crate) const RSA_MAX_INPUT: usize = 512; // authority `rsa_aspeed.c:54-57`
const STS_RSA_DONE: u32 = 1 << 4; // secure014 bit-4: RSA op complete

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
    /// byte-swap**: operand byte-convention is the caller's.
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

    // --- curated, intent-named, safe operations ---

    /// Run the full pre-trigger + trigger sequence for one P-384 verify.
    ///
    /// Reproduces `zephyr-reference/ecdsa_aspeed.c:54-112` in exact order.
    /// `delay_ns` is the injected cooperative strategy used for settle windows.
    /// Completion polling and result decode are the caller's (`verify_is_done`/`verify_passed`).
    ///
    /// The trigger is the **literal value `2`** written raw to `0xbc`,
    /// never the PAC `sec_boot_ecceng_trigger_reg` bit-0 setter.
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
        // validity is the constructor contract; offsets/values match the authority.
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
    /// The authority performs **no** status-clear on the reachable valid/invalid
    /// paths. This is invoked **only** on the timeout path and must never run
    /// on a reachable verdict path.
    #[inline]
    pub(crate) fn clear_status(&self) {
        self.regs().secure0bc().write(|w| unsafe { w.bits(0) });
    }

    // --- RSA modexp façade ops (separate path) ---

    #[inline]
    unsafe fn sram_wr8(&self, off: usize, val: u8) {
        // SAFETY: SECSRAM region (P5-OPEN-A); `off < RSA_SCRATCH`.
        unsafe { write_volatile((SBC_SRAM_BASE + off) as *mut u8, val) }
    }

    #[inline]
    unsafe fn sram_rd8(&self, off: usize) -> u8 {
        // SAFETY: as `sram_wr8`.
        unsafe { read_volatile((SBC_SRAM_BASE + off) as *const u8) }
    }

    /// Zero `span` bytes of SECSRAM (`span % 4 == 0`).
    /// Order/granularity agnostic — observably identical to the authority's `memset(.,0,.)`.
    #[inline]
    unsafe fn sram_zero(&self, span: usize) {
        let mut i = 0;
        while i < span {
            // SAFETY: confined; `span` ≤ RSA_SCRATCH, word-aligned.
            unsafe { self.sram_wr(i, 0) };
            i += 4;
        }
    }

    /// Write `buf[0..n]` into SECSRAM at `off` **byte-reversed**
    /// (`sram[i] = buf[n-1-i]`) — matches the authority's reversal.
    #[inline]
    unsafe fn load_reversed(&self, buf: &[u8], n: usize, off: usize) {
        let mut i = 0;
        while i < n {
            // SAFETY: caller guarantees `buf.len() >= n` and
            // `off + n <= RSA_SCRATCH` (validated in `SbcOp::modexp`).
            unsafe { self.sram_wr8(off + i, buf[n - 1 - i]) };
            i += 1;
        }
    }

    /// Zero the RSA scratch span (`memset(sram,0,0x1800)`);
    /// also the fault-path teardown.
    #[inline]
    pub(crate) fn rsa_clear_scratch(&self) {
        // SAFETY: confined SECSRAM zeroing, fixed span.
        unsafe { self.sram_zero(RSA_SCRATCH) };
    }

    /// Run the RSA modexp pre-trigger + trigger sequence.
    /// `e_len`/`m_len` are byte lengths `(bits+7)/8`; `exp`/`modulus`/`data`
    /// must be ≥ those (caller-validated). No settle delays. Touches only SECSRAM,
    /// `secure0b0` (key-len), and `secure0bc` (trigger).
    pub(crate) fn start_rsa(
        &self,
        exp: &[u8],
        modulus: &[u8],
        data: &[u8],
        e_len: usize,
        m_len: usize,
        e_bits: u32,
        m_bits: u32,
    ) {
        // SAFETY: all SECSRAM access is confined; offsets/spans are the
        // authority's and bounds were validated by `SbcOp::modexp`.
        unsafe {
            self.sram_zero(RSA_SCRATCH); // :59
            self.load_reversed(exp, e_len, SR_RSA_EXP); // :61-63
            self.load_reversed(modulus, m_len, SR_RSA_MOD); // :65-68
            self.load_reversed(data, data.len(), SR_RSA_DATA); // :70-73
        }
        // :75 key-len word = e_bits<<16 | m_bits (PAC-named reg, raw bits)
        self.regs()
            .secure0b0()
            .write(|w| unsafe { w.bits((e_bits << 16) | m_bits) });
        // :76-77 trigger pulse 1→0 (no hold delay, unlike ECDSA)
        self.regs().secure0bc().write(|w| unsafe { w.bits(1) });
        self.regs().secure0bc().write(|w| unsafe { w.bits(0) });
    }

    /// RSA op complete (`secure014` bit-4).
    #[inline]
    pub(crate) fn rsa_is_done(&self) -> bool {
        self.regs().secure014().read().bits() & STS_RSA_DONE != 0
    }

    /// Read the result from SECSRAM `+0x1400`, **byte-reversed with leading
    /// zeros stripped**; returns the significant byte count written to `out` (big-endian).
    /// `out.len()` must be ≥ `RSA_MAX_LEN` (caller-validated).
    pub(crate) fn read_rsa_result(&self, out: &mut [u8]) -> usize {
        let mut i = 0usize;
        let mut leading = true;
        let mut nbytes = RSA_MAX_LEN;
        let mut j = RSA_MAX_LEN;
        while j > 0 {
            j -= 1;
            // SAFETY: confined SECSRAM read within the result region.
            let b = unsafe { self.sram_rd8(SR_RSA_RESULT + j) };
            if b == 0 && leading {
                nbytes -= 1;
            } else {
                leading = false;
                out[i] = b;
                i += 1;
            }
        }
        nbytes
    }
}
