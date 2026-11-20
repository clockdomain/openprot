// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Blocking HAL contract for SPI flash filters.
//!
//! A SPI flash filter sits between a host and the host's flash and decides
//! which transactions get through. Chips split that job very differently, so
//! this crate has one small core trait, [`SpiFilter`], and one capability
//! trait per independent question a filter answers:
//!
//! | Question | Trait |
//! |---|---|
//! | What does a rule match? | [`OpcodeRules`], [`RegionRules`] |
//! | How is it enforced? | [`SpiFilter`] and its `Mode` |
//! | What happens on a violation? | [`ViolationResponse`] |
//! | How do I find out? | [`ViolationReport`] |
//! | Who owns the bus, and when may policy change? | [`BusIsolation`], [`PolicyPhases`], [`Lockable`] |
//! | What flash is behind it? | [`AddressMode`], [`FlashExtent`], [`DualFlash`] |
//!
//! Where chips disagree, a trait uses an associated type bounded by a
//! conversion from the small value every chip shares ([`Action`],
//! [`FlashOp`], [`Span`], [`FlashAddress`]). Portable code builds the shared
//! value and converts; chip-aware code uses the native type. The generic
//! functions at the bottom ([`allow_only`], [`write_protect`],
//! [`writable_only`], [`drain_violations`], [`lock_down`]) are what boot code
//! should call.
//!
//! The design, its hardware survey and the AST1060 mapping are in
//! `docs/src/design/spi-filter.md`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use hal_flash_driver::FlashAddress;

// ================================================================ errors ==

/// Common filter failures. Backends map their own errors onto these so
/// generic code can react without knowing the chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The command table or region table is full, or `set_all(Pass)` was
    /// asked of a chip with a bounded allow-list.
    CapacityExceeded,
    /// The chip cannot parse this command (it has no descriptor for it).
    UnsupportedCommand,
    /// The region is misaligned, too large, or outside what the filter sees.
    InvalidRegion,
    /// The target register or table is locked.
    Locked,
    /// A read-back after a write did not match.
    VerificationFailed,
    /// The operation exists in the trait but not on this chip or in this mode.
    Unsupported,
    /// Anything else the hardware reported.
    Hardware,
}

/// A backend's error type, reducible to an [`ErrorKind`].
pub trait Error: core::fmt::Debug {
    /// The common failure this error is an instance of.
    fn kind(&self) -> ErrorKind;
}

impl Error for core::convert::Infallible {
    fn kind(&self) -> ErrorKind {
        match *self {}
    }
}

/// Supplies the one error type a backend uses for every trait in this crate.
pub trait ErrorType {
    /// The backend's error type.
    type Error: Error;
}

// ========================================================= shared values ==

/// A SPI flash command byte. Fixed by the protocol, so concrete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode(pub u8);

/// The two outcomes every chip has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Let the transaction through.
    Pass,
    /// Stop the transaction.
    Block,
}

/// The two kinds of flash access every region-capable chip distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashOp {
    /// Read data.
    Read,
    /// Program or erase.
    Write,
}

/// How many address bytes a flash command carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrWidth {
    /// 3-byte addresses, up to 16 MiB.
    Three,
    /// 4-byte addresses, up to 4 GiB.
    Four,
}

/// Whether the writable (inactive) flash has been written since last cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashState {
    /// Not written since the state was last cleared.
    Clean,
    /// Written since the state was last cleared.
    Dirty,
}

/// An inclusive address range.
///
/// Inclusive so that a whole 4 GiB device is expressible and no
/// start-plus-length sum can overflow. Valid by construction: `first <=
/// last`. Chips convert it into their own checked `Region`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    first: FlashAddress,
    last: FlashAddress,
}

impl Span {
    /// A span from `first` to `last` inclusive, or `None` if `last`
    /// precedes `first`.
    pub fn new(first: FlashAddress, last: FlashAddress) -> Option<Self> {
        if last.offset() < first.offset() {
            return None;
        }
        Some(Self { first, last })
    }

    /// The first address in the span.
    pub fn first(&self) -> FlashAddress {
        self.first
    }

    /// The last address in the span.
    pub fn last(&self) -> FlashAddress {
        self.last
    }
}

// ================================================================== core ==

/// The one trait every filter has.
pub trait SpiFilter: ErrorType {
    /// How the chip enforces policy (off, passthrough, passive, active...).
    /// No shared part exists, so only chip-aware code picks a mode.
    type Mode: Copy;
    /// What the chip can do with a matching command.
    type Rule: Copy + From<Action>;

    /// Selects how the filter enforces policy.
    fn set_mode(&mut self, mode: Self::Mode) -> Result<(), Self::Error>;
    /// The current mode.
    fn mode(&self) -> Result<Self::Mode, Self::Error>;
}

// ========================================================== capabilities ==

/// Rules that match on the command byte.
pub trait OpcodeRules: SpiFilter {
    /// How many opcodes can pass at once. `None` means all 256. A chip with
    /// reserved slots reports the table size; the usable count is lower.
    const PASS_CAPACITY: Option<usize>;

    /// Gives every opcode the same outcome. `Pass` fails with
    /// [`ErrorKind::CapacityExceeded`] where the allow-list is bounded.
    fn set_all(&mut self, action: Action) -> Result<(), Self::Error>;

    /// Sets the outcome for one opcode.
    ///
    /// A chip that must parse the command to find its address fails with
    /// [`ErrorKind::UnsupportedCommand`] for an opcode it has no descriptor
    /// for; adding a described command is an inherent, chip-specific
    /// operation.
    fn set_rule(&mut self, opcode: Opcode, rule: Self::Rule) -> Result<(), Self::Error>;

    /// The outcome currently set for one opcode.
    fn rule(&self, opcode: Opcode) -> Result<Self::Rule, Self::Error>;
}

/// Rules that match on the flash address and the kind of access.
pub trait RegionRules: SpiFilter {
    /// The kinds of access this chip has rules for. `TryFrom`, because a
    /// chip may have rules for only some operations.
    type Op: Copy + TryFrom<FlashOp>;
    /// An address range the chip can express, checked for alignment, size
    /// and reach on conversion.
    type Region: Copy + TryFrom<Span>;
    /// `Some(n)`: a table of `n` regions. `None`: a bitmap over the address
    /// space, so any number of regions at the chip's granularity.
    const REGION_CAPACITY: Option<usize>;

    /// Outcome for addresses no region covers. Call it before adding
    /// regions: on a bitmap chip it rewrites the whole table.
    fn set_default(&mut self, op: Self::Op, action: Action) -> Result<(), Self::Error>;

    /// Sets the outcome for one range and kind of access. Regions carry
    /// only pass or block; no surveyed chip has richer per-region behaviour.
    fn set_region_rule(
        &mut self,
        region: Self::Region,
        op: Self::Op,
        action: Action,
    ) -> Result<(), Self::Error>;
}

/// Choose what the chip does when a rule is broken.
pub trait ViolationResponse: SpiFilter {
    /// The chip's set of reactions.
    type Response: Copy;

    /// Selects the reaction to a violation.
    fn set_response(&mut self, response: Self::Response) -> Result<(), Self::Error>;
}

/// Read back what was blocked.
pub trait ViolationReport: SpiFilter {
    /// One recorded violation.
    type Violation;

    /// Returns and consumes the oldest pending violation, if any. A chip
    /// whose log has no hardware read pointer keeps the cursor in the driver.
    fn take_violation(&mut self) -> Result<Option<Self::Violation>, Self::Error>;
}

/// Control who is electrically connected to the flash.
///
/// What a connection means (host, root of trust, nobody) is board wiring, so
/// the type is the chip's and the board gives it meaning.
pub trait BusIsolation: SpiFilter {
    /// The chip's selector for who is wired to the flash.
    type Connection: Copy;

    /// Wires the flash to `connection`.
    fn connect(&mut self, connection: Self::Connection) -> Result<(), Self::Error>;
    /// Who is currently wired to the flash.
    fn connection(&self) -> Result<Self::Connection, Self::Error>;
}

/// Freeze part of the configuration until reset.
///
/// Which parts can be frozen separately is the chip's business. Depends only
/// on [`ErrorType`] so it can move to a shared module when a second
/// peripheral needs it.
pub trait Lockable: ErrorType {
    /// One lockable part of the configuration.
    type Target: Copy;

    /// Freezes `target` until the next reset.
    fn lock(&mut self, target: Self::Target) -> Result<(), Self::Error>;
    /// Whether `target` is frozen.
    fn is_locked(&self, target: Self::Target) -> Result<bool, Self::Error>;
}

/// The filter must know how many address bytes commands carry, or it cannot
/// find the address in a transaction.
pub trait AddressMode: SpiFilter {
    /// Tells the filter how many address bytes commands carry.
    fn set_addr_width(&mut self, width: AddrWidth) -> Result<(), Self::Error>;
    /// The address width the filter assumes.
    fn addr_width(&self) -> Result<AddrWidth, Self::Error>;
}

/// Provisional (MCX A577, Cerberus). Tell the filter how large the flash is,
/// as its last valid address.
pub trait FlashExtent: SpiFilter {
    /// The chip's representation of the last valid address.
    type Extent: Copy + TryFrom<FlashAddress>;

    /// Tells the filter the extent of the flash behind it.
    fn set_extent(&mut self, extent: Self::Extent) -> Result<(), Self::Error>;
}

/// Provisional (MCX A577, Cerberus). Two flashes behind one filter: the
/// active one is read-only, the other is the writable update target.
pub trait DualFlash: SpiFilter {
    /// One of the two flashes.
    type ChipSelect: Copy;

    /// Chooses which flash is active (read-only).
    fn set_active(&mut self, chip_select: Self::ChipSelect) -> Result<(), Self::Error>;
    /// Whether the inactive flash has been written since last cleared.
    fn inactive_state(&self) -> Result<FlashState, Self::Error>;
    /// Marks the inactive flash clean.
    fn clear_inactive_state(&mut self) -> Result<(), Self::Error>;
}

/// Provisional (CEC173x). Separate rule sets per boot phase; the host itself
/// requests the switch to post-boot.
pub trait PolicyPhases: SpiFilter {
    /// One boot phase.
    type Phase: Copy;

    /// The phase whose rule set is in force.
    fn phase(&self) -> Result<Self::Phase, Self::Error>;
}

/// Provisional (OpenTitan). The chip answers selected commands itself instead
/// of forwarding them.
pub trait CommandIntercept: SpiFilter {
    /// A command the chip can answer itself.
    type Command: Copy;

    /// Enables or disables the chip answering `command` itself.
    fn set_intercept(&mut self, command: Self::Command, enabled: bool) -> Result<(), Self::Error>;
}

// ========================================================== generic code ==

/// Blocks every command, then allows exactly `opcodes`.
pub fn allow_only<F: OpcodeRules>(filter: &mut F, opcodes: &[Opcode]) -> Result<(), F::Error> {
    filter.set_all(Action::Block)?;
    for &opcode in opcodes {
        filter.set_rule(opcode, Action::Pass.into())?;
    }
    Ok(())
}

/// Why a portable region operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionError<E, R, O> {
    /// The chip cannot express this address range.
    BadRegion(R),
    /// The chip has no rules for this kind of access.
    BadOp(O),
    /// The filter rejected the rule.
    Filter(E),
}

/// Shorthand for the error of a portable region function on filter `F`.
pub type RegionResult<F> = Result<
    (),
    RegionError<
        <F as ErrorType>::Error,
        <<F as RegionRules>::Region as TryFrom<Span>>::Error,
        <<F as RegionRules>::Op as TryFrom<FlashOp>>::Error,
    >,
>;

/// Blocks writes to one range.
pub fn write_protect<F: RegionRules>(filter: &mut F, span: Span) -> RegionResult<F> {
    let region = F::Region::try_from(span).map_err(RegionError::BadRegion)?;
    let write = F::Op::try_from(FlashOp::Write).map_err(RegionError::BadOp)?;
    filter
        .set_region_rule(region, write, Action::Block)
        .map_err(RegionError::Filter)
}

/// Blocks writes everywhere except `spans`.
pub fn writable_only<F: RegionRules>(filter: &mut F, spans: &[Span]) -> RegionResult<F> {
    let write = F::Op::try_from(FlashOp::Write).map_err(RegionError::BadOp)?;
    filter
        .set_default(write, Action::Block)
        .map_err(RegionError::Filter)?;
    for &span in spans {
        let region = F::Region::try_from(span).map_err(RegionError::BadRegion)?;
        filter
            .set_region_rule(region, write, Action::Pass)
            .map_err(RegionError::Filter)?;
    }
    Ok(())
}

/// Hands every pending violation to `handle`, oldest first.
pub fn drain_violations<F: ViolationReport>(
    filter: &mut F,
    mut handle: impl FnMut(F::Violation),
) -> Result<(), F::Error> {
    while let Some(violation) = filter.take_violation()? {
        handle(violation);
    }
    Ok(())
}

/// Why [`lock_down`] failed, and at which step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockDownError<E, R, O> {
    /// Restricting the command set failed.
    Opcodes(E),
    /// Write-protecting the range failed.
    Region(RegionError<E, R, O>),
    /// Freezing a target failed.
    Lock(E),
}

/// Shorthand for the error of [`lock_down`] on filter `F`.
pub type LockDownResult<F> = Result<
    (),
    LockDownError<
        <F as ErrorType>::Error,
        <<F as RegionRules>::Region as TryFrom<Span>>::Error,
        <<F as RegionRules>::Op as TryFrom<FlashOp>>::Error,
    >,
>;

/// Restricts commands to `opcodes`, blocks writes to `protected`, then
/// freezes each of `locks`.
pub fn lock_down<F>(
    filter: &mut F,
    opcodes: &[Opcode],
    protected: Span,
    locks: &[F::Target],
) -> LockDownResult<F>
where
    F: OpcodeRules + RegionRules + Lockable,
{
    allow_only(filter, opcodes).map_err(LockDownError::Opcodes)?;
    write_protect(filter, protected).map_err(LockDownError::Region)?;
    for &target in locks {
        filter.lock(target).map_err(LockDownError::Lock)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u32 = 1024;
    const REGION_SLOTS: usize = 4;

    fn span(first: u32, last: u32) -> Span {
        Span::new(FlashAddress::new(first), FlashAddress::new(last)).unwrap()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeError {
        TableFull,
        Locked,
    }

    impl Error for FakeError {
        fn kind(&self) -> ErrorKind {
            match self {
                FakeError::TableFull => ErrorKind::CapacityExceeded,
                FakeError::Locked => ErrorKind::Locked,
            }
        }
    }

    /// Regions on the fake are whole 4 KiB blocks, like a sector-granular chip.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeRegion(Span);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Misaligned;

    impl TryFrom<Span> for FakeRegion {
        type Error = Misaligned;
        fn try_from(span: Span) -> Result<Self, Misaligned> {
            let first = span.first().offset();
            let last = span.last().offset();
            if first % (4 * KIB) != 0 || last % (4 * KIB) != 4 * KIB - 1 {
                return Err(Misaligned);
            }
            Ok(FakeRegion(span))
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeLock {
        Commands,
        Regions,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeMode {
        Off,
        On,
    }

    /// A filter with a bounded allow-list, a 4-entry region table, a
    /// violation queue and two lock targets.
    struct Fake {
        mode: FakeMode,
        opcodes: [Action; 256],
        default: [Action; 2],
        regions: Vec<(FakeRegion, FlashOp, Action)>,
        violations: std::collections::VecDeque<u8>,
        locked: [bool; 2],
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                mode: FakeMode::Off,
                opcodes: [Action::Pass; 256],
                default: [Action::Pass; 2],
                regions: Vec::new(),
                violations: Default::default(),
                locked: [false; 2],
            }
        }
        fn op_index(op: FlashOp) -> usize {
            match op {
                FlashOp::Read => 0,
                FlashOp::Write => 1,
            }
        }
    }

    impl ErrorType for Fake {
        type Error = FakeError;
    }

    impl SpiFilter for Fake {
        type Mode = FakeMode;
        type Rule = Action;
        fn set_mode(&mut self, mode: FakeMode) -> Result<(), FakeError> {
            self.mode = mode;
            Ok(())
        }
        fn mode(&self) -> Result<FakeMode, FakeError> {
            Ok(self.mode)
        }
    }

    impl OpcodeRules for Fake {
        const PASS_CAPACITY: Option<usize> = Some(8);
        fn set_all(&mut self, action: Action) -> Result<(), FakeError> {
            if self.locked[0] {
                return Err(FakeError::Locked);
            }
            if action == Action::Pass {
                return Err(FakeError::TableFull);
            }
            self.opcodes = [action; 256];
            Ok(())
        }
        fn set_rule(&mut self, opcode: Opcode, rule: Action) -> Result<(), FakeError> {
            if self.locked[0] {
                return Err(FakeError::Locked);
            }
            let passing = self.opcodes.iter().filter(|&&a| a == Action::Pass).count();
            if rule == Action::Pass
                && self.opcodes[opcode.0 as usize] == Action::Block
                && passing >= 8
            {
                return Err(FakeError::TableFull);
            }
            self.opcodes[opcode.0 as usize] = rule;
            Ok(())
        }
        fn rule(&self, opcode: Opcode) -> Result<Action, FakeError> {
            Ok(self.opcodes[opcode.0 as usize])
        }
    }

    impl RegionRules for Fake {
        type Op = FlashOp;
        type Region = FakeRegion;
        const REGION_CAPACITY: Option<usize> = Some(REGION_SLOTS);
        fn set_default(&mut self, op: FlashOp, action: Action) -> Result<(), FakeError> {
            if self.locked[1] {
                return Err(FakeError::Locked);
            }
            self.default[Self::op_index(op)] = action;
            Ok(())
        }
        fn set_region_rule(
            &mut self,
            region: FakeRegion,
            op: FlashOp,
            action: Action,
        ) -> Result<(), FakeError> {
            if self.locked[1] {
                return Err(FakeError::Locked);
            }
            if self.regions.len() >= REGION_SLOTS {
                return Err(FakeError::TableFull);
            }
            self.regions.push((region, op, action));
            Ok(())
        }
    }

    impl ViolationReport for Fake {
        type Violation = u8;
        fn take_violation(&mut self) -> Result<Option<u8>, FakeError> {
            Ok(self.violations.pop_front())
        }
    }

    impl Lockable for Fake {
        type Target = FakeLock;
        fn lock(&mut self, target: FakeLock) -> Result<(), FakeError> {
            self.locked[target as usize] = true;
            Ok(())
        }
        fn is_locked(&self, target: FakeLock) -> Result<bool, FakeError> {
            Ok(self.locked[target as usize])
        }
    }

    #[test]
    fn mode_round_trips() {
        let mut fake = Fake::new();
        assert_eq!(fake.mode().unwrap(), FakeMode::Off);
        fake.set_mode(FakeMode::On).unwrap();
        assert_eq!(fake.mode().unwrap(), FakeMode::On);
    }

    #[test]
    fn span_rejects_reversed_range() {
        assert!(Span::new(FlashAddress::new(10), FlashAddress::new(9)).is_none());
        let whole = span(0, u32::MAX);
        assert_eq!(whole.first().offset(), 0);
        assert_eq!(whole.last().offset(), u32::MAX);
    }

    #[test]
    fn allow_only_blocks_everything_else() {
        let mut fake = Fake::new();
        allow_only(&mut fake, &[Opcode(0x03), Opcode(0x9f)]).unwrap();
        assert_eq!(fake.rule(Opcode(0x03)).unwrap(), Action::Pass);
        assert_eq!(fake.rule(Opcode(0x9f)).unwrap(), Action::Pass);
        assert_eq!(fake.rule(Opcode(0x06)).unwrap(), Action::Block);
        let passing = fake.opcodes.iter().filter(|&&a| a == Action::Pass).count();
        assert_eq!(passing, 2);
    }

    #[test]
    fn allow_only_reports_bounded_capacity() {
        let mut fake = Fake::new();
        let too_many: Vec<Opcode> = (0..9).map(Opcode).collect();
        let err = allow_only(&mut fake, &too_many).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::CapacityExceeded);
        // set_all(Pass) is the same refusal, as the trait documents.
        assert_eq!(
            fake.set_all(Action::Pass).unwrap_err().kind(),
            ErrorKind::CapacityExceeded
        );
    }

    #[test]
    fn write_protect_adds_one_blocking_write_region() {
        let mut fake = Fake::new();
        let boot = span(0, 64 * KIB - 1);
        write_protect(&mut fake, boot).unwrap();
        assert_eq!(
            fake.regions,
            vec![(FakeRegion(boot), FlashOp::Write, Action::Block)]
        );
        assert_eq!(fake.default, [Action::Pass; 2]);
    }

    #[test]
    fn write_protect_rejects_a_region_the_chip_cannot_express() {
        let mut fake = Fake::new();
        let err = write_protect(&mut fake, span(1, 64 * KIB - 1)).unwrap_err();
        assert_eq!(err, RegionError::BadRegion(Misaligned));
        assert!(fake.regions.is_empty());
    }

    #[test]
    fn writable_only_sets_default_then_windows() {
        let mut fake = Fake::new();
        let a = span(0, 4 * KIB - 1);
        let b = span(64 * KIB, 128 * KIB - 1);
        writable_only(&mut fake, &[a, b]).unwrap();
        assert_eq!(fake.default[Fake::op_index(FlashOp::Write)], Action::Block);
        assert_eq!(fake.default[Fake::op_index(FlashOp::Read)], Action::Pass);
        assert_eq!(
            fake.regions,
            vec![
                (FakeRegion(a), FlashOp::Write, Action::Pass),
                (FakeRegion(b), FlashOp::Write, Action::Pass),
            ]
        );
    }

    #[test]
    fn writable_only_surfaces_a_full_table() {
        let mut fake = Fake::new();
        let windows: Vec<Span> = (0..REGION_SLOTS as u32 + 1)
            .map(|i| span(i * 4 * KIB, (i + 1) * 4 * KIB - 1))
            .collect();
        let err = writable_only(&mut fake, &windows).unwrap_err();
        assert_eq!(err, RegionError::Filter(FakeError::TableFull));
        assert_eq!(fake.regions.len(), REGION_SLOTS);
    }

    #[test]
    fn drain_violations_is_oldest_first_and_empties_the_queue() {
        let mut fake = Fake::new();
        fake.violations.extend([0x06, 0xd8, 0x02]);
        let mut seen = Vec::new();
        drain_violations(&mut fake, |v| seen.push(v)).unwrap();
        assert_eq!(seen, vec![0x06, 0xd8, 0x02]);
        assert_eq!(fake.take_violation().unwrap(), None);
    }

    #[test]
    fn lock_down_configures_then_freezes() {
        let mut fake = Fake::new();
        let boot = span(0, 64 * KIB - 1);
        lock_down(
            &mut fake,
            &[Opcode(0x03)],
            boot,
            &[FakeLock::Commands, FakeLock::Regions],
        )
        .unwrap();
        assert!(fake.is_locked(FakeLock::Commands).unwrap());
        assert!(fake.is_locked(FakeLock::Regions).unwrap());
        assert_eq!(fake.rule(Opcode(0x03)).unwrap(), Action::Pass);
        assert_eq!(fake.regions.len(), 1);
        assert_eq!(
            fake.set_rule(Opcode(0x06), Action::Pass)
                .unwrap_err()
                .kind(),
            ErrorKind::Locked
        );
    }

    #[test]
    fn lock_down_names_the_failing_step() {
        let mut fake = Fake::new();
        fake.lock(FakeLock::Commands).unwrap();
        let err = lock_down(&mut fake, &[Opcode(0x03)], span(0, 4 * KIB - 1), &[]).unwrap_err();
        assert_eq!(err, LockDownError::Opcodes(FakeError::Locked));

        let mut fake = Fake::new();
        let err = lock_down(&mut fake, &[Opcode(0x03)], span(1, 4 * KIB - 1), &[]).unwrap_err();
        assert_eq!(
            err,
            LockDownError::Region(RegionError::BadRegion(Misaligned))
        );
    }
}
