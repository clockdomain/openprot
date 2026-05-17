// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Type-erased ECDSA operation adapter: owns the bounded completion-poll loop.
//!
//! Operation adapter of the *Cooperative-Yield Bounded-Poll Device* pattern.
//! Built from an [`SbcDevice`](super::device::SbcDevice) by a borrow split
//! — `regs`/`poll_budget` are `Copy`d out, `yield_fn` is reborrowed and
//! **type-erased** to `&mut dyn FnMut(u32)` so this adapter (and any future
//! verify/protocol impls on it) need not be generic over the strategy.

use super::constants::POLL_YIELD_NS;
use super::error::SbcError;
use super::registers::SbcRegisters;

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
    /// **without** the HAL trait (ADR-1: the trait skin is `hal_impl`, not
    /// this driver).
    ///
    /// Reproduces the authority sequence (goal.md §1.2): drive the engine via
    /// the façade, bounded-poll `verify_is_done` with the injected strategy
    /// once per non-completing poll, then decode `verify_passed`. The two
    /// authority settle windows (D2) reuse the same type-erased strategy,
    /// reborrowed for the pre-trigger phase.
    ///
    /// - `Ok(())` — engine completed, signature valid (bit-20 ∧ bit-21).
    /// - `Err(VerificationFailed)` — completed, invalid (bit-20 ∧ ¬bit-21).
    /// - `Err(Timeout)` — poll budget exhausted (D3 intentional delta:
    ///   the authority would hang here); façade fault-cleanup then typed err.
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
}
