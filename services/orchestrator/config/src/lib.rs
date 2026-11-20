// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Schema for the per-board device table. Board device tables
//! (`target/<board>/devices.rs`) declare the values; no concrete line or
//! device is named here.
//!
//! Invariants are enforced in the `const fn` constructors, so an invalid
//! table is a build error and there is no validate step to forget. Checks
//! on board-defined types belong next to the table that gives them
//! meaning (`services/orchestrator/test/devices.rs` shows the pattern).

#![cfg_attr(not(test), no_std)]

/// One boot checkpoint: a probe the orchestrator evaluates, and how long
/// it waits. Retry policy is deliberately not table data: a retry
/// re-resets the device and re-runs the whole walk, so budgets are
/// per boot attempt and owned by the orchestrator state machine.
///
/// The probe is board-defined, the schema attaches no meaning to it and
/// names no probe kinds. Each board defines its own vocabulary (a small
/// enum: a GPIO line, a progress-register threshold, a message-path
/// readiness) and gives it meaning in its `EvidenceReader`. The probe is
/// a defunctionalized evidence check: data in the table instead of a
/// function, so the table stays printable, comparable, const-checkable,
/// and could one day be generated instead of written.
///
/// Fields are private so a checkpoint that violates the schema is
/// unrepresentable: [`new`](Self::new) is the only way in, and it checks.
///
/// # Example: three GPIO checkpoints
///
/// A BMC behind three GPIO ready lines (bl1 on pin 4, kernel on pin 5,
/// service on pin 6), all on the same SGPIOM bank. Each probe variant
/// maps to one `GpioBootMonitor` in the board's `EvidenceReader`, and
/// the walker (`CheckpointWalk`) walks them in declaration order.
///
/// ```ignore
/// #[derive(Debug, Clone, Copy)]
/// enum BmcProbe { Bl1, Kernel, Service }
///
/// const BMC: DeviceConfig<u8, BmcProbe> = DeviceConfig::new(
///     "bmc", 0,
///     &[
///         BootCheckpoint::new("bl1",     BmcProbe::Bl1,     Duration::from_millis(500)),
///         BootCheckpoint::new("kernel",  BmcProbe::Kernel,  Duration::from_secs(5)),
///         BootCheckpoint::new("service", BmcProbe::Service, Duration::from_secs(30)),
///     ],
/// );
///
/// // Pin binding at bring-up: one GpioBootMonitor per probe.
/// let bl1     = GpioBootMonitor::new(&sgpiom, Mask(1 << 4), ActivePolarity::ActiveHigh);
/// let kernel  = GpioBootMonitor::new(&sgpiom, Mask(1 << 5), ActivePolarity::ActiveHigh);
/// let service = GpioBootMonitor::new(&sgpiom, Mask(1 << 6), ActivePolarity::ActiveHigh);
///
/// // The board's EvidenceReader dispatches probe to monitor.
/// // See EvidenceReader's docs for the full impl pattern.
/// let bmc_walk = CheckpointWalk::new(bmc_reader, &BMC);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct BootCheckpoint<P> {
    name: &'static str,
    probe: P,
    timeout: core::time::Duration,
}

impl<P> BootCheckpoint<P> {
    /// Declares a checkpoint. `const`, so board tables run the checks at
    /// build time.
    ///
    /// # Panics
    ///
    /// Panics, a build error in const context, if `name` is empty or
    /// `timeout` is shorter than 1 ms (the walk rounds to whole
    /// milliseconds, so a sub-millisecond window would silently become
    /// zero).
    #[must_use]
    pub const fn new(name: &'static str, probe: P, timeout: core::time::Duration) -> Self {
        assert!(!name.is_empty(), "checkpoint name must not be empty");
        assert!(
            timeout.as_millis() >= 1,
            "checkpoint timeout must be at least 1 ms"
        );
        Self {
            name,
            probe,
            timeout,
        }
    }

    /// Names the checkpoint in failure reports ("bl1", "kernel", ...).
    /// Unique within a device's checkpoint list.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Board-defined probe, resolved by the board's `EvidenceReader`
    /// (in `orchestrator-capabilities`). Data rather than a function, so
    /// the table stays pure data, the type-level docs say why.
    #[must_use]
    pub const fn probe(&self) -> &P {
        &self.probe
    }

    /// Window for one attempt at this checkpoint. Expiry is the boot
    /// walk's own judgment; hung devices report nothing.
    ///
    /// The orchestrator state machine never sees this value, it is
    /// clockless. The walk consumes the windows and reports expiry as a
    /// failed attempt; a component's whole boot timeout is nothing more
    /// than its walk over these windows, in order.
    #[must_use]
    pub const fn timeout(&self) -> core::time::Duration {
        self.timeout
    }
}

/// One managed downstream device, as declared by the board config.
///
/// Generic over the board's reset signal type `R` (which must match the
/// `ResetId` of the reset controller behind the board's `BootControl`
/// implementation) and its boot-probe vocabulary `P`, for the same
/// reason: probes are board-specific.
///
/// Deliberately says nothing about attestation or commit requirements:
/// those follow from what kind of device this is (iRoT-backed or
/// symbiont, the orchestrator's `ComponentKind`), not from a table
/// setting — a second knob would only let the two disagree.
///
/// Fields are private so a device entry that violates the schema is
/// unrepresentable: [`new`](Self::new) is the only way in, and it checks.
///
/// The third parameter `G` is the board's filter id type, used by the
/// optional [`FlashGuardConfig`]: which SPI filter fronts this device's
/// flash. A board with no filters names any type for it (`()` will do)
/// and never calls [`with_flash_guard`](Self::with_flash_guard).
#[derive(Debug, Clone, Copy)]
pub struct DeviceConfig<R, P: 'static, G> {
    name: &'static str,
    reset_signal: R,
    checkpoints: &'static [BootCheckpoint<P>],
    flash_guard: Option<FlashGuardConfig<G>>,
}

impl<R, P, G> DeviceConfig<R, P, G> {
    /// Declares a managed device. `const`, so board tables run the checks
    /// at build time.
    ///
    /// # Panics
    ///
    /// Panics — a build error in const context — if `name` is empty, if
    /// `checkpoints` is empty, or if two checkpoints share a name
    /// (failure reports identify a checkpoint by name; a duplicate would
    /// make them ambiguous).
    #[must_use]
    pub const fn new(
        name: &'static str,
        reset_signal: R,
        checkpoints: &'static [BootCheckpoint<P>],
    ) -> Self {
        assert!(!name.is_empty(), "device name must not be empty");
        assert!(
            !checkpoints.is_empty(),
            "device must declare at least one boot checkpoint"
        );
        let mut c = 0;
        while c < checkpoints.len() {
            let mut d = c + 1;
            while d < checkpoints.len() {
                assert!(
                    !str_eq(checkpoints[c].name, checkpoints[d].name),
                    "checkpoint names must be unique per device"
                );
                d += 1;
            }
            c += 1;
        }
        Self {
            name,
            reset_signal,
            checkpoints,
            flash_guard: None,
        }
    }

    /// The device's name in reports and logs.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Reset signal id, passed to HalBootControl::new.
    #[must_use]
    pub const fn reset_signal(&self) -> &R {
        &self.reset_signal
    }

    /// Boot checkpoints, in the order the device passes them. The device
    /// counts as booted when the last one is reached; a checkpoint whose
    /// window expires fails the attempt — whether to retry or recover is
    /// the orchestrator's decision, not table data.
    #[must_use]
    pub const fn checkpoints(&self) -> &'static [BootCheckpoint<P>] {
        self.checkpoints
    }

    /// The filter that guards this device's flash, if any. `None` means the
    /// orchestrator's `ArmFlashGuard` for this device is a no-op.
    #[must_use]
    pub const fn flash_guard(&self) -> Option<&FlashGuardConfig<G>> {
        self.flash_guard.as_ref()
    }
}

// `G: Copy` so the overwritten `None` has no drop glue: a `const fn` cannot
// run a destructor, and filter ids are small `Copy` enums anyway.
impl<R, P, G: Copy> DeviceConfig<R, P, G> {
    /// Declares that a SPI filter fronts this device's flash, and how the
    /// orchestrator arms it. `const`, so it composes with [`new`](Self::new)
    /// in a board table.
    #[must_use]
    pub const fn with_flash_guard(mut self, guard: FlashGuardConfig<G>) -> Self {
        self.flash_guard = Some(guard);
        self
    }
}

/// How the orchestrator arms the SPI filter in front of one device's flash.
///
/// Policy lives here, in the table; mechanism lives in the adapter
/// (`services/orchestrator/adapters/hal`); sequencing lives in the reducer,
/// which knows one verb (arm) and one noun (tampered). Design:
/// `docs/src/design/orchestrator/orchestrator-platform.md`, section "Flash
/// guarding".
///
/// Fields are private so a guard entry that violates the schema is
/// unrepresentable: [`new`](Self::new) is the only way in, and it checks.
#[derive(Debug, Clone, Copy)]
pub struct FlashGuardConfig<G> {
    filter: G,
    opcodes: &'static [u8],
    regions: RegionSource,
    lock: FilterLockPolicy,
    violations: ViolationPolicy,
}

impl<G> FlashGuardConfig<G> {
    /// Declares a guard. `const`, so board tables run the checks at build
    /// time.
    ///
    /// # Panics
    ///
    /// Panics, a build error in const context, if `opcodes` is empty (a
    /// filter that passes no command blocks the device's every fetch, which
    /// is a wiring mistake and not a policy), or if a
    /// [`RegionSource::Table`] span has `last < first`, or if its spans are
    /// not in strictly ascending, non-overlapping order (a region table has
    /// no defined meaning for overlapping entries, and sorted input lets the
    /// adapter and a reviewer check coverage by eye).
    #[must_use]
    pub const fn new(
        filter: G,
        opcodes: &'static [u8],
        regions: RegionSource,
        lock: FilterLockPolicy,
        violations: ViolationPolicy,
    ) -> Self {
        assert!(
            !opcodes.is_empty(),
            "flash guard must allow at least one opcode"
        );
        if let RegionSource::Table(spans) = regions {
            validate_spans(spans);
        }
        Self {
            filter,
            opcodes,
            regions,
            lock,
            violations,
        }
    }

    /// Board filter id, resolved by the board's `HalFlashGuard` wiring to a
    /// filter instance the way `reset_signal` resolves to a line.
    #[must_use]
    pub const fn filter(&self) -> &G {
        &self.filter
    }

    /// Opcodes the device may issue. A property of the flash part, not of
    /// the image; fixed at build time.
    #[must_use]
    pub const fn opcodes(&self) -> &'static [u8] {
        self.opcodes
    }

    /// Where the writable spans come from.
    #[must_use]
    pub const fn regions(&self) -> RegionSource {
        self.regions
    }

    /// What the adapter locks after a successful arm.
    #[must_use]
    pub const fn lock(&self) -> FilterLockPolicy {
        self.lock
    }

    /// What the driver does with a blocked transaction.
    #[must_use]
    pub const fn violations(&self) -> ViolationPolicy {
        self.violations
    }
}

/// Where a guarded device's writable spans come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionSource {
    /// Fixed layout: `(first, last)` byte offsets, inclusive, in strictly
    /// ascending non-overlapping order. Everything outside them is read-only
    /// to the device.
    Table(&'static [(u32, u32)]),
    /// The spans are read from the verified image's manifest by the board's
    /// verifier at `VerifyFirmware` and handed to `ArmFlashGuard`.
    Manifest,
}

/// What the adapter freezes once the rules are in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
///
/// The opcode allow list depends on the flash part and never changes after
/// the first boot, so it is locked. The region rules depend on the image
/// that was just verified and must be rewritten when an update changes the
/// layout; locking them would defend against a compromised RoT runtime,
/// which is the trust anchor, at the price of never re-arming. ASPEED's PFR
/// firmware on the same hardware locks nothing and rewrites the tables on
/// every host reset (see `target/ast10x0/peripherals/spimonitor/README.md`);
/// this design locks more than that and less than everything.
pub enum FilterLockPolicy {
    /// Lock the opcode allow list and the control registers; leave the
    /// privilege tables writable by the RoT, so a verified image with a new
    /// layout can be re-armed. Default.
    #[default]
    CommandsOnly,
    /// Lock everything, once, on the first successful arm. Later arms must
    /// present the same rules or fail. For boards that never update host
    /// firmware in the field.
    All,
}

/// What the driver does with a transaction the filter blocked.
///
/// A device writing into its own protected firmware is either compromised
/// or badly broken, and `Recovering` is the state the orchestrator already
/// has for a device it no longer trusts; so under `Recover` a blocked write
/// is evidence, not noise. ASPEED's PFR firmware prints the log entry and
/// carries on, which is `Log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViolationPolicy {
    /// Report every violation and carry on.
    Log,
    /// Report every violation; a blocked write into a protected span also
    /// produces `FlashTampered`, which the reducer handles like corruption.
    /// Blocked reads and opcodes are report-only under both policies: hosts
    /// probe, and a stray unsupported opcode is not a security event.
    /// Default.
    #[default]
    Recover,
}

/// Spans are `(first, last)` inclusive, each well-formed, strictly ascending
/// and non-overlapping.
const fn validate_spans(spans: &[(u32, u32)]) {
    let mut i = 0;
    while i < spans.len() {
        let (first, last) = spans[i];
        assert!(first <= last, "flash guard span ends before it starts");
        if i > 0 {
            let (_, prev_last) = spans[i - 1];
            assert!(
                prev_last < first,
                "flash guard spans must be ascending and non-overlapping"
            );
        }
        i += 1;
    }
}

// `==` on `&str` is not const; compare bytes by hand.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    // Board tables run the constructors at compile time, where a
    // rejection is a build error nobody can assert on. These tests call
    // them at runtime to prove the reject paths actually fire.

    const CHECKPOINT: BootCheckpoint<u8> =
        BootCheckpoint::new("boot-complete", 0, Duration::from_secs(1));

    // Same name, different signal: each checkpoint is individually valid,
    // so the pair only trips the device-level duplicate check.
    const CHECKPOINT_DUP: BootCheckpoint<u8> =
        BootCheckpoint::new("boot-complete", 1, Duration::from_secs(1));

    #[test]
    fn accepts_a_valid_table() {
        let device = DeviceConfig::<_, _, ()>::new("dev", 0u8, &[CHECKPOINT]);
        assert_eq!(device.name(), "dev");
        assert_eq!(*device.reset_signal(), 0);
        assert_eq!(device.checkpoints().len(), 1);
        assert_eq!(device.checkpoints()[0].name(), "boot-complete");
        assert_eq!(*device.checkpoints()[0].probe(), 0);
        assert_eq!(device.checkpoints()[0].timeout(), Duration::from_secs(1));
        assert!(device.flash_guard().is_none());
    }

    const READ_ONLY: &[u8] = &[0x03, 0x0b, 0x9f];
    const TWO_WINDOWS: &[(u32, u32)] = &[(0x0000_0000, 0x0000_ffff), (0x0010_0000, 0x001f_ffff)];

    #[test]
    fn accepts_a_guarded_device() {
        let guard = FlashGuardConfig::new(
            2u8,
            READ_ONLY,
            RegionSource::Table(TWO_WINDOWS),
            FilterLockPolicy::CommandsOnly,
            ViolationPolicy::Recover,
        );
        let device = DeviceConfig::new("dev", 0u8, &[CHECKPOINT]).with_flash_guard(guard);
        let guard = device.flash_guard().expect("guard declared");
        assert_eq!(*guard.filter(), 2);
        assert_eq!(guard.opcodes(), READ_ONLY);
        assert_eq!(guard.regions(), RegionSource::Table(TWO_WINDOWS));
        assert_eq!(guard.lock(), FilterLockPolicy::CommandsOnly);
        assert_eq!(guard.violations(), ViolationPolicy::Recover);
    }

    #[test]
    fn manifest_regions_need_no_table() {
        let guard = FlashGuardConfig::new(
            2u8,
            READ_ONLY,
            RegionSource::Manifest,
            FilterLockPolicy::default(),
            ViolationPolicy::default(),
        );
        assert_eq!(guard.regions(), RegionSource::Manifest);
        assert_eq!(guard.lock(), FilterLockPolicy::CommandsOnly);
        assert_eq!(guard.violations(), ViolationPolicy::Recover);
    }

    #[test]
    #[should_panic(expected = "at least one opcode")]
    fn rejects_an_empty_allow_list() {
        let _ = FlashGuardConfig::new(
            2u8,
            &[],
            RegionSource::Manifest,
            FilterLockPolicy::CommandsOnly,
            ViolationPolicy::Recover,
        );
    }

    #[test]
    #[should_panic(expected = "ends before it starts")]
    fn rejects_a_reversed_span() {
        let _ = FlashGuardConfig::new(
            2u8,
            READ_ONLY,
            RegionSource::Table(&[(0x1000, 0x0fff)]),
            FilterLockPolicy::CommandsOnly,
            ViolationPolicy::Recover,
        );
    }

    #[test]
    #[should_panic(expected = "ascending and non-overlapping")]
    fn rejects_overlapping_spans() {
        let _ = FlashGuardConfig::new(
            2u8,
            READ_ONLY,
            RegionSource::Table(&[(0x0000, 0xffff), (0x8000, 0x1_ffff)]),
            FilterLockPolicy::CommandsOnly,
            ViolationPolicy::Recover,
        );
    }

    #[test]
    #[should_panic(expected = "ascending and non-overlapping")]
    fn rejects_unsorted_spans() {
        let _ = FlashGuardConfig::new(
            2u8,
            READ_ONLY,
            RegionSource::Table(&[(0x1_0000, 0x1_ffff), (0x0000, 0xffff)]),
            FilterLockPolicy::CommandsOnly,
            ViolationPolicy::Recover,
        );
    }

    #[test]
    #[should_panic(expected = "checkpoint names must be unique")]
    fn rejects_duplicate_checkpoint_names() {
        let _ = DeviceConfig::<_, _, ()>::new("dev", 0u8, &[CHECKPOINT, CHECKPOINT_DUP]);
    }

    #[test]
    #[should_panic(expected = "device name must not be empty")]
    fn rejects_an_empty_device_name() {
        let _ = DeviceConfig::<_, _, ()>::new("", 0u8, &[CHECKPOINT]);
    }

    #[test]
    #[should_panic(expected = "at least one boot checkpoint")]
    fn rejects_an_empty_checkpoint_list() {
        let _ = DeviceConfig::<_, _, ()>::new("dev", 0u8, &[] as &[BootCheckpoint<u8>]);
    }

    #[test]
    #[should_panic(expected = "checkpoint name must not be empty")]
    fn rejects_an_empty_checkpoint_name() {
        let _ = BootCheckpoint::new("", 0u8, Duration::from_secs(1));
    }

    #[test]
    #[should_panic(expected = "checkpoint timeout must be at least 1 ms")]
    fn rejects_a_zero_checkpoint_timeout() {
        let _ = BootCheckpoint::new("boot-complete", 0u8, Duration::ZERO);
    }

    #[test]
    #[should_panic(expected = "checkpoint timeout must be at least 1 ms")]
    fn rejects_a_sub_millisecond_checkpoint_timeout() {
        let _ = BootCheckpoint::new("boot-complete", 0u8, Duration::from_micros(999));
    }
}
