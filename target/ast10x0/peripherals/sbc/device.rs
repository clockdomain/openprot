// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! ECDSA device binding with cooperative yield.
//!
//! Construction gate of the *Cooperative-Yield Bounded-Poll Device* pattern
//! (`design-patterns` catalog entry `cooperative-yield-bounded-poll-device`):
//! the wait policy is injected here as a `Y: FnMut(u32)` strategy, alongside
//! the [`SbcRegisters`] façade handle and a tunable poll budget.

use super::constants::DEFAULT_POLL_BUDGET;
use super::registers::SbcRegisters;

/// ECDSA engine bound to a cooperative-yield wait strategy.
///
/// `Y` is the caller-injected wait policy invoked between completion polls
/// (busy-spin, RTOS sleep, async-executor yield, instrumented backoff); the
/// generic lives only at this construction gate. The bounded poll loop itself
/// is owned by the type-erased [`super::op::SbcOp`] adapter.
pub struct SbcDevice<Y: FnMut(u32)> {
    pub(crate) regs: SbcRegisters,
    /// Cooperative yield hook invoked between completion polls.
    /// Argument is a suggested wait window in nanoseconds.
    pub(crate) yield_fn: Y,
    pub(crate) poll_budget: u32,
}

impl<Y: FnMut(u32)> SbcDevice<Y> {
    /// Create a device bound to a raw SBC/ECDSA register block with a
    /// caller-provided cooperative yield strategy.
    ///
    /// # Safety
    /// Caller must uphold the same safety contract as [`SbcRegisters::new`].
    /// This type is non-reentrant: only one `SbcDevice` may be active at a
    /// time.
    pub unsafe fn new_with_yield(
        base: *const ast1060_pac::secure::RegisterBlock,
        yield_fn: Y,
    ) -> Self {
        Self {
            // SAFETY: Caller upholds register-pointer validity/ownership.
            regs: unsafe { SbcRegisters::new(base) },
            yield_fn,
            poll_budget: DEFAULT_POLL_BUDGET,
        }
    }

    /// Create a device bound to a raw SBC/ECDSA register block.
    ///
    /// # Safety
    /// Caller must uphold the same safety contract as [`SbcRegisters::new`].
    /// This type is non-reentrant: only one `SbcDevice` may be active at a
    /// time.
    pub unsafe fn new(
        base: *const ast1060_pac::secure::RegisterBlock,
        yield_fn: Y,
    ) -> Self {
        // SAFETY: Same contract as this wrapper.
        unsafe { Self::new_with_yield(base, yield_fn) }
    }

    /// Create a device bound to the singleton SBC/ECDSA instance with a
    /// caller-provided cooperative yield strategy.
    ///
    /// # Safety
    /// Caller must coordinate singleton access globally.
    /// This type is non-reentrant: only one `SbcDevice` may be active at a
    /// time.
    pub unsafe fn new_global_with_yield(yield_fn: Y) -> Self {
        Self {
            // SAFETY: Caller coordinates singleton access.
            regs: unsafe { SbcRegisters::new_global() },
            yield_fn,
            poll_budget: DEFAULT_POLL_BUDGET,
        }
    }

    /// Create a device bound to the singleton SBC/ECDSA instance.
    ///
    /// # Safety
    /// Caller must coordinate singleton access globally.
    /// This type is non-reentrant: only one `SbcDevice` may be active at a
    /// time.
    pub unsafe fn new_global(yield_fn: Y) -> Self {
        // SAFETY: Same contract as this wrapper.
        unsafe { Self::new_global_with_yield(yield_fn) }
    }

    /// Override polling timeout budget for operation completion.
    #[must_use]
    pub const fn with_timeout_polls(mut self, timeout_polls: u32) -> Self {
        self.poll_budget = timeout_polls;
        self
    }
}
