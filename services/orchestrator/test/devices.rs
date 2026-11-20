// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Mock board: a device table exercising every device archetype the
//! orchestrator manages. Not a real board — consumed by host tests and QEMU
//! runs until a hardware target declares its own table.

#![no_std]

use core::time::Duration;

use orchestrator_config::{
    BootCheckpoint, DeviceConfig, FilterLockPolicy, FlashGuardConfig, RegionSource, ViolationPolicy,
};

/// The mock board's boot-probe vocabulary. The schema carries these
/// opaquely; only this board's `EvidenceReader` gives them meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockProbe {
    /// A boot-complete GPIO line, by index.
    Gpio(u8),
    /// The device's MCTP endpoint answers as ready.
    MctpReady,
    /// The device sends a heartbeat message (latched; the reset path
    /// clears it).
    Heartbeat,
}

/// The mock board's SPI filters, by monitor index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockFilter(pub u8);

/// Commands the BMC's flash part needs: read, fast read, JEDEC id, status,
/// write enable, page program, 4 KiB and 64 KiB erase.
const BMC_OPCODES: &[u8] = &[0x03, 0x0b, 0x9f, 0x05, 0x06, 0x02, 0x20, 0xd8];

/// The BMC may write its own log and config partitions; the rest of its
/// flash (bootloader, kernel, root filesystem) is read-only to it.
const BMC_WRITABLE: &[(u32, u32)] = &[(0x0200_0000, 0x020f_ffff), (0x03f0_0000, 0x03ff_ffff)];

/// Declaration order is the boot order: the orchestrator releases devices
/// top to bottom, one at a time. This table is the authority — the
/// orchestrator's chain of trust is built from it, never beside it.
///
/// The mock board's reset controller addresses reset lines by plain index,
/// so the reset id type is `u8`.
pub const MANAGED_DEVICES: &[DeviceConfig<u8, MockProbe, MockFilter>] = &[
    // Direct-flash SPI device (BMC archetype): the eRoT fronts its flash
    // through SPI monitor 0, so it carries a flash guard. Single
    // checkpoint: it raises a boot-complete GPIO.
    DeviceConfig::new(
        "bmc",
        7,
        &[BootCheckpoint::new(
            "boot-complete",
            MockProbe::Gpio(12),
            Duration::from_secs(90),
        )],
    )
    .with_flash_guard(FlashGuardConfig::new(
        MockFilter(0),
        BMC_OPCODES,
        RegionSource::Table(BMC_WRITABLE),
        FilterLockPolicy::CommandsOnly,
        ViolationPolicy::Recover,
    )),
    // PLDM device (NIC archetype): self-updating, SPDM-capable. Two
    // checkpoints, exercising the multi-checkpoint path: transport up
    // first, then proof the workload is alive.
    DeviceConfig::new(
        "nic",
        3,
        &[
            BootCheckpoint::new("mctp-ready", MockProbe::MctpReady, Duration::from_secs(20)),
            BootCheckpoint::new("heartbeat", MockProbe::Heartbeat, Duration::from_secs(10)),
        ],
    ),
];

/// Board-local checks the schema constructors cannot do — they know the
/// schema's shape, not this board's meanings. Const-fence pattern: a bad
/// probe fails the build.
const fn validate_probes(devices: &[DeviceConfig<u8, MockProbe, MockFilter>]) {
    let mut i = 0;
    while i < devices.len() {
        let checkpoints = devices[i].checkpoints();
        let mut c = 0;
        while c < checkpoints.len() {
            if let MockProbe::Gpio(line) = *checkpoints[c].probe() {
                // The mock ready-line bank packs 32 lines, SGPIO-style.
                assert!(line < 32, "gpio probe names a line outside the bank");
            }
            c += 1;
        }
        if let Some(guard) = devices[i].flash_guard() {
            // The mock board has four SPI monitors, like the AST1060.
            assert!(
                guard.filter().0 < 4,
                "flash guard names a monitor the board lacks"
            );
        }
        i += 1;
    }
}

const _: () = validate_probes(MANAGED_DEVICES);
