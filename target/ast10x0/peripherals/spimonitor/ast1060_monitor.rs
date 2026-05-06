// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Phase C: PAC-based concrete implementation of the Monitor trait.
//!
//! This module provides `Ast1060Monitor`, a concrete implementation of the `Monitor`
//! trait using AST10x0 PAC register bindings (ast10x0-pac). It maps the Monitor
//! trait's abstract methods to low-level register reads/writes for SPIPF1-4
//! hardware blocks.
//!//! # Register Map (from aspeed-rust reverse engineering)
//!
//! - **SPIPF000**: Control register (SPIPF base + 0x00)
//!   - Bits [1]: Single-bit passthrough enable
//!   - Bits [2]: Multi-bit passthrough enable
//!   - Bits [7]: Soft reset (derived from aspeed-rust patterns)
//! - **SPIPF004**: Secondary control register (SPIPF base + 0x04)
//! - **SPIPF07C**: Lock/status register (SPIPF base + 0x7C)
//!   - Bits [0]: wr_dis_of_spipfwa (write disable for address privilege tables = policy lock)
//! - **SPIPFWT[n]**: Allow command table entry (SPIPF base + 0x10 + n*4)
//! - **SPIPFWA[n]**: Address filter table entry (SPIPF base + 0x20 + n*4)
//!
//! # External Mux Control
//!
//! External mux selection (ROT vs Host) is controlled via **SCU0F0 register**,
//! not SPIPF. From aspeed-rust:
//! - `ext_mux_select_sig_of_spipf1()` for SPIM0 (through `scu.scu0f0()`)
//! - Similar fields for SPIM1-3
//!
//! **BLOCKING TODO**: Implement SCU register access when ast10x0-pac includes SCU bindings.
//! Current issue: ast10x0-pac does not export SCU register types. Once SCU support is added,
//! `set_mux()` and `read_mux()` can read/write the ext_mux_select_sig_of_spipfN() field.
//!//! # Safety
//!
//! `PacMonitor` requires exclusive ownership of all SPIPF instances. Boot code
//! must ensure only one `PacMonitor` instance exists and maintains this invariant
//! throughout execution.

use super::traits::Monitor;
use super::types::{
    BootError, BootResult, MuxSelect, MonitorInstance, MonitorStatus,
    PrivilegeDirection, PrivilegeOp,
};
use super::registers::{SpiMonitorController, SpiMonitorRegisters};

/// Concrete Monitor implementation using PAC register access.
///
/// Holds references to all four SPIPF register blocks. Boot code creates
/// a single `Ast1060Monitor` instance and passes `&mut self` to boot functions.
///
/// # Example
///
/// ```ignore
/// let mut monitor = Ast1060Monitor::new();
/// let config = BootConfig::default();
/// let status = monitor.read_status(MonitorInstance::Spim0)?;
/// println!("Mux: {:?}", status.mux);
/// ```
pub struct Ast1060Monitor {
    spim0: SpiMonitorRegisters,
    spim1: SpiMonitorRegisters,
    spim2: SpiMonitorRegisters,
    spim3: SpiMonitorRegisters,
}

impl Ast1060Monitor {
    /// Create a new PacMonitor with exclusive access to all SPIPF blocks.
    ///
    /// # Safety
    ///
    /// Caller must ensure:
    /// - This is called only once during boot.
    /// - No other code holds references to the SPIPF hardware.
    /// - The returned instance is the sole owner until dropped.
    ///
    /// # Note
    ///
    /// External mux control (SCU0F0) is not yet accessible via this implementation
    /// pending PAC updates. Mux operations may be incomplete.
    #[allow(unsafe_op_in_unsafe_fn)]
    pub unsafe fn new() -> Self {
        Self {
            spim0: unsafe { SpiMonitorRegisters::new_for_controller(SpiMonitorController::Spim0) },
            spim1: unsafe { SpiMonitorRegisters::new_for_controller(SpiMonitorController::Spim1) },
            spim2: unsafe { SpiMonitorRegisters::new_for_controller(SpiMonitorController::Spim2) },
            spim3: unsafe { SpiMonitorRegisters::new_for_controller(SpiMonitorController::Spim3) },
        }
    }

    /// Get the register accessor for the specified monitor instance.
    #[inline]
    fn regs(&self, instance: MonitorInstance) -> &SpiMonitorRegisters {
        match instance {
            MonitorInstance::Spim0 => &self.spim0,
            MonitorInstance::Spim1 => &self.spim1,
            MonitorInstance::Spim2 => &self.spim2,
            MonitorInstance::Spim3 => &self.spim3,
        }
    }

    /// Get mutable reference to register accessor (for write operations).
    #[inline]
    fn regs_mut(&mut self, instance: MonitorInstance) -> &mut SpiMonitorRegisters {
        match instance {
            MonitorInstance::Spim0 => &mut self.spim0,
            MonitorInstance::Spim1 => &mut self.spim1,
            MonitorInstance::Spim2 => &mut self.spim2,
            MonitorInstance::Spim3 => &mut self.spim3,
        }
    }

    /// Extract mux select from hardware state.
    /// BLOCKING: Requires SCU0F0 register access (not yet in ast10x0-pac).
    /// This method reads the SPIPF000 register (which does not contain mux state),
    /// so it returns a placeholder value. Real implementation must read SCU0F0.ext_mux_select_sig_of_spipfN().
    fn extract_mux_from_ctrl(_ctrl: u32) -> MuxSelect {
        // Placeholder: SCU register access needed - for now always return HostControl
        MuxSelect::HostControl
    }

    /// Encode mux select for SCU0F0 register.
    fn encode_mux_to_ctrl(mux: MuxSelect) -> u32 {
        match mux {
            MuxSelect::RotControl => 0,
            MuxSelect::HostControl => 1,
        }
    }

    /// Extract enforcement active flag from CTRL register.
    /// Enforcement is active when passthrough bits are NOT set.
    /// - SPIPF000[1] = enbl_single_bit_passthrough
    /// - SPIPF000[2] = enbl_multiple_bit_passthrough
    /// When both bits are 0, enforcement is active and SPI commands are filtered.
    fn is_enforcement_active(ctrl: u32) -> bool {
        let pass_bits = (ctrl >> 1) & 0x3;
        pass_bits == 0  // Enforcement active when passthrough disabled
    }

    /// Extract policy lock flag from lock/status register.
    /// Policy is locked when write-disable bits are set in SPIPF07C.
    /// SPIPF07C bit 0: wr_dis_of_spipfwa (write disable for address privilege tables)
    fn is_policy_locked(lock_status: u32) -> bool {
        // Write-disable bit in SPIPF07C
        // When this bit is set, policy tables cannot be modified
        (lock_status >> 0) & 0x1 != 0
    }
}

impl Monitor for Ast1060Monitor {
    fn set_mux(&mut self, _instance: MonitorInstance, _mux: MuxSelect) -> BootResult<()> {
        // External mux selection (ROT vs Host) is controlled via SCU0F0 register, not SPIPF.
        // From aspeed-rust: scu.scu0f0().modify(|_, w| w.ext_mux_select_sig_of_spipfN().bit(...))
        // BLOCKING: Requires SCU register support in AST1060_PAC. Currently returns HardwareError.
        // Once ast10x0-pac exports SCU0F0 with ext_mux_select_sig_of_spipf1/2/3/4 fields,
        // implement as: self.scu.scu0f0().modify(|_, w| w.ext_mux_select_sig_of_spipfN().bit(mux_bit))
        Err(BootError::HardwareError)
    }

    fn read_mux(&self, _instance: MonitorInstance) -> BootResult<MuxSelect> {
        // BLOCKING: Requires SCU0F0 register access via ast10x0-pac.
        // Implementation pattern: read scu.scu0f0().read().ext_mux_select_sig_of_spipfN().bit()
        // Returns HardwareError until SCU bindings are available.
        Err(BootError::HardwareError)
    }

    fn soft_reset(&mut self, instance: MonitorInstance) -> BootResult<()> {
        let regs = self.regs_mut(instance);
        // Soft reset clears status/logs but preserves policy.
        // NON-BLOCKING TODO 1: Verify soft reset bit position from AST10x0 datasheet.
        // Current: uses bit 7 in SPIPF000 as placeholder. May need adjustment.
        // NON-BLOCKING TODO 2: Implement polling/timeout after write.
        // Pattern: poll SPIPF000 until reset bit clears or timeout (e.g., 100 microseconds).
        // In aspeed-rust, hardware self-clears the bit after reset completes.
        let mut ctrl = regs.read_ctrl();
        ctrl |= 0x80; // Soft reset bit (placeholder - verify with datasheet)
        regs.write_ctrl(ctrl);
        // TODO: Poll until bit 7 clears, with timeout check
        Ok(())
    }

    fn hardware_reset(&mut self, instance: MonitorInstance) -> BootResult<()> {
        let regs = self.regs_mut(instance);
        // Full hardware reset of all state (SPIPF and related SCU registers).
        // NON-BLOCKING TODO 1: Verify hardware reset bit and sequence from AST10x0 datasheet.
        // Current: uses bit 8 in SPIPF000 as placeholder.
        // NON-BLOCKING TODO 2: Implement polling/timeout after write.
        // Check if reset bit self-clears like soft_reset, or if a separate status poll is needed.
        let mut ctrl = regs.read_ctrl();
        ctrl |= 0x100; // Hardware reset bit (placeholder - verify with datasheet)
        regs.write_ctrl(ctrl);
        // TODO: Poll until bit 8 clears or wait for timeout, similar to soft_reset
        Ok(())
    }

    fn set_address_privilege(
        &mut self,
        instance: MonitorInstance,
        start_addr: u32,
        end_addr: u32,
        _direction: PrivilegeDirection,
        _op: PrivilegeOp,
    ) -> BootResult<()> {
        if end_addr < start_addr {
            return Err(BootError::InvalidAddress);
        }

        let regs = self.regs_mut(instance);

        // Address filtering in AST10x0 uses 16KB blocks (ACCESS_BLOCK_UNIT from aspeed-rust).
        // Each SPIPFWA register controls 32 blocks per register (32 * 16KB = 512KB per register).
        // NON-BLOCKING TODO: Implement dynamic slot allocation instead of fixed slot 0.
        // Phase C work: currently allocates all regions to slot 0 for simplicity.
        // Phase D work: track used slots, allocate new regions to free slots, or reject if full.
        let slot = 0;

        // Address filter encoding: SPIPFWA stores address range in HW format.
        // Placeholder uses start/end field assignment; actual format needs datasheet verification.
        let entry = ((start_addr & 0xFFF0_0000) << 0) | ((end_addr & 0x0F_FFFF) << 0);
        regs.write_addr_filter_slot(slot, entry);

        Ok(())
    }

    fn read_region_count(&self, instance: MonitorInstance) -> BootResult<u32> {
        let regs = self.regs(instance);
        // NON-BLOCKING TODO: Determine region count source from AST10x0 datasheet.
        // Options:
        // 1. CTRL2 register field [23:16] (current placeholder)
        // 2. Software-tracked count in parent struct
        // 3. Count derived from policy table scan
        // For now, read hypothetical CTRL2 bits [23:16].
        let ctrl2 = regs.read_ctrl2();
        let count = (ctrl2 >> 16) & 0xFF;
        Ok(count)
    }

    fn read_status(&self, instance: MonitorInstance) -> BootResult<MonitorStatus> {
        let regs = self.regs(instance);
        let ctrl = regs.read_ctrl();
        let lock_status = regs.read_lock_status();

        Ok(MonitorStatus {
            mux: Self::extract_mux_from_ctrl(ctrl),
            policy_locked: Self::is_policy_locked(lock_status),
            enforcement_active: Self::is_enforcement_active(ctrl),
            violation_count: 0, // NON-BLOCKING TODO: Implement violation log register reading
                                  // Future: read from SPIPF violation count register if available
        })
    }

    fn supports_policy_lock(&self) -> bool {
        // Check hardware capability from a known register or constant.
        // Placeholder: assume AST10x0 always supports policy lock.
        true
    }

    fn lock_policy(&mut self, instance: MonitorInstance) -> BootResult<()> {
        if !self.supports_policy_lock() {
            return Err(BootError::LockedOutFromMonitor);
        }

        let regs = self.regs_mut(instance);
        // Policy lock is controlled by SPIPF07C register.
        // From aspeed-rust: bit 0 is `wr_dis_of_spipfwa` (write disable for address tables).
        let mut lock_status = regs.read_lock_status();
        lock_status |= 0x1;  // Set wr_dis_of_spipfwa bit
        regs.write_lock_status(lock_status);
        Ok(())
    }

    fn verify_policy_locked(&self, instance: MonitorInstance) -> BootResult<()> {
        if !self.supports_policy_lock() {
            return Ok(());
        }

        let regs = self.regs(instance);
        let lock_status = regs.read_lock_status();
        if Self::is_policy_locked(lock_status) {
            Ok(())
        } else {
            Err(BootError::PolicyVerificationFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mux_encoding() {
        assert_eq!(Ast1060Monitor::encode_mux_to_ctrl(MuxSelect::RotControl), 0);
        assert_eq!(Ast1060Monitor::encode_mux_to_ctrl(MuxSelect::HostControl), 1);
    }

    #[test]
    fn test_mux_extraction() {
        assert_eq!(
            Ast1060Monitor::extract_mux_from_ctrl(0),
            MuxSelect::RotControl
        );
        assert_eq!(
            Ast1060Monitor::extract_mux_from_ctrl(1),
            MuxSelect::HostControl
        );
    }

    #[test]
    fn test_enforcement_flag() {
        assert!(!Ast1060Monitor::is_enforcement_active(0));
        assert!(Ast1060Monitor::is_enforcement_active(1 << 4));
    }

    #[test]
    fn test_lock_flag() {
        assert!(!Ast1060Monitor::is_policy_locked(0));
        assert!(Ast1060Monitor::is_policy_locked(1));
    }
}
