// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 SPI monitor (SPIPF) module.

pub mod registers;
pub mod types;
pub mod policy;
pub mod profile;
pub mod controller;

pub use registers::{
    SpiMonitorController, SpiMonitorRegisters, SPIPF1_BASE, SPIPF2_BASE, SPIPF3_BASE, SPIPF4_BASE,
    SPIPF_REG_SIZE,
};
pub use types::{
    ExtMuxSel, LockState, MonitorState, PassthroughMode, PrivilegeDirection, PrivilegeOp,
    RegionPolicy, Result as SpiMonitorResult, SpiMonitorError, ViolationLogEntry,
};
pub use policy::{MonitorPolicy, MAX_CMD_SLOTS, MAX_REGION_SLOTS};
pub use controller::{
    Configured, ConfiguredSpiMonitor, Locked, LockedSpiMonitor, SpiMonitor, Uninitialized,
    UninitSpiMonitor,
};
