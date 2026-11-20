// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! HAL-backed [`FlashGuard`]: arm one SPI filter from the orchestrator's
//! rules.
//!
//! This is the only code that knows a filter has opcode slots, privilege
//! tables and lock bits. The reducer says "arm" and "tampered"; the board
//! table says which filter, which opcodes, what to lock and how to react;
//! this adapter turns the one into the other through the `hal_spi_filter`
//! traits. Design: `docs/src/design/orchestrator/orchestrator-platform.md`,
//! section "Flash guarding".

use hal_spi_filter::{
    Action, BusIsolation, Error as FilterError, ErrorKind, FlashAddress, FlashOp, Lockable, Opcode,
    OpcodeRules, RegionRules, Span, ViolationReport,
};
use orchestrator_capabilities::{FlashGuard, GuardRules, Owner, Violation};

/// Everything chip- and board-specific the adapter needs, declared by the
/// board's composition code from its device table.
///
/// The four fields the generic adapter cannot name itself: which `Mode` means
/// "filtering" on this chip, which bus `Connection` means the root of trust
/// and which the device, which lock `Target`s the board's lock policy maps
/// to, and how this chip's violation record reads as the capability's
/// [`Violation`].
pub struct GuardWiring<F>
where
    F: OpcodeRules + BusIsolation + ViolationReport + Lockable,
    F::Target: 'static,
{
    /// Opcodes the device may issue. A property of the flash part.
    pub opcodes: &'static [Opcode],
    /// The mode in which the chip enforces the programmed rules.
    pub filtering: F::Mode,
    /// The connection on which the root of trust's own controller drives
    /// the flash.
    pub rot: F::Connection,
    /// The connection on which the guarded device drives the flash.
    pub device: F::Connection,
    /// What to freeze after the rules are in force. The board maps its
    /// `FilterLockPolicy` onto this chip's targets.
    pub locks: &'static [F::Target],
    /// Whether `locks` freezes the region tables too. If so, the first arm
    /// is final: later arms must present the same rules or fail.
    pub locks_freeze_regions: bool,
    /// Reads one chip-specific violation record as a [`Violation`]; `None`
    /// drops a record the orchestrator has no use for (a decode error, a
    /// chip housekeeping flag).
    pub map_violation: fn(F::Violation) -> Option<Violation>,
}

/// Why the adapter could not arm, connect or read the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardError<E> {
    /// The filter reported an error; `kind()` says which.
    Filter(E),
    /// A span ends before it starts.
    BadSpan,
    /// The chip cannot express this span (alignment, size or reach).
    UnsupportedRegion,
    /// The chip has no rules for this kind of access.
    UnsupportedOp,
    /// The region tables are frozen and the new rules differ from the ones
    /// they hold.
    RulesLocked,
    /// A rule or lock did not read back as written.
    ReadBackMismatch,
}

impl<E: FilterError> GuardError<E> {
    /// The filter's failure category, where the filter was the cause.
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            GuardError::Filter(e) => Some(e.kind()),
            _ => None,
        }
    }
}

impl<E: FilterError> core::fmt::Display for GuardError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GuardError::Filter(e) => write!(f, "flash guard: filter error: {:?}", e.kind()),
            GuardError::BadSpan => f.write_str("flash guard: span ends before it starts"),
            GuardError::UnsupportedRegion => {
                f.write_str("flash guard: the filter cannot express this span")
            }
            GuardError::UnsupportedOp => {
                f.write_str("flash guard: the filter has no rules for this access")
            }
            GuardError::RulesLocked => {
                f.write_str("flash guard: rules differ from the locked tables")
            }
            GuardError::ReadBackMismatch => {
                f.write_str("flash guard: a rule or lock did not read back as written")
            }
        }
    }
}

impl<E: FilterError> core::error::Error for GuardError<E> {}

impl<E> From<E> for GuardError<E> {
    fn from(err: E) -> Self {
        GuardError::Filter(err)
    }
}

/// Binds one SPI filter to one managed device's flash.
pub struct HalFlashGuard<F>
where
    F: OpcodeRules + RegionRules + BusIsolation + ViolationReport + Lockable,
    F::Target: 'static,
{
    filter: F,
    wiring: GuardWiring<F>,
    /// Fingerprint of the rules in force, once armed. With
    /// `locks_freeze_regions` it is what a later arm is compared against.
    armed: Option<u64>,
}

impl<F> HalFlashGuard<F>
where
    F: OpcodeRules + RegionRules + BusIsolation + ViolationReport + Lockable,
    F::Target: 'static,
    F::Connection: PartialEq,
    F::Mode: PartialEq,
    F::Rule: PartialEq,
{
    pub fn new(filter: F, wiring: GuardWiring<F>) -> Self {
        Self {
            filter,
            wiring,
            armed: None,
        }
    }

    pub fn filter(&self) -> &F {
        &self.filter
    }

    /// Whether `arm` has succeeded at least once since construction.
    pub fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    fn region(span: (u32, u32)) -> Result<F::Region, GuardError<F::Error>> {
        let (first, last) = span;
        let span = Span::new(FlashAddress::new(first), FlashAddress::new(last))
            .ok_or(GuardError::BadSpan)?;
        F::Region::try_from(span).map_err(|_| GuardError::UnsupportedRegion)
    }

    fn op(op: FlashOp) -> Result<F::Op, GuardError<F::Error>> {
        F::Op::try_from(op).map_err(|_| GuardError::UnsupportedOp)
    }

    /// Programs one kind of access: `default` for addresses no span
    /// covers, the opposite for each span.
    fn program_regions(
        &mut self,
        op: FlashOp,
        default: Action,
        spans: &[(u32, u32)],
    ) -> Result<(), GuardError<F::Error>> {
        let op = Self::op(op)?;
        let inside = match default {
            Action::Pass => Action::Block,
            Action::Block => Action::Pass,
        };
        self.filter.set_default(op, default)?;
        for &span in spans {
            let region = Self::region(span)?;
            self.filter.set_region_rule(region, op, inside)?;
        }
        Ok(())
    }

    /// Read back what the HAL lets us read: every opcode in the allow list
    /// passes, and every lock target is locked.
    fn read_back(&self) -> Result<(), GuardError<F::Error>> {
        for &opcode in self.wiring.opcodes {
            if self.filter.rule(opcode)? != F::Rule::from(Action::Pass) {
                return Err(GuardError::ReadBackMismatch);
            }
        }
        for &target in self.wiring.locks {
            if !self.filter.is_locked(target)? {
                return Err(GuardError::ReadBackMismatch);
            }
        }
        Ok(())
    }
}

/// FNV-1a over the rule spans, so a re-arm against frozen tables can be
/// judged without storing a copy of the rules. Not a security boundary:
/// the hardware lock is; this only decides whether to report `RulesLocked`
/// or `Ok` for a re-arm.
fn fingerprint(rules: &GuardRules<'_>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut feed = |word: u32| {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for &(first, last) in rules.writable {
        feed(first);
        feed(last);
    }
    feed(u32::MAX); // separator: `writable` and `unreadable` must not alias
    for &(first, last) in rules.unreadable {
        feed(first);
        feed(last);
    }
    hash
}

impl<F> FlashGuard for HalFlashGuard<F>
where
    F: OpcodeRules + RegionRules + BusIsolation + ViolationReport + Lockable,
    F::Target: 'static,
    F::Connection: PartialEq,
    F::Mode: PartialEq,
    F::Rule: PartialEq,
{
    type Error = GuardError<F::Error>;

    /// In order: take the bus if the device holds it, allow exactly the
    /// board's opcodes (first arm only; the list is fixed and may be locked
    /// afterwards), block writes outside `writable`, block reads inside
    /// `unreadable`, switch the chip to filtering, apply the lock plan, then
    /// read back. With frozen region tables a re-arm compares fingerprints
    /// instead of rewriting.
    fn arm(&mut self, rules: &GuardRules<'_>) -> Result<(), Self::Error> {
        if self.filter.connection()? != self.wiring.rot {
            self.filter.connect(self.wiring.rot)?;
        }
        let wanted = fingerprint(rules);
        if let Some(current) = self.armed
            && self.wiring.locks_freeze_regions
        {
            return if current == wanted {
                Ok(())
            } else {
                Err(GuardError::RulesLocked)
            };
        }
        if self.armed.is_none() {
            hal_spi_filter::allow_only(&mut self.filter, self.wiring.opcodes)?;
        }
        self.program_regions(FlashOp::Write, Action::Block, rules.writable)?;
        self.program_regions(FlashOp::Read, Action::Pass, rules.unreadable)?;
        // Mode and locks are skipped when already in place: on a re-arm the
        // control register may itself be locked, and a locked bit stays set.
        if self.filter.mode()? != self.wiring.filtering {
            self.filter.set_mode(self.wiring.filtering)?;
        }
        for &target in self.wiring.locks {
            if !self.filter.is_locked(target)? {
                self.filter.lock(target)?;
            }
        }
        self.read_back()?;
        self.armed = Some(wanted);
        Ok(())
    }

    fn connect(&mut self, owner: Owner) -> Result<(), Self::Error> {
        let connection = match owner {
            Owner::Rot => self.wiring.rot,
            Owner::Device => self.wiring.device,
        };
        self.filter.connect(connection)?;
        Ok(())
    }

    /// Drains chip records the board maps to `None` so a housekeeping entry
    /// never hides a violation queued behind it.
    fn take_violation(&mut self) -> Result<Option<Violation>, Self::Error> {
        while let Some(record) = self.filter.take_violation()? {
            if let Some(violation) = (self.wiring.map_violation)(record) {
                return Ok(Some(violation));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use hal_spi_filter::{ErrorType, SpiFilter};
    use std::vec::Vec;

    const KIB: u32 = 1024;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeError {
        TableFull,
        Locked,
        Bus,
    }

    impl FilterError for FakeError {
        fn kind(&self) -> ErrorKind {
            match self {
                FakeError::TableFull => ErrorKind::CapacityExceeded,
                FakeError::Locked => ErrorKind::Locked,
                FakeError::Bus => ErrorKind::Hardware,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Passthrough,
        Filtering,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Mux {
        Rot,
        Host,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Lock {
        Commands,
        Control,
        Regions,
    }

    /// 4 KiB-granular regions, like a sector-granular chip.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Region(u32, u32);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Misaligned;

    impl TryFrom<Span> for Region {
        type Error = Misaligned;
        fn try_from(span: Span) -> Result<Self, Misaligned> {
            let (first, last) = (span.first().offset(), span.last().offset());
            if first % (4 * KIB) != 0 || last % (4 * KIB) != 4 * KIB - 1 {
                return Err(Misaligned);
            }
            Ok(Region(first, last))
        }
    }

    /// The chip's own log record: a decoded entry or a word it could not
    /// decode.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Record {
        Write(u32),
        Read(u32),
        Command(u8),
        Garbage,
    }

    fn map_record(record: Record) -> Option<Violation> {
        match record {
            Record::Write(a) => Some(Violation::BlockedWrite(a)),
            Record::Read(a) => Some(Violation::BlockedRead(a)),
            Record::Command(op) => Some(Violation::BlockedOpcode(op)),
            Record::Garbage => None,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Connect(Mux),
        SetAll(Action),
        SetRule(u8, Action),
        SetDefault(FlashOp, Action),
        SetRegion(Region, FlashOp, Action),
        SetMode(Mode),
        Lock(Lock),
    }

    struct Fake {
        calls: Vec<Call>,
        mux: Mux,
        mode: Mode,
        opcodes: [Action; 256],
        regions: Vec<(Region, FlashOp, Action)>,
        locked: [bool; 3],
        log: std::collections::VecDeque<Record>,
        fail_bus: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                mux: Mux::Host,
                mode: Mode::Passthrough,
                opcodes: [Action::Pass; 256],
                regions: Vec::new(),
                locked: [false; 3],
                log: Default::default(),
                fail_bus: false,
            }
        }
    }

    impl ErrorType for Fake {
        type Error = FakeError;
    }

    impl SpiFilter for Fake {
        type Mode = Mode;
        type Rule = Action;
        fn set_mode(&mut self, mode: Mode) -> Result<(), FakeError> {
            if self.locked[Lock::Control as usize] {
                return Err(FakeError::Locked);
            }
            self.calls.push(Call::SetMode(mode));
            self.mode = mode;
            Ok(())
        }
        fn mode(&self) -> Result<Mode, FakeError> {
            Ok(self.mode)
        }
    }

    impl OpcodeRules for Fake {
        const PASS_CAPACITY: Option<usize> = Some(32);
        fn set_all(&mut self, action: Action) -> Result<(), FakeError> {
            if self.locked[Lock::Commands as usize] {
                return Err(FakeError::Locked);
            }
            if action == Action::Pass {
                return Err(FakeError::TableFull);
            }
            self.calls.push(Call::SetAll(action));
            self.opcodes = [action; 256];
            Ok(())
        }
        fn set_rule(&mut self, opcode: Opcode, rule: Action) -> Result<(), FakeError> {
            if self.locked[Lock::Commands as usize] {
                return Err(FakeError::Locked);
            }
            self.calls.push(Call::SetRule(opcode.0, rule));
            self.opcodes[opcode.0 as usize] = rule;
            Ok(())
        }
        fn rule(&self, opcode: Opcode) -> Result<Action, FakeError> {
            Ok(self.opcodes[opcode.0 as usize])
        }
    }

    impl RegionRules for Fake {
        type Op = FlashOp;
        type Region = Region;
        const REGION_CAPACITY: Option<usize> = Some(8);
        fn set_default(&mut self, op: FlashOp, action: Action) -> Result<(), FakeError> {
            if self.locked[Lock::Regions as usize] {
                return Err(FakeError::Locked);
            }
            self.calls.push(Call::SetDefault(op, action));
            self.regions.retain(|(_, o, _)| *o != op);
            Ok(())
        }
        fn set_region_rule(
            &mut self,
            region: Region,
            op: FlashOp,
            action: Action,
        ) -> Result<(), FakeError> {
            if self.locked[Lock::Regions as usize] {
                return Err(FakeError::Locked);
            }
            if self.regions.len() >= 8 {
                return Err(FakeError::TableFull);
            }
            self.calls.push(Call::SetRegion(region, op, action));
            self.regions.push((region, op, action));
            Ok(())
        }
    }

    impl BusIsolation for Fake {
        type Connection = Mux;
        fn connect(&mut self, connection: Mux) -> Result<(), FakeError> {
            if self.fail_bus {
                return Err(FakeError::Bus);
            }
            self.calls.push(Call::Connect(connection));
            self.mux = connection;
            Ok(())
        }
        fn connection(&self) -> Result<Mux, FakeError> {
            Ok(self.mux)
        }
    }

    impl ViolationReport for Fake {
        type Violation = Record;
        fn take_violation(&mut self) -> Result<Option<Record>, FakeError> {
            if self.fail_bus {
                return Err(FakeError::Bus);
            }
            Ok(self.log.pop_front())
        }
    }

    impl Lockable for Fake {
        type Target = Lock;
        fn lock(&mut self, target: Lock) -> Result<(), FakeError> {
            self.calls.push(Call::Lock(target));
            self.locked[target as usize] = true;
            Ok(())
        }
        fn is_locked(&self, target: Lock) -> Result<bool, FakeError> {
            Ok(self.locked[target as usize])
        }
    }

    // Everything below is what a board's composition code declares from its
    // device table.
    const OPCODES: &[Opcode] = &[Opcode(0x03), Opcode(0x9f)];
    const COMMANDS_ONLY: &[Lock] = &[Lock::Commands, Lock::Control];
    const ALL: &[Lock] = &[Lock::Commands, Lock::Control, Lock::Regions];

    fn wiring(locks: &'static [Lock], locks_freeze_regions: bool) -> GuardWiring<Fake> {
        GuardWiring {
            opcodes: OPCODES,
            filtering: Mode::Filtering,
            rot: Mux::Rot,
            device: Mux::Host,
            locks,
            locks_freeze_regions,
            map_violation: map_record,
        }
    }

    const LOG: &[(u32, u32)] = &[(64 * KIB, 128 * KIB - 1)];
    const SECRETS: &[(u32, u32)] = &[(0, 4 * KIB - 1)];

    fn rules() -> GuardRules<'static> {
        GuardRules {
            writable: LOG,
            unreadable: SECRETS,
        }
    }

    #[test]
    fn arm_programs_the_filter_in_order() {
        let mut guard = HalFlashGuard::new(Fake::new(), wiring(COMMANDS_ONLY, false));

        guard.arm(&rules()).expect("arm failed");

        assert_eq!(
            guard.filter().calls,
            std::vec![
                Call::Connect(Mux::Rot),
                Call::SetAll(Action::Block),
                Call::SetRule(0x03, Action::Pass),
                Call::SetRule(0x9f, Action::Pass),
                Call::SetDefault(FlashOp::Write, Action::Block),
                Call::SetRegion(
                    Region(64 * KIB, 128 * KIB - 1),
                    FlashOp::Write,
                    Action::Pass
                ),
                Call::SetDefault(FlashOp::Read, Action::Pass),
                Call::SetRegion(Region(0, 4 * KIB - 1), FlashOp::Read, Action::Block),
                Call::SetMode(Mode::Filtering),
                Call::Lock(Lock::Commands),
                Call::Lock(Lock::Control),
            ]
        );
        assert!(guard.is_armed());
    }

    #[test]
    fn rearm_rewrites_regions_but_not_the_locked_allow_list() {
        let mut guard = HalFlashGuard::new(Fake::new(), wiring(COMMANDS_ONLY, false));
        guard.arm(&rules()).unwrap();
        guard.filter.calls.clear();

        let new_layout = GuardRules {
            writable: &[(128 * KIB, 256 * KIB - 1)],
            unreadable: &[],
        };
        guard
            .arm(&new_layout)
            .expect("re-arm with a new layout failed");

        // The allow list is locked and was not touched; the regions were.
        assert!(!guard
            .filter()
            .calls
            .iter()
            .any(|c| matches!(c, Call::SetAll(_) | Call::SetRule(..))));
        assert_eq!(
            guard.filter().regions,
            std::vec![(
                Region(128 * KIB, 256 * KIB - 1),
                FlashOp::Write,
                Action::Pass
            )]
        );
    }

    #[test]
    fn frozen_regions_accept_the_same_rules_and_refuse_others() {
        let mut guard = HalFlashGuard::new(Fake::new(), wiring(ALL, true));
        guard.arm(&rules()).unwrap();
        guard.filter.calls.clear();

        guard
            .arm(&rules())
            .expect("same rules against frozen tables");
        assert!(guard.filter().calls.is_empty(), "nothing rewritten");

        let other = GuardRules {
            writable: &[],
            unreadable: SECRETS,
        };
        assert_eq!(guard.arm(&other), Err(GuardError::RulesLocked));
    }

    #[test]
    fn a_span_the_chip_cannot_express_is_refused_before_filtering_starts() {
        let mut guard = HalFlashGuard::new(Fake::new(), wiring(COMMANDS_ONLY, false));
        let misaligned = GuardRules {
            writable: &[(1, 4 * KIB - 1)],
            unreadable: &[],
        };

        assert_eq!(guard.arm(&misaligned), Err(GuardError::UnsupportedRegion));
        assert!(!guard.is_armed());
        assert_eq!(guard.filter().mode, Mode::Passthrough);

        let reversed = GuardRules {
            writable: &[(4 * KIB, 0)],
            unreadable: &[],
        };
        assert_eq!(guard.arm(&reversed), Err(GuardError::BadSpan));
    }

    #[test]
    fn filter_errors_keep_their_kind() {
        let mut fake = Fake::new();
        fake.fail_bus = true;
        let mut guard = HalFlashGuard::new(fake, wiring(COMMANDS_ONLY, false));

        let err = guard.arm(&rules()).expect_err("bus fault");
        assert_eq!(err.kind(), Some(ErrorKind::Hardware));
        assert_eq!(err.to_string(), "flash guard: filter error: Hardware");
    }

    #[test]
    fn connect_maps_the_owner_onto_the_board_wiring() {
        let mut guard = HalFlashGuard::new(Fake::new(), wiring(COMMANDS_ONLY, false));

        guard.connect(Owner::Rot).unwrap();
        guard.connect(Owner::Device).unwrap();

        assert_eq!(
            guard.filter().calls,
            std::vec![Call::Connect(Mux::Rot), Call::Connect(Mux::Host)]
        );
    }

    #[test]
    fn violations_are_mapped_and_housekeeping_records_skipped() {
        let mut fake = Fake::new();
        fake.log.extend([
            Record::Garbage,
            Record::Write(64 * KIB),
            Record::Garbage,
            Record::Command(0xc7),
            Record::Read(0),
        ]);
        let mut guard = HalFlashGuard::new(fake, wiring(COMMANDS_ONLY, false));

        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedWrite(64 * KIB))
        );
        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedOpcode(0xc7))
        );
        assert_eq!(
            guard.take_violation().unwrap(),
            Some(Violation::BlockedRead(0))
        );
        assert_eq!(guard.take_violation().unwrap(), None);
    }

    /// Compile-time fence: the adapter error satisfies `core::error::Error`.
    fn _assert_error_contract<E: core::error::Error>() {}

    #[test]
    fn guard_error_satisfies_the_contract() {
        _assert_error_contract::<GuardError<FakeError>>();
    }
}
