// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Declarative policy model for SPI monitor configuration.

use crate::spimonitor::types::RegionPolicy;

/// Policy payload applied to a monitor instance.
#[derive(Clone, Debug)]
pub struct MonitorPolicy {
    pub allow_commands: [u8; 32],
    pub allow_command_count: usize,
    pub regions: [Option<RegionPolicy>; 32],
}

impl MonitorPolicy {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            allow_commands: [0; 32],
            allow_command_count: 0,
            regions: [None; 32],
        }
    }
}
