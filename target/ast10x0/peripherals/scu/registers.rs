// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 SCU low-level register access.

use ast1060_pac as device;
use core::cell::UnsafeCell;
use core::marker::PhantomData;

/// Safe wrapper around the AST10x0 SCU register block.
pub struct ScuRegisters {
    base: *const device::scu::RegisterBlock,
    _not_sync: PhantomData<UnsafeCell<()>>, // Prevent Sync, allow Send.
}

impl ScuRegisters {
    /// Create a register accessor from a raw SCU register block pointer.
    ///
    /// # Safety
    /// Caller must ensure:
    /// - `base` points to a valid SCU register block.
    /// - access to the SCU instance is serialized appropriately.
    pub const unsafe fn new(base: *const device::scu::RegisterBlock) -> Self {
        Self {
            base,
            _not_sync: PhantomData,
        }
    }

    /// Create a register accessor for the global SCU instance.
    ///
    /// # Safety
    /// Caller must ensure access to the singleton SCU is coordinated.
    pub const unsafe fn new_global() -> Self {
        // SAFETY: Caller upholds the singleton access contract.
        unsafe { Self::new(device::Scu::ptr()) }
    }

    #[inline]
    pub(crate) fn regs(&self) -> &device::scu::RegisterBlock {
        // SAFETY: Constructor guarantees a valid SCU register block pointer.
        unsafe { &*self.base }
    }
}