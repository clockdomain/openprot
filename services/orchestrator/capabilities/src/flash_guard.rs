// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! The [`FlashGuard`] capability: the rules a filter enforces on one managed
//! device's flash, and who owns the bus to that flash.
//!
//! The reducer's `ArmFlashGuard(id)` effect executes against this. It is
//! deliberately coarser than the SPI filter HAL: "make these rules true", not
//! "set this bit", so the reducer and its tests never see opcodes, privilege
//! tables or lock registers. How a span becomes a hardware rule is the
//! adapter's business (`services/orchestrator/adapters/hal`); which filter
//! guards which device, what to lock and how to react are board table data.
//! Design: `docs/src/design/orchestrator/orchestrator-platform.md`, section
//! "Flash guarding".

/// The rules a guard enforces for one device.
///
/// Spans are `(first, last)` byte offsets in the device's flash, inclusive at
/// both ends so a whole 4 GiB device is expressible. Borrowed rather than
/// owned: the sources are a board table (`'static`) or the spans a verifier
/// read from the signed manifest of the image it just authenticated, and a
/// borrow lets the capability stay free of any region-count parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GuardRules<'a> {
    /// Spans the device may write. Everything else is read-only to it.
    pub writable: &'a [(u32, u32)],
    /// Spans the device may not read. Usually empty.
    pub unreadable: &'a [(u32, u32)],
}

/// Who drives the guarded flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// The root of trust's own controller drives the flash: for reading and
    /// verifying the image, and for recovery writes.
    Rot,
    /// The guarded device drives the flash, through the filter.
    Device,
}

/// One transaction the guard blocked.
///
/// Addresses are byte offsets in the device's flash, at whatever granularity
/// the hardware logs them. Intentionally exhaustive: adding a variant must
/// make the driver's policy mapping fail to compile until it is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// A write into a span the device may not write.
    BlockedWrite(u32),
    /// A read from a span the device may not read.
    BlockedRead(u32),
    /// A command outside the allow list.
    BlockedOpcode(u8),
}

/// The flash guard of one managed device.
///
/// Like its peer capabilities this is a dependency-free contract bounded only
/// by [`core::error::Error`], so a mock or an IPC proxy implements it without
/// a HAL.
pub trait FlashGuard {
    /// The error type of this guard's mechanism.
    ///
    /// Bounded by [`core::error::Error`] so the caller gets `Display` and a
    /// `source()` cause chain. Error categories are implementation-defined.
    type Error: core::error::Error;

    /// Loads `rules`, enforces them, and locks whatever the board's lock
    /// policy says to lock.
    ///
    /// Idempotent: arming twice with the same rules succeeds. Arming with
    /// rules that differ from what a locked table holds fails. `Ok` means the
    /// rules are in force and verified by read-back, not merely requested; a
    /// filter that cannot be proven armed must not get a device released
    /// behind it, so the driver fails closed on `Err`.
    fn arm(&mut self, rules: &GuardRules<'_>) -> Result<(), Self::Error>;

    /// Hands the flash to `owner`. Does not change the rules.
    fn connect(&mut self, owner: Owner) -> Result<(), Self::Error>;

    /// Returns and consumes the oldest unread violation, if any.
    fn take_violation(&mut self) -> Result<Option<Violation>, Self::Error>;
}

/// The guard type of a board on which no device has a flash filter.
///
/// Uninhabited, so it can fill the board's `FlashGuard` type slot but can
/// never be constructed or bound to a device: a board that has no guard
/// cannot accidentally arm one that does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoFlashGuard {}

impl FlashGuard for NoFlashGuard {
    type Error = core::convert::Infallible;

    fn arm(&mut self, _rules: &GuardRules<'_>) -> Result<(), Self::Error> {
        match *self {}
    }

    fn connect(&mut self, _owner: Owner) -> Result<(), Self::Error> {
        match *self {}
    }

    fn take_violation(&mut self) -> Result<Option<Violation>, Self::Error> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    // Implements FlashGuard with no HAL dependency: the contract must be
    // satisfiable from any stack (mock, IPC proxy, simulator). A HAL-bound
    // `Error` type would stop this compiling.
    /// The spans a mock was armed with: `(writable, unreadable)`.
    type ArmedRules = (std::vec::Vec<(u32, u32)>, std::vec::Vec<(u32, u32)>);

    struct MockGuard {
        armed: Option<ArmedRules>,
        locked: bool,
        owner: Owner,
        log: std::collections::VecDeque<Violation>,
        fail: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum MockFault {
        Hardware,
        RulesLocked,
    }

    impl core::fmt::Display for MockFault {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(match self {
                MockFault::Hardware => "mock guard fault",
                MockFault::RulesLocked => "rules differ from the locked table",
            })
        }
    }

    impl core::error::Error for MockFault {}

    impl MockGuard {
        fn new() -> Self {
            Self {
                armed: None,
                locked: false,
                owner: Owner::Rot,
                log: Default::default(),
                fail: false,
            }
        }
    }

    impl FlashGuard for MockGuard {
        type Error = MockFault;

        fn arm(&mut self, rules: &GuardRules<'_>) -> Result<(), MockFault> {
            if self.fail {
                return Err(MockFault::Hardware);
            }
            let wanted = (rules.writable.to_vec(), rules.unreadable.to_vec());
            if self.locked && self.armed.as_ref() != Some(&wanted) {
                return Err(MockFault::RulesLocked);
            }
            self.armed = Some(wanted);
            self.locked = true;
            Ok(())
        }

        fn connect(&mut self, owner: Owner) -> Result<(), MockFault> {
            if self.fail {
                return Err(MockFault::Hardware);
            }
            self.owner = owner;
            Ok(())
        }

        fn take_violation(&mut self) -> Result<Option<Violation>, MockFault> {
            if self.fail {
                return Err(MockFault::Hardware);
            }
            Ok(self.log.pop_front())
        }
    }

    const BOOT_BLOCK: &[(u32, u32)] = &[(0x0010_0000, 0x001f_ffff)];

    #[test]
    fn contract_is_implementable_without_the_hal() {
        let mut guard = MockGuard::new();
        let rules = GuardRules {
            writable: BOOT_BLOCK,
            unreadable: &[],
        };

        guard.arm(&rules).expect("arm failed");
        guard
            .arm(&rules)
            .expect("re-arm with the same rules failed"); // idempotent
        guard.connect(Owner::Device).expect("connect failed");

        assert_eq!(guard.owner, Owner::Device);
        assert_eq!(
            guard.armed,
            Some((BOOT_BLOCK.to_vec(), std::vec::Vec::new()))
        );
    }

    #[test]
    fn different_rules_against_a_locked_table_are_refused() {
        let mut guard = MockGuard::new();
        guard
            .arm(&GuardRules {
                writable: BOOT_BLOCK,
                unreadable: &[],
            })
            .unwrap();

        let err = guard
            .arm(&GuardRules {
                writable: &[(0, 0xffff)],
                unreadable: &[],
            })
            .expect_err("expected the locked table to refuse new rules");

        assert_eq!(err, MockFault::RulesLocked);
    }

    #[test]
    fn violations_drain_oldest_first() {
        let mut guard = MockGuard::new();
        guard.log.extend([
            Violation::BlockedWrite(0x0010_4000),
            Violation::BlockedOpcode(0xc7),
            Violation::BlockedRead(0x0),
        ]);

        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedWrite(0x0010_4000))
        );
        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedOpcode(0xc7))
        );
        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedRead(0x0))
        );
        assert_eq!(guard.take_violation().unwrap(), None);
    }

    #[test]
    fn errors_surface_through_the_generic_seam() {
        let mut guard = MockGuard::new();
        guard.fail = true;

        let err = guard
            .connect(Owner::Device)
            .expect_err("expected the guard fault");

        // Display comes from the core::error::Error bound, not a Debug dump.
        assert_eq!(err.to_string(), "mock guard fault");
    }

    /// Compile-time fence: `NoFlashGuard` satisfies the contract so a board
    /// without filters can name it, and cannot be instantiated.
    fn _no_guard_is_a_flash_guard<G: FlashGuard>() {}

    #[test]
    fn no_flash_guard_fills_the_type_slot() {
        _no_guard_is_a_flash_guard::<NoFlashGuard>();
        assert_eq!(core::mem::size_of::<Option<NoFlashGuard>>(), 0);
    }
}
