// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 System Control Unit (SCU) module.

pub mod registers;
pub mod types;
pub mod reset;
pub mod clock;
pub mod routing;
pub mod status;

pub use registers::ScuRegisters;
pub use types::{
    ClockRegisterHalf, Result as ScuResult, ScuError, ScuExtMuxSelect, ScuRegisterHalf,
    SpiMonitorInstance, SpiMonitorPassthrough, SpiMonitorSource,
};