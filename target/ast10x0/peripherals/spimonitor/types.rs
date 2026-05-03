// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI monitor public types.

/// Direction selector for address privilege programming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeDirection {
    Read,
    Write,
}

/// Lock state observed from hardware policy tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockState {
    Unlocked,
    Locked,
}

/// High-level monitor lifecycle stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorState {
    Uninitialized,
    Configured,
    Locked,
}

/// Region policy entry used by higher-level policy/controller code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionPolicy {
    pub start: u32,
    pub length: u32,
    pub direction: PrivilegeDirection,
}

/// SPI monitor module error type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiMonitorError {
    InvalidRegion,
    InvalidSlot,
    Locked,
    InvalidTransition,
}

/// Result alias for SPI monitor APIs.
pub type Result<T> = core::result::Result<T, SpiMonitorError>;
