// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Type-erased SBC operation adapter: owns the bounded completion-poll loop,
//! shared by ECDSA verify (`verify_raw`) and RSA modexp (`modexp`).
//!
//! Operation adapter of the *Cooperative-Yield Bounded-Poll Device* pattern.
//! Built from an [`SbcDevice`](super::device::SbcDevice) by a borrow split
//! — `regs`/`poll_budget` are `Copy`d out, `yield_fn` is reborrowed and
//! **type-erased** to `&mut dyn FnMut(u32)` so this adapter need not be
//! generic over the strategy.

use super::constants::POLL_YIELD_NS;
use super::error::SbcError;
use super::registers::{RSA_MAX_INPUT, RSA_MAX_LEN, SbcRegisters};

pub struct SbcOp<'a> {
    pub(crate) regs: SbcRegisters,
    pub(crate) poll_budget: u32,
    /// Cooperative yield hook, borrowed from the originating
    /// [`SbcDevice`](super::device::SbcDevice) and invoked once between
    /// every completion poll. Type-erased so the adapter (and the future
    /// verify/protocol impls) need not be generic over the strategy.
    pub(crate) yield_fn: &'a mut dyn FnMut(u32),
}

impl<'a> SbcOp<'a> {
    /// Construct an operation adapter from an existing register handle, poll
    /// budget, and cooperative yield hook.
    pub(crate) fn new(
        regs: SbcRegisters,
        poll_budget: u32,
        yield_fn: &'a mut dyn FnMut(u32),
    ) -> Self {
        Self {
            regs,
            poll_budget,
            yield_fn,
        }
    }

    /// Construct an operation adapter from an [`SbcDevice`].
    ///
    /// `regs`/`poll_budget` are `Copy`; the only retained borrow is the
    /// disjoint `yield_fn` field, reborrowed (never moved/copied) for `'a`.
    ///
    /// # Safety
    /// Caller must ensure no concurrent or reentrant ECDSA access for the
    /// lifetime of the returned [`SbcOp`].
    pub unsafe fn from_device<Y: FnMut(u32)>(
        device: &'a mut super::device::SbcDevice<Y>,
    ) -> Self {
        let regs = device.regs;
        let poll_budget = device.poll_budget;
        let yield_fn: &'a mut dyn FnMut(u32) = &mut device.yield_fn;
        Self::new(regs, poll_budget, yield_fn)
    }

    /// Verify one P-384 signature. The internal operation entry — callable
    /// **without** the HAL trait.
    ///
    /// Drives the engine via the façade, bounded-poll `verify_is_done` with
    /// the injected strategy once per non-completing poll, then decode
    /// `verify_passed`.
    ///
    /// - `Ok(())` — engine completed, signature valid (bit-20 ∧ bit-21).
    /// - `Err(VerificationFailed)` — completed, invalid (bit-20 ∧ ¬bit-21).
    /// - `Err(Timeout)` — poll budget exhausted; façade fault-cleanup then typed err.
    pub fn verify_raw(
        &mut self,
        qx: &[u8; 48],
        qy: &[u8; 48],
        r: &[u8; 48],
        s: &[u8; 48],
        m: &[u8; 48],
    ) -> Result<(), SbcError> {
        // Pre-trigger + trigger; settle delays use the reborrowed strategy.
        self.regs
            .start_verify(qx, qy, r, s, m, &mut *self.yield_fn);

        for _ in 0..self.poll_budget {
            if self.regs.verify_is_done() {
                return if self.regs.verify_passed() {
                    Ok(())
                } else {
                    Err(SbcError::VerificationFailed)
                };
            }
            (self.yield_fn)(POLL_YIELD_NS); // injected strategy, advisory ns
        }
        self.regs.clear_status(); // O8: fault-path only
        Err(SbcError::Timeout) // D3: typed, bounded failure
    }

    /// RSA modular exponentiation `out = data ^ exp mod modulus` — the
    /// engine's raw primitive (verify/enc = public exponent `e`; sign/dec =
    /// private `d`; the caller picks which). Internal, trait-free entry.
    ///
    /// `exp`/`modulus` are big-endian buffers ≥ `(bits+7)/8`; `data` is the
    /// big-endian input (≤ 512 B). On success `out` (≥ `RSA_MAX_LEN`) gets
    /// the big-endian result, leading zeros stripped; returns its length.
    ///
    /// - `Ok(n)` — completed; `out[..n]` is the result.
    /// - `Err(InvalidInput)` — sizes out of range.
    /// - `Err(Timeout)` — poll budget exhausted; scratch zeroed, then typed error.
    pub fn modexp(
        &mut self,
        exp: &[u8],
        modulus: &[u8],
        data: &[u8],
        e_bits: u32,
        m_bits: u32,
        out: &mut [u8],
    ) -> Result<usize, SbcError> {
        let e_len = (e_bits as usize).div_ceil(8);
        let m_len = (m_bits as usize).div_ceil(8);
        // Authority -EINVAL (`rsa_aspeed.c:54-57`) + Rust-side bound safety
        // the C code omits (no out-of-bounds SECSRAM/buffer access).
        if data.len() > RSA_MAX_INPUT
            || e_len > RSA_MAX_INPUT
            || m_len > RSA_MAX_INPUT
            || exp.len() < e_len
            || modulus.len() < m_len
            || out.len() < RSA_MAX_LEN
        {
            return Err(SbcError::InvalidInput);
        }

        self.regs
            .start_rsa(exp, modulus, data, e_len, m_len, e_bits, m_bits);

        for _ in 0..self.poll_budget {
            if self.regs.rsa_is_done() {
                let n = self.regs.read_rsa_result(out);
                self.regs.rsa_clear_scratch(); // authority :98 (reachable)
                return Ok(n);
            }
            (self.yield_fn)(POLL_YIELD_NS); // injected strategy, advisory ns
        }
        self.regs.rsa_clear_scratch(); // R1 fault teardown (= authority zero)
        Err(SbcError::Timeout) // R1: typed, bounded failure
    }
}
