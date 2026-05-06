// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI monitor public types.

/// Direction selector for address privilege programming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeDirection {
    Read,
    Write,
}

/// Whether a privilege region grants or denies access.
///
/// Mirrors Zephyr's `SPI_FILTER_PRIV_ENABLE` / `SPI_FILTER_PRIV_DISABLE`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegeOp {
    /// Grant access: the region is permitted for the given direction.
    Enable,
    /// Deny access: the region is blocked for the given direction.
    Disable,
}

/// Monitor filter passthrough mode.
///
/// When `Enabled`, SPI traffic bypasses the filter (used during mux ownership
/// transitions). When `Disabled`, the programmed policy is enforced.
/// Mirrors Zephyr's `spim_passthrough_config`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassthroughMode {
    Enabled,
    Disabled,
}

/// External mux selection for SPI path routing.
///
/// `Sel0` and `Sel1` are hardware-level values. Platform code is responsible
/// for mapping these to ROT vs BMC/PCH roles (with optional polarity inversion).
/// Mirrors Zephyr's `spim_ext_mux_sel`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtMuxSel {
    Sel0,
    Sel1,
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
    /// Whether this region grants (`Enable`) or denies (`Disable`) access.
    pub op: PrivilegeOp,
}

impl RegionPolicy {
    /// Construct a region that grants access in the given direction.
    #[must_use]
    pub const fn allow(start: u32, length: u32, direction: PrivilegeDirection) -> Self {
        Self { start, length, direction, op: PrivilegeOp::Enable }
    }

    /// Construct a region that denies access in the given direction.
    #[must_use]
    pub const fn deny(start: u32, length: u32, direction: PrivilegeDirection) -> Self {
        Self { start, length, direction, op: PrivilegeOp::Disable }
    }
}

/// Decoded entry from the SPIPF violation log RAM.
///
/// The hardware log word encoding (bits[19:18]):
/// - `0b00` → blocked command opcode in bits[7:0]
/// - `0b01` → blocked write address: bits[17:0] << 14
/// - `0b10` → blocked read address:  bits[17:0] << 14
/// - other  → invalid/reserved
///
/// ISR callback installation, workqueue deferral, and log-pointer reset are
/// caller / platform responsibilities and are not part of this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViolationLogEntry {
    BlockedCommand(u8),
    BlockedWriteAddr(u32),
    BlockedReadAddr(u32),
    Invalid(u32),
}

impl ViolationLogEntry {
    /// Decode a raw 32-bit hardware log word.
    #[must_use]
    pub fn parse(word: u32) -> Self {
        match (word >> 18) & 0x3 {
            0x0 => Self::BlockedCommand((word & 0xFF) as u8),
            0x1 => Self::BlockedWriteAddr((word & 0x3_FFFF) << 14),
            0x2 => Self::BlockedReadAddr((word & 0x3_FFFF) << 14),
            _ => Self::Invalid(word),
        }
    }
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
