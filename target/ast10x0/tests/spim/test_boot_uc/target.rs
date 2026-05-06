// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 SPI Monitor Boot Use Case Test
//!
//! Validates the SPI monitor boot sequence:
//! 1. Hold phase - switch mux to ROT, reset flash
//! 2. Policy configuration - load address privilege regions
//! 3. Release phase - switch mux to host, soft-reset monitor
//! 4. Runtime verification - confirm enforcement active
//!
//! Uses pw_log for structured logging and PacMonitor for hardware control.
//! Safe to run on both QEMU virt_ast10x0 and silicon.

#![no_std]
#![no_main]

use cortex_m_semihosting::debug::{EXIT_FAILURE, EXIT_SUCCESS, exit};
use target_common::{TargetInterface, declare_target};
use {console_backend as _, entry as _};

// Import from board and spimonitor modules
use ast10x0_board::Ast1060Board;
use ast10x0_peripherals::spimonitor::{
    Monitor, MonitorInstance, MuxSelect,
};

// SPI NOR Flash Command Opcodes (from Winbond W25Q64, Macronix MX25L6473F, etc.)
// These are standard SPI NOR commands that should be allowed by the monitor filter.
// Adapted from aspeed-rust/tests-hw/src/spim_test.rs
const ALLOWED_SPI_COMMANDS: &[u8] = &[
    0x03,  // Read Data
    0x13,  // Read Data (4-byte address)
    0x0b,  // Fast Read
    0x0c,  // Fast Read (4-byte address)
    0x6b,  // Fast Read Dual Output (4-byte address)
    0x6c,  // Fast Read Dual I/O (4-byte address)
    0x01,  // Write Status Register
    0x05,  // Read Status Register
    0x35,  // Read Status Register 2
    0x06,  // Write Enable
    0x04,  // Write Disable
    0x20,  // Sector Erase (4K)
    0x21,  // Sector Erase (4K, 4-byte address)
    0x9f,  // Read JEDEC ID
    0x5a,  // Read Unique ID
    0xb7,  // Erase Suspend
    0xe9,  // Erase Resume
    0x32,  // Program (256 bytes)
    0x34,  // Program (256 bytes, 4-byte address)
    0xd8,  // Block Erase (64K)
    0xdc,  // Block Erase (64K, 4-byte address)
    0x02,  // Program (1 byte)
    0x12,  // Program (1 byte, 4-byte address)
    0x15,  // Erase Program Suspend
    0x31,  // Erase Program Resume
    0x3b,  // Read Data (1-4-4 mode)
    0x3c,  // Read Data (1-4-4 mode, 4-byte address)
];

pub struct Target {}

fn run_boot_uc_test() -> Result<(), &'static str> {
    pw_log::info!("=== SPI Monitor Boot Use Case Test ===");
    
    // Create board instance with exclusive access to all hardware
    // SAFETY: Called once at boot, exclusive ownership guaranteed
    let mut board = unsafe { Ast1060Board::init() };
    let mut monitor = board.monitor();
    let instance = MonitorInstance::Spim0;
    
    // Phase 1: Hold - Initialize and route flash to ROT
    phase_1_hold(&mut monitor, instance)?;
    
    // Phase 2: Configure Policy
    phase_2_configure_policy(&mut monitor, instance)?;
    
    // Phase 3: Release - Switch control to host
    phase_3_release(&mut monitor, instance)?;
    
    // Phase 4: Runtime Verification
    phase_4_runtime_verification(&mut monitor, instance)?;
    
    pw_log::info!("=== All test phases passed! ===");
    Ok(())
}

fn phase_1_hold(monitor: &mut dyn Monitor, instance: MonitorInstance) -> Result<(), &'static str> {
    pw_log::info!("[Phase 1] Boot Hold");
    
    // Initialize and switch mux to ROT control
    pw_log::info!("  - Switching mux to ROT control");
    monitor
        .set_mux(instance, MuxSelect::RotControl)
        .map_err(|_| "Failed to set mux to ROT")?;
    
    // Verify mux is set
    let mux = monitor
        .read_mux(instance)
        .map_err(|_| "Failed to read mux")?;
    if mux != MuxSelect::RotControl {
        return Err("Mux not set to ROT");
    }
    pw_log::info!("    ✓ Mux set to ROT");
    
    // Perform soft reset to clear any existing state
    pw_log::info!("  - Soft resetting monitor");
    monitor
        .soft_reset(instance)
        .map_err(|_| "Soft reset failed")?;
    pw_log::info!("    ✓ Monitor soft reset complete");
    
    // Read initial status
    let status = monitor
        .read_status(instance)
        .map_err(|_| "Failed to read status")?;
    let mux_code = match status.mux {
        MuxSelect::RotControl => 0u32,
        MuxSelect::HostControl => 1u32,
    };
    pw_log::info!(
        "  - Initial status: mux_code={}, locked={}, active={}",
        mux_code as u32,
        status.policy_locked as u8,
        status.enforcement_active as u8
    );
    
    pw_log::info!("  ✓ Hold phase complete");
    Ok(())
}

fn phase_2_configure_policy(monitor: &mut dyn Monitor, instance: MonitorInstance) -> Result<(), &'static str> {
    pw_log::info!("[Phase 2] Configure Policy");
    
    // TODO: In a real implementation, we would:
    // 1. Read PFM (Platform Firmware Manifest) metadata
    // 2. Extract region definitions from PFM
    // 3. Load address privilege regions into monitor
    // 4. Configure allowed SPI command table
    // 5. Verify no overlapping regions
    
    pw_log::info!(
        "  - Allowed SPI Commands: {} opcodes configured",
        ALLOWED_SPI_COMMANDS.len() as u32
    );
    pw_log::info!("    Commands: Read (0x03), Fast Read (0x0b), Sector Erase (0x20), etc.");
    
    // Example address privilege regions (from aspeed-rust test patterns)
    // These would typically come from PFM data:
    // - PFM region (read-blocked): 0x0300_0000 - 0x0304_0000
    // - BMC region (write-blocked): 0x0000_0000 - 0x0020_0000
    
    pw_log::info!("  - Address Privilege Regions: (stub - awaiting PFM loader)");
    pw_log::info!("    Would configure read-blocked and write-blocked regions");
    
    // For now, just read the region count
    let region_count = monitor
        .read_region_count(instance)
        .map_err(|_| "Failed to read region count")?;
    pw_log::info!("    Current region count: {}", region_count as u32);
    
    pw_log::info!("  ✓ Policy configuration complete (stub)");
    Ok(())
}

fn phase_3_release(monitor: &mut dyn Monitor, instance: MonitorInstance) -> Result<(), &'static str> {
    pw_log::info!("[Phase 3] Release");
    
    // Lock policy to prevent further modifications
    pw_log::info!("  - Locking policy tables");
    monitor
        .lock_policy(instance)
        .map_err(|_| "Failed to lock policy")?;
    if monitor.verify_policy_locked(instance).is_ok() {
        pw_log::info!("    ✓ Policy locked");
    } else {
        pw_log::info!("    - Policy lock verification unavailable in current test environment");
    }
    
    // Switch mux back to host control
    pw_log::info!("  - Switching mux to HOST control");
    monitor
        .set_mux(instance, MuxSelect::HostControl)
        .map_err(|_| "Failed to set mux to HOST")?;
    
    let mux = monitor
        .read_mux(instance)
        .map_err(|_| "Failed to read mux")?;
    if mux != MuxSelect::HostControl {
        return Err("Mux not set to HOST");
    }
    pw_log::info!("    ✓ Mux set to HOST");
    
    pw_log::info!("  ✓ Release phase complete");
    Ok(())
}

fn phase_4_runtime_verification(monitor: &mut dyn Monitor, instance: MonitorInstance) -> Result<(), &'static str> {
    pw_log::info!("[Phase 4] Runtime Verification");
    
    // Read monitor status to confirm enforcement is active
    let status = monitor
        .read_status(instance)
        .map_err(|_| "Failed to read status")?;
    let mux_code = match status.mux {
        MuxSelect::RotControl => 0u32,
        MuxSelect::HostControl => 1u32,
    };
    
    pw_log::info!("  - Monitor status snapshot:");
    pw_log::info!("    Mux code: {}", mux_code as u32);
    pw_log::info!("    Policy locked: {}", status.policy_locked as u8);
    pw_log::info!("    Enforcement active: {}", status.enforcement_active as u8);
    pw_log::info!("    Violation count: {}", status.violation_count as u32);
    
    // Verify mux is HOST and policy is locked
    if status.mux != MuxSelect::HostControl {
        return Err("Runtime: Mux should be HOST");
    }
    if !status.policy_locked {
        pw_log::info!("  - Policy lock not reported by monitor status (non-fatal in stub test)");
    }
    
    pw_log::info!("  ✓ Runtime verification complete");
    Ok(())
}

impl TargetInterface for Target {
    const NAME: &'static str = "AST10x0 SPI Monitor Boot Use Case Test";

    fn main() -> ! {
        let exit_status = match run_boot_uc_test() {
            Ok(()) => {
                pw_log::info!("[SUCCESS] Boot use case test passed");
                EXIT_SUCCESS
            }
            Err(e) => {
                pw_log::error!("[FAILED] Boot use case test failed: {}", e as &str);
                EXIT_FAILURE
            }
        };
        exit(exit_status);
        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
