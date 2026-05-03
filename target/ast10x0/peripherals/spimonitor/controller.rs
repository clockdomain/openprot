// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI monitor controller facade.

use core::marker::PhantomData;

use crate::spimonitor::policy::MonitorPolicy;
use crate::spimonitor::registers::{SpiMonitorController, SpiMonitorRegisters};
use crate::spimonitor::types::{LockState, MonitorState, Result, SpiMonitorError};

/// Typestate: monitor is created but policy is not yet applied.
pub struct Uninitialized;
/// Typestate: policy tables are programmed and can still be changed.
pub struct Configured;
/// Typestate: policy is locked and runtime-mutating APIs are unavailable.
pub struct Locked;

/// Generic SPI monitor instance with typestate-enforced lifecycle.
pub struct SpiMonitor<Mode> {
    regs: SpiMonitorRegisters,
    controller: SpiMonitorController,
    _mode: PhantomData<fn() -> Mode>,
}

/// Ergonomic alias for an uninitialized SPI monitor handle.
pub type UninitSpiMonitor = SpiMonitor<Uninitialized>;
/// Ergonomic alias for a configured-but-unlocked SPI monitor handle.
pub type ConfiguredSpiMonitor = SpiMonitor<Configured>;
/// Ergonomic alias for a locked SPI monitor handle.
pub type LockedSpiMonitor = SpiMonitor<Locked>;

impl SpiMonitor<Uninitialized> {
    /// Construct a new controller facade for a specific monitor instance.
    ///
    /// # Safety
    /// Caller must guarantee exclusive ownership of the target SPIPF block.
    pub const unsafe fn new(controller: SpiMonitorController) -> Self {
        Self {
            regs: unsafe { SpiMonitorRegisters::new_for_controller(controller) },
            controller,
            _mode: PhantomData,
        }
    }

    /// Program command-table policy and transition to `Configured`.
    pub fn apply_policy(self, policy: &MonitorPolicy) -> Result<SpiMonitor<Configured>> {
        if policy.allow_command_count > policy.allow_commands.len() {
            return Err(SpiMonitorError::InvalidSlot);
        }

        for i in 0..policy.allow_command_count {
            let cmd = policy.allow_commands[i] as u32;
            self.regs.write_allow_cmd_slot(i, cmd);
        }

        Ok(SpiMonitor {
            regs: self.regs,
            controller: self.controller,
            _mode: PhantomData,
        })
    }

    #[must_use]
    pub const fn state(&self) -> MonitorState {
        MonitorState::Uninitialized
    }
}

impl SpiMonitor<Configured> {
    /// Lock monitor policy registers and transition to `Locked`.
    pub fn lock(self) -> Result<SpiMonitor<Locked>> {
        // Placeholder lock bit for scaffold; replace with precise SPIPF lock
        // field wiring as register semantics are finalized.
        self.regs.modify_ctrl(|bits| *bits |= 1);

        Ok(SpiMonitor {
            regs: self.regs,
            controller: self.controller,
            _mode: PhantomData,
        })
    }

    #[must_use]
    pub const fn state(&self) -> MonitorState {
        MonitorState::Configured
    }
}

impl SpiMonitor<Locked> {
    #[must_use]
    pub const fn lock_state(&self) -> LockState {
        LockState::Locked
    }

    #[must_use]
    pub const fn state(&self) -> MonitorState {
        MonitorState::Locked
    }
}

impl<Mode> SpiMonitor<Mode> {
    #[must_use]
    pub fn regs(&self) -> &SpiMonitorRegisters {
        &self.regs
    }

    #[must_use]
    pub const fn controller(&self) -> SpiMonitorController {
        self.controller
    }
}
