// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]

use ast10x0_peripherals::scu::{PinctrlPin, ScuRegisters};

/// Board-level descriptor for AST10x0 SMC flash topology.
///
/// Not `Eq`/`PartialEq` because the embedded `MonitorPolicy` is not (yet);
/// callers compare individual fields if they need equality.
#[derive(Clone, Debug)]
pub struct Ast10x0BoardDescriptor {
    /// Pin control groups to apply before SPIM wiring during board init.
    /// Applied in order via `ScuRegisters::apply_pinctrl_group()` before
    /// SPIM routing is programmed and locked.
    pub pinctrl_groups: &'static [&'static [PinctrlPin]],
}

impl Ast10x0BoardDescriptor {
    /// Initialize board: apply pinctrl groups.
    ///
    /// # Safety
    /// Caller must hold exclusive access to the SCU register block.
    pub unsafe fn init_board(&self, scu: &ScuRegisters) {
        for group in self.pinctrl_groups {
            scu.apply_pinctrl_group(group);
        }
    }
}