// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

extern crate std;

use crate::board::{RegionSource, ViolationPolicy};
use crate::*;
use openprot_orchestrator_sm::{
    BootFailureKind, ComponentAttrs, ComponentId, ComponentKind, Effect, Event, Orchestrator,
    Platform, PowerOnResult, State,
};
use orchestrator_capabilities::{
    BootWatch, FailureCause, FlashGuard, GuardRules, Owner, Svn, SvnFloor, Violation, WalkVerdict,
};

const C0: ComponentId = ComponentId::new(0);

/// One actuation on the shared trace, so a test can assert the order in
/// which the reset line and the flash bus were touched across two mocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Hold,
    Release,
    Arm,
    Connect(Owner),
}

type Trace = std::rc::Rc<core::cell::RefCell<std::vec::Vec<Step>>>;

fn trace() -> Trace {
    std::rc::Rc::new(core::cell::RefCell::new(std::vec::Vec::new()))
}

fn record(trace: &Option<Trace>, step: Step) {
    if let Some(t) = trace {
        t.borrow_mut().push(step);
    }
}

// Test image convention, shared with the fwmanager itests: 4 magic bytes,
// payload, final byte makes the XOR over the image zero. A board-side
// stand-in for signature + SVN verification.
const IMAGE_MAGIC: [u8; 4] = *b"OPRT";
const IMAGE_LEN: usize = 16;

fn valid_image() -> std::vec::Vec<u8> {
    let mut image = std::vec![0u8; IMAGE_LEN];
    image[..4].copy_from_slice(&IMAGE_MAGIC);
    image[4..IMAGE_LEN - 1].fill(0xAB);
    image[IMAGE_LEN - 1] = image[..IMAGE_LEN - 1].iter().fold(0, |acc, b| acc ^ b);
    image
}

#[derive(Debug)]
struct MemFault;

impl core::fmt::Display for MemFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("mem image fault")
    }
}

impl core::error::Error for MemFault {}

/// RAM-backed image source — the seam satisfied without a HAL. A re-flash
/// can be queued: it lands on the first re-open (a re-stage), the way real
/// flash changes underneath a source between stagings — never on the
/// initial staging.
struct MemImage {
    data: std::vec::Vec<u8>,
    reflash: Option<std::vec::Vec<u8>>,
    opened: bool,
    fail_open: bool,
    fail_read: bool,
}

impl MemImage {
    fn holding(data: std::vec::Vec<u8>) -> Self {
        Self {
            data,
            reflash: None,
            opened: false,
            fail_open: false,
            fail_read: false,
        }
    }

    /// Queue a re-flash: the first re-open stages `data` instead.
    fn reflash_on_reopen(mut self, data: std::vec::Vec<u8>) -> Self {
        self.reflash = Some(data);
        self
    }
}

impl ImageSource for MemImage {
    type Error = MemFault;

    fn open(&mut self) -> Result<(), MemFault> {
        if self.fail_open {
            return Err(MemFault);
        }
        if let Some(data) = self.reflash.take_if(|_| self.opened) {
            self.data = data;
        }
        self.opened = true;
        Ok(())
    }

    fn size(&self) -> Result<usize, MemFault> {
        Ok(self.data.len())
    }

    fn read_at(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), MemFault> {
        if self.fail_read {
            return Err(MemFault);
        }
        buf.copy_from_slice(&self.data[offset..offset + buf.len()]);
        Ok(())
    }
}

#[derive(Debug)]
struct VerifierError;

impl core::fmt::Display for VerifierError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("verifier broken")
    }
}

impl core::error::Error for VerifierError {}

/// The magic + XOR-zero check as a board-supplied verifier, reading in
/// chunks.
struct XorVerifier {
    fault: bool,
    /// The manifest SVN this verifier reports for an authenticated image.
    /// A real verifier reads it from the signed manifest of the image it
    /// just checked; the test image format has no manifest, so tests pin it.
    svn: u32,
    /// The writable spans this verifier claims to have read from the
    /// manifest, for `RegionSource::Manifest` tests. `None`: no
    /// authenticated image on record.
    manifest_writable: Option<std::vec::Vec<(u32, u32)>>,
}

impl Verifier for XorVerifier {
    type Error = VerifierError;

    fn verify(
        &mut self,
        _id: ComponentId,
        image: &mut impl ImageSource,
    ) -> Result<Verdict, VerifierError> {
        if self.fault {
            return Err(VerifierError);
        }
        let len = image.size().map_err(|_| VerifierError)?;
        let mut magic = [0u8; 4];
        let mut xor = 0u8;
        let mut offset = 0;
        let mut chunk = [0u8; 4];
        while offset < len {
            let take = chunk.len().min(len - offset);
            image
                .read_at(offset, &mut chunk[..take])
                .map_err(|_| VerifierError)?;
            if offset == 0 && take >= 4 {
                magic.copy_from_slice(&chunk[..4]);
            }
            xor = chunk[..take].iter().fold(xor, |acc, b| acc ^ b);
            offset += take;
        }
        let ok = len > IMAGE_MAGIC.len() && magic == IMAGE_MAGIC && xor == 0;
        Ok(if ok {
            Verdict::Authenticated { svn: Svn(self.svn) }
        } else {
            Verdict::Rejected
        })
    }

    fn guard_rules(&self, _id: ComponentId) -> Option<GuardRules<'_>> {
        self.manifest_writable
            .as_deref()
            .map(|writable| GuardRules {
                writable,
                unreadable: &[],
            })
    }
}

#[derive(Debug)]
struct ResetFault;

impl core::fmt::Display for ResetFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("reset line fault")
    }
}

impl core::error::Error for ResetFault {}

/// Reset actuation without a HAL; `held` is shared so tests can observe the
/// line after the control moves into the driver.
struct MockReset {
    held: std::rc::Rc<core::cell::Cell<bool>>,
    fail: bool,
    trace: Option<Trace>,
}

impl MockReset {
    fn new() -> Self {
        Self {
            held: std::rc::Rc::new(core::cell::Cell::new(true)),
            fail: false,
            trace: None,
        }
    }

    fn traced(trace: &Trace) -> Self {
        Self {
            trace: Some(trace.clone()),
            ..Self::new()
        }
    }
}

impl orchestrator_capabilities::BootControl for MockReset {
    type Error = ResetFault;

    fn hold_in_reset(&mut self) -> Result<(), ResetFault> {
        if self.fail {
            return Err(ResetFault);
        }
        self.held.set(true);
        record(&self.trace, Step::Hold);
        Ok(())
    }

    fn release(&mut self) -> Result<(), ResetFault> {
        if self.fail {
            return Err(ResetFault);
        }
        self.held.set(false);
        record(&self.trace, Step::Release);
        Ok(())
    }
}

#[derive(Debug)]
struct GuardFault;

impl core::fmt::Display for GuardFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("guard fault")
    }
}

impl core::error::Error for GuardFault {}

/// A flash guard without a filter: records what it is asked, serves a
/// scripted violation log. Tests read it back through
/// `PlatformDriver::board`.
/// The spans one `arm` call carried: `(writable, unreadable)`.
type ArmedRules = (std::vec::Vec<(u32, u32)>, std::vec::Vec<(u32, u32)>);

struct MockGuard {
    armed: std::vec::Vec<ArmedRules>,
    owner: Owner,
    violations: std::collections::VecDeque<Violation>,
    fail_arm: bool,
    fail: bool,
    trace: Option<Trace>,
}

impl MockGuard {
    fn new() -> Self {
        Self {
            armed: std::vec::Vec::new(),
            owner: Owner::Rot,
            violations: Default::default(),
            fail_arm: false,
            fail: false,
            trace: None,
        }
    }

    fn traced(trace: &Trace) -> Self {
        Self {
            trace: Some(trace.clone()),
            ..Self::new()
        }
    }

    fn logging(violations: &[Violation]) -> Self {
        Self {
            violations: violations.iter().copied().collect(),
            ..Self::new()
        }
    }
}

impl FlashGuard for MockGuard {
    type Error = GuardFault;

    fn arm(&mut self, rules: &GuardRules<'_>) -> Result<(), GuardFault> {
        if self.fail || self.fail_arm {
            return Err(GuardFault);
        }
        self.armed
            .push((rules.writable.to_vec(), rules.unreadable.to_vec()));
        record(&self.trace, Step::Arm);
        Ok(())
    }

    fn connect(&mut self, owner: Owner) -> Result<(), GuardFault> {
        if self.fail {
            return Err(GuardFault);
        }
        self.owner = owner;
        record(&self.trace, Step::Connect(owner));
        Ok(())
    }

    fn take_violation(&mut self) -> Result<Option<Violation>, GuardFault> {
        if self.fail {
            return Err(GuardFault);
        }
        Ok(self.violations.pop_front())
    }
}

/// The guard a test bound to `ComponentId(i)`.
fn guard_of<B: BoardCapabilities<FlashGuard = MockGuard>, const N: usize>(
    driver: &PlatformDriver<B, N>,
    i: usize,
) -> &MockGuard {
    match &driver.board().flash_guards[i] {
        GuardBinding::Guarded { guard, .. } => guard,
        GuardBinding::Unguarded => panic!("component {i} has no guard"),
    }
}

/// Boot walk without a device; scripted verdicts. An exhausted script
/// holds its last verdict; an empty script waits forever. `arm` rewinds
/// to the script start, so a fresh attempt is observable from the
/// verdicts alone — no poll or arm counters needed.
struct MockWalk {
    verdicts: std::vec::Vec<WalkVerdict>,
    next: usize,
}

const IDLE_DEADLINE: u64 = 60_000;

impl MockWalk {
    fn scripted(verdicts: std::vec::Vec<WalkVerdict>) -> Self {
        Self { verdicts, next: 0 }
    }

    /// A walk that reports "still waiting" forever.
    fn idle() -> Self {
        Self::scripted(std::vec::Vec::new())
    }
}

impl BootWatch for MockWalk {
    fn arm(&mut self) {
        self.next = 0;
    }

    fn poll(&mut self, _now_millis: u64) -> WalkVerdict {
        match self.verdicts.get(self.next) {
            Some(v) => {
                self.next += 1;
                *v
            }
            // Exhausted: repeat the last verdict, like a real finished
            // walk. A driver bug that re-polls one then shows up as a
            // duplicate event in the exactly-once assertions instead of
            // panicking here.
            None => self
                .verdicts
                .last()
                .copied()
                .unwrap_or(WalkVerdict::Waiting {
                    deadline_millis: IDLE_DEADLINE,
                }),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FloorFaultInjected;

impl core::fmt::Display for FloorFaultInjected {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("floor fault injected")
    }
}

impl core::error::Error for FloorFaultInjected {}

/// An SVN floor without storage; tests read it back through the
/// capability's own `floor()` via `PlatformDriver::board`.
struct MockFloor {
    floor: u32,
    fail: bool,
}

impl MockFloor {
    fn new() -> Self {
        Self {
            floor: 0,
            fail: false,
        }
    }
}

impl orchestrator_capabilities::SvnFloor for MockFloor {
    type Error = FloorFaultInjected;

    fn floor(&self) -> Result<Svn, FloorFaultInjected> {
        if self.fail {
            return Err(FloorFaultInjected);
        }
        Ok(Svn(self.floor))
    }

    fn advance(&mut self, to: Svn) -> Result<(), FloorFaultInjected> {
        if self.fail {
            return Err(FloorFaultInjected);
        }
        self.floor = self.floor.max(to.0);
        Ok(())
    }
}

/// The test board's type choices.
struct MockBoard;

impl BoardCapabilities for MockBoard {
    type Image = MemImage;
    type Verifier = XorVerifier;
    type BootControl = MockReset;
    type BootWatch = MockWalk;
    type SvnFloor = MockFloor;
    type ReportSink = RecordingSink;
    type FlashGuard = MockGuard;
}

/// The SVN `mock_board`'s verifier vouches for. Tests that read the floor
/// back assert against it.
const MOCK_SVN: u32 = 5;

/// Happy-path wiring for `N` components. Tests override the one field
/// they exercise with `..mock_board()`.
fn mock_board<const N: usize>() -> Board<MockBoard, N> {
    Board {
        images: core::array::from_fn(|_| MemImage::holding(valid_image())),
        verifier: XorVerifier {
            fault: false,
            svn: MOCK_SVN,
            manifest_writable: None,
        },
        boot_controls: core::array::from_fn(|_| MockReset::new()),
        boot_watches: core::array::from_fn(|_| MockWalk::idle()),
        component_kinds: core::array::from_fn(|_| ComponentKind::Passive),
        svn_floors: core::array::from_fn(|_| SvnFloorBinding::Erot(MockFloor::new())),
        report_sink: RecordingSink::new(),
        flash_guards: core::array::from_fn(|_| GuardBinding::Unguarded),
    }
}

fn driver(images: [MemImage; 1]) -> PlatformDriver<MockBoard, 1> {
    PlatformDriver::new(Board {
        images,
        ..mock_board()
    })
}

fn orchestrator() -> Orchestrator<1, 4> {
    let mut chain = heapless::Vec::<_, 1>::new();
    chain
        .push((C0, ComponentAttrs::passive_required()))
        .unwrap();
    Orchestrator::new(chain.try_into().unwrap(), 3)
}

// PowerGood drives ReadFirmware + VerifyFirmware into the driver; the
// verdict returns through execute and the SM settles it in the same
// dispatch run, carrying a passive component all the way to Ready.
#[test]
fn boot_verifies_the_first_component() {
    let mut orch = orchestrator();
    let mut driver = driver([MemImage::holding(valid_image())]);

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Ready);
}

#[test]
fn corrupt_image_fails_verification() {
    let mut corrupt = valid_image();
    corrupt[7] ^= 0x01;
    let mut driver = driver([MemImage::holding(corrupt)]);

    driver.stage_firmware(C0).unwrap();

    assert_eq!(
        driver.verify_firmware(C0),
        Ok(Event::VerificationFailed(C0))
    );
}

#[test]
fn verify_without_read_is_refused() {
    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(driver.verify_firmware(C0), Err(DriverError::NotStaged));
}

// An unopenable source is a failed actuation, not a verdict: the SM
// latches Locked instead of getting a forged VerificationFailed.
#[test]
fn unopenable_source_fails_closed() {
    let mut orch = orchestrator();
    let mut image = MemImage::holding(valid_image());
    image.fail_open = true;
    let mut driver = driver([image]);

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Locked);
}

// A source that opens but cannot be read fails the same way, via the
// verifier's error.
#[test]
fn unreadable_source_fails_closed() {
    let mut orch = orchestrator();
    let mut image = MemImage::holding(valid_image());
    image.fail_read = true;
    let mut driver = driver([image]);

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Locked);
}

// So does a verifier that cannot run its check.
#[test]
fn verifier_fault_fails_closed() {
    let mut orch = orchestrator();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        verifier: XorVerifier {
            fault: true,
            svn: MOCK_SVN,
            manifest_writable: None,
        },
        ..mock_board()
    });

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Locked);
}

const C1: ComponentId = ComponentId::new(1);

#[test]
fn verify_for_a_different_component_is_refused() {
    let mut driver = PlatformDriver::<MockBoard, 2>::new(mock_board());

    driver.stage_firmware(C0).unwrap();

    assert_eq!(driver.verify_firmware(C1), Err(DriverError::NotStaged));
}

#[test]
fn unknown_component_is_refused() {
    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(
        driver.stage_firmware(ComponentId::new(9)),
        Err(DriverError::UnknownComponent)
    );
}

// An unknown id is reported as such even though it is also never staged.
#[test]
fn verify_of_unknown_component_is_refused() {
    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(
        driver.verify_firmware(ComponentId::new(9)),
        Err(DriverError::UnknownComponent)
    );
}

#[test]
fn reset_release_and_assert_reach_the_boot_control() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(mock_board());

    driver.release_reset(C0).unwrap();
    assert!(!driver.board().boot_controls[0].held.get());

    driver.assert_reset(C0).unwrap();
    assert!(driver.board().boot_controls[0].held.get());
}

#[test]
fn reset_of_unknown_component_is_refused() {
    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(
        driver.release_reset(ComponentId::new(9)),
        Err(DriverError::UnknownComponent)
    );
    assert_eq!(
        driver.assert_reset(ComponentId::new(9)),
        Err(DriverError::UnknownComponent)
    );
}

#[test]
fn reset_line_fault_is_reported() {
    let mut control = MockReset::new();
    control.fail = true;
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [control],
        ..mock_board()
    });

    assert_eq!(driver.release_reset(C0), Err(DriverError::BootControlFault));
    assert_eq!(driver.assert_reset(C0), Err(DriverError::BootControlFault));
}

// Effect::Emit is the orchestrator's internal channel and must never reach
// a Platform; the driver refuses it rather than acting on it.
#[test]
fn emit_is_refused() {
    use openprot_orchestrator_sm::{Effect, EffectError, Platform};

    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(
        driver.execute(Effect::Emit(Event::UpdateRequest)),
        Err(EffectError)
    );
}

// The verdict is the returned event of the VerifyFirmware effect.
#[test]
fn execute_returns_the_verdict_event() {
    use openprot_orchestrator_sm::{Effect, Platform};

    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(driver.execute(Effect::ReadFirmware(C0)), Ok(None));
    assert_eq!(
        driver.execute(Effect::VerifyFirmware(C0)),
        Ok(Some(Event::VerificationPassed(C0)))
    );
}

/// Wraps [`XorVerifier`] and snapshots the reset line as the check runs,
/// so the test can see the line state inside the verification window.
struct LineWatchingVerifier {
    inner: XorVerifier,
    line: std::rc::Rc<core::cell::Cell<bool>>,
    held_during_verify: std::rc::Rc<core::cell::Cell<bool>>,
}

impl Verifier for LineWatchingVerifier {
    type Error = VerifierError;

    fn verify(
        &mut self,
        id: ComponentId,
        image: &mut impl ImageSource,
    ) -> Result<Verdict, VerifierError> {
        self.held_during_verify.set(self.line.get());
        self.inner.verify(id, image)
    }
}

struct WatchBoard;

impl BoardCapabilities for WatchBoard {
    type Image = MemImage;
    type Verifier = LineWatchingVerifier;
    type BootControl = MockReset;
    type BootWatch = MockWalk;
    type FlashGuard = MockGuard;
    type SvnFloor = MockFloor;
    // A board with nothing to tell: exercises the no-op sink.
    type ReportSink = ();
}

// The at-rest guarantee end to end: the component is still held while its
// image is verified, and the line is released only on the passing verdict.
#[test]
fn release_follows_verification() {
    let control = MockReset::new();
    let held = control.held.clone();
    let held_during_verify = std::rc::Rc::new(core::cell::Cell::new(false));
    let mut driver = PlatformDriver::<WatchBoard, 1>::new(Board {
        images: [MemImage::holding(valid_image())],
        verifier: LineWatchingVerifier {
            inner: XorVerifier {
                fault: false,
                svn: MOCK_SVN,
                manifest_writable: None,
            },
            line: held.clone(),
            held_during_verify: held_during_verify.clone(),
        },
        boot_controls: [control],
        boot_watches: [MockWalk::idle()],
        component_kinds: [ComponentKind::Passive],
        svn_floors: [SvnFloorBinding::Erot(MockFloor::new())],
        report_sink: (),
        flash_guards: core::array::from_fn(|_| GuardBinding::Unguarded),
    });
    let mut orch = orchestrator();

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Ready);
    assert!(
        held_during_verify.get(),
        "held while its image was verified"
    );
    assert!(!held.get(), "released after the verdict");
}

// A dead reset line is a failed actuation, not a verdict: the SM fails
// closed and the component stays quiesced.
#[test]
fn failed_release_fails_closed() {
    let mut control = MockReset::new();
    control.fail = true;
    let held = control.held.clone();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [control],
        ..mock_board()
    });
    let mut orch = orchestrator();

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Locked);
    assert!(held.get(), "never left reset");
}

// ---------------------------------------------------------------------------
// Boot-walk supervision.
// ---------------------------------------------------------------------------

/// A 2-component driver with per-component scripted walks and kinds;
/// everything else is the happy-path mock.
fn walk_driver(
    walks: [MockWalk; 2],
    component_kinds: [ComponentKind; 2],
) -> PlatformDriver<MockBoard, 2> {
    PlatformDriver::new(Board {
        boot_watches: walks,
        component_kinds,
        ..mock_board()
    })
}

// A completed walk becomes ComponentReady for Active, Booted for Passive.
// One event per call, drained in index order; a finished walk never
// reports twice.
#[test]
fn completed_walks_report_by_kind() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![WalkVerdict::Complete]),
            MockWalk::scripted(std::vec![WalkVerdict::Complete]),
        ],
        [ComponentKind::Active, ComponentKind::Passive],
    );
    driver.release_reset(C0).unwrap();
    driver.release_reset(C1).unwrap();

    assert_eq!(
        driver.poll_boot_walks(0).event,
        Some(Event::ComponentReady(C0))
    );
    assert_eq!(driver.poll_boot_walks(0).event, Some(Event::Booted(C1)));

    let quiet = driver.poll_boot_walks(0);
    assert_eq!(quiet.event, None, "verdicts are delivered exactly once");
    assert_eq!(quiet.next_deadline_millis, None, "no walk left waiting");
}

// A failed walk becomes BootFailed, preserving the checkpoint name and
// classified cause so the SM can differentiate retry decisions later.
#[test]
fn failed_walks_map_to_boot_failed() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![WalkVerdict::Failed {
                checkpoint: "heartbeat",
                cause: FailureCause::TimedOut,
            }]),
            MockWalk::scripted(std::vec![WalkVerdict::Failed {
                checkpoint: "self-test",
                cause: FailureCause::DeviceFatal,
            }]),
        ],
        [ComponentKind::Active, ComponentKind::Passive],
    );
    driver.release_reset(C0).unwrap();
    driver.release_reset(C1).unwrap();

    assert_eq!(
        driver.poll_boot_walks(0).event,
        Some(Event::BootFailed {
            id: C0,
            checkpoint: "heartbeat",
            kind: BootFailureKind::TimedOut,
        })
    );
    assert_eq!(
        driver.poll_boot_walks(0).event,
        Some(Event::BootFailed {
            id: C1,
            checkpoint: "self-test",
            kind: BootFailureKind::DeviceFatal,
        })
    );
    assert_eq!(driver.poll_boot_walks(0).event, None);
}

// An event-carrying poll returns before visiting later walks, so its
// deadline is partial and must not be trusted; the drain's final,
// event-free poll visits every remaining walk and reports the earliest
// deadline.
#[test]
fn deadline_is_authoritative_only_when_no_event() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![WalkVerdict::Complete]),
            MockWalk::scripted(std::vec![WalkVerdict::Waiting {
                deadline_millis: 1_000,
            }]),
        ],
        [ComponentKind::Passive, ComponentKind::Passive],
    );
    driver.release_reset(C0).unwrap();
    driver.release_reset(C1).unwrap();

    let first = driver.poll_boot_walks(0);
    assert_eq!(first.event, Some(Event::Booted(C0)));
    assert_eq!(
        first.next_deadline_millis, None,
        "returned before the waiting walk was visited"
    );

    let last = driver.poll_boot_walks(0);
    assert_eq!(last.event, None);
    assert_eq!(last.next_deadline_millis, Some(1_000));
}

// While every watched walk waits, the poll carries the earliest deadline
// as the run loop's next wake-up.
#[test]
fn waiting_walks_report_the_earliest_deadline() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![WalkVerdict::Waiting {
                deadline_millis: 9_000,
            }]),
            MockWalk::scripted(std::vec![WalkVerdict::Waiting {
                deadline_millis: 4_000,
            }]),
        ],
        [ComponentKind::Passive, ComponentKind::Passive],
    );
    driver.release_reset(C0).unwrap();
    driver.release_reset(C1).unwrap();

    let poll = driver.poll_boot_walks(0);
    assert_eq!(poll.event, None);
    assert_eq!(poll.next_deadline_millis, Some(4_000));
}

// A walk is watched only between release and terminal verdict. The script's
// terminal verdict would surface as an event if the gate were missing: no
// event before release, no stale event after assert_reset, and the verdict
// still arrives once the device is actually released.
#[test]
fn only_released_components_are_watched() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![WalkVerdict::Complete]),
            MockWalk::idle(),
        ],
        [ComponentKind::Passive, ComponentKind::Passive],
    );

    assert_eq!(
        driver.poll_boot_walks(0).event,
        None,
        "unreleased: no event"
    );

    driver.release_reset(C0).unwrap();
    driver.assert_reset(C0).unwrap();
    assert_eq!(
        driver.poll_boot_walks(0).event,
        None,
        "back in reset: no stale event"
    );

    driver.release_reset(C0).unwrap();
    assert_eq!(driver.poll_boot_walks(0).event, Some(Event::Booted(C0)));
}

// Every release re-arms the walk: a retry judges a new attempt from the
// first checkpoint, not the failed one resumed. With a script of
// [Failed, Complete], a resumed walk would report Complete on the second
// attempt; a fresh one reports Failed again.
#[test]
fn rerelease_arms_a_fresh_walk() {
    let mut driver = walk_driver(
        [
            MockWalk::scripted(std::vec![
                WalkVerdict::Failed {
                    checkpoint: "heartbeat",
                    cause: FailureCause::TimedOut,
                },
                WalkVerdict::Complete,
            ]),
            MockWalk::idle(),
        ],
        [ComponentKind::Passive, ComponentKind::Passive],
    );

    driver.release_reset(C0).unwrap();
    assert_eq!(
        driver.poll_boot_walks(0).event,
        Some(Event::BootFailed {
            id: C0,
            checkpoint: "heartbeat",
            kind: BootFailureKind::TimedOut,
        })
    );

    driver.release_reset(C0).unwrap();
    assert_eq!(
        driver.poll_boot_walks(0).event,
        Some(Event::BootFailed {
            id: C0,
            checkpoint: "heartbeat",
            kind: BootFailureKind::TimedOut,
        }),
        "fresh attempt from the first checkpoint, not the old walk resumed"
    );
}

// End to end: a passive component is released speculatively (Ready), its
// walk completes, and the Booted event settles cleanly.
#[test]
fn booted_walk_settles_in_ready() {
    let mut orch = orchestrator();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_watches: [MockWalk::scripted(std::vec![WalkVerdict::Complete])],
        ..mock_board()
    });

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));
    assert_eq!(orch.state(), State::Ready);

    let event = driver.poll_boot_walks(0).event.expect("walk completed");
    assert_eq!(event, Event::Booted(C0));
    orch.dispatch(&mut driver, event);

    assert_eq!(orch.state(), State::Ready);
}

// End to end, failure path: the released component's walk fails, its
// BootFailed enters recovery, and with no recovery capability composed
// yet the machine fails closed.
#[test]
fn boot_failure_fails_closed_without_recovery() {
    let mut orch = orchestrator();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_watches: [MockWalk::scripted(std::vec![WalkVerdict::Failed {
            checkpoint: "heartbeat",
            cause: FailureCause::TimedOut,
        }])],
        ..mock_board()
    });

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));
    assert_eq!(orch.state(), State::Ready);

    let event = driver.poll_boot_walks(0).event.expect("walk failed");
    assert_eq!(
        event,
        Event::BootFailed {
            id: C0,
            checkpoint: "heartbeat",
            kind: BootFailureKind::TimedOut,
        }
    );
    orch.dispatch(&mut driver, event);

    assert_eq!(orch.state(), State::Locked);
}

// ---------------------------------------------------------------------------
// Report sink.
// ---------------------------------------------------------------------------

/// Records what it is handed: the seam satisfied without a management
/// transport. Tests read `seen` back through `PlatformDriver::board`.
#[derive(Default)]
struct RecordingSink {
    seen: std::vec::Vec<Report>,
}

impl RecordingSink {
    fn new() -> Self {
        Self::default()
    }
}

impl ReportSink for RecordingSink {
    fn report(&mut self, report: Report) {
        self.seen.push(report);
    }
}

/// One of each report, so a test covers the whole enum.
fn every_report() -> [Report; 6] {
    [
        Report::Isolated(C0),
        Report::RecoveryFailed(C0),
        Report::BootFailed {
            id: C0,
            checkpoint: "self-test",
            kind: BootFailureKind::DeviceFatal,
        },
        Report::UpdateDeferred,
        Report::UpdateAborted,
        Report::FlashViolation {
            id: C0,
            violation: Violation::BlockedOpcode(0xc7),
        },
    ]
}

// Every report is deliverable through the seam alone, in the order handed
// over; the unit sink is a wiring choice and satisfies the same caller.
#[test]
fn every_report_reaches_a_sink() {
    fn tell<S: ReportSink>(sink: &mut S, reports: [Report; 6]) {
        for report in reports {
            sink.report(report);
        }
    }

    let mut recording = RecordingSink::new();
    tell(&mut recording, every_report());
    assert_eq!(recording.seen, every_report());

    tell(&mut (), every_report());
}

// ---------------------------------------------------------------------------
// Anti-rollback floor commits.
// ---------------------------------------------------------------------------

// The floor may only move to the SVN the verifier vouched for, and only
// after a verification has passed — the two halves of the commit contract.
#[test]
fn commit_advances_the_floor_to_the_verified_svn() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(mock_board());

    driver
        .execute(Effect::ReadFirmware(C0))
        .expect("stage failed");
    driver
        .execute(Effect::VerifyFirmware(C0))
        .expect("verify failed");

    assert_eq!(driver.execute(Effect::CommitSvnFloor(C0)), Ok(None));
    // Read back through the capability's own seam.
    let SvnFloorBinding::Erot(floor) = &driver.board().svn_floors[0] else {
        panic!("C0 is wired with an eRoT floor");
    };
    assert_eq!(
        floor.floor(),
        Ok(Svn(MOCK_SVN)),
        "floor advanced to the verifier's SVN"
    );
}

// A component wired without an eRoT floor tracks its own SVN. The commit
// succeeds even with no verified image cached: there is no floor to
// mis-advance.
#[test]
fn commit_without_an_erot_floor_is_a_no_op() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        svn_floors: [SvnFloorBinding::SelfManaged],
        ..mock_board()
    });

    assert_eq!(driver.execute(Effect::CommitSvnFloor(C0)), Ok(None));
}

#[test]
fn commit_without_a_verified_image_fails_closed() {
    let mut driver = driver([MemImage::holding(valid_image())]);

    assert_eq!(
        driver.commit_svn_floor(C0),
        Err(DriverError::NoVerifiedImage)
    );
}

// A rejection must clear the cached SVN, or the floor could commit against
// an image that is no longer the authenticated one.
#[test]
fn rejected_image_clears_the_verified_svn() {
    let mut corrupt = valid_image();
    corrupt[7] ^= 0x01;
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        images: [MemImage::holding(valid_image()).reflash_on_reopen(corrupt)],
        ..mock_board()
    });

    driver.stage_firmware(C0).expect("stage failed");
    assert_eq!(
        driver.verify_firmware(C0),
        Ok(Event::VerificationPassed(C0))
    );

    // The queued re-flash lands on the re-stage; the rejection must take
    // the cached SVN with it.
    driver.stage_firmware(C0).expect("re-stage failed");
    assert_eq!(
        driver.verify_firmware(C0),
        Ok(Event::VerificationFailed(C0))
    );

    assert_eq!(
        driver.commit_svn_floor(C0),
        Err(DriverError::NoVerifiedImage)
    );
}

#[test]
fn floor_fault_is_reported() {
    let mut mock = MockFloor::new();
    mock.fail = true;
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        svn_floors: [SvnFloorBinding::Erot(mock)],
        ..mock_board()
    });

    driver.stage_firmware(C0).expect("stage failed");
    driver.verify_firmware(C0).expect("verify failed");

    assert_eq!(driver.commit_svn_floor(C0), Err(DriverError::SvnFloorFault));
}

// Every report effect reaches the board's sink, in emission order, and none
// hands back an error for the SM to fail closed on.
#[test]
fn reports_reach_the_board_sink() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        verifier: XorVerifier {
            fault: false,
            svn: 0,
            manifest_writable: None,
        },
        ..mock_board()
    });

    for effect in [
        Effect::ReportIsolated(C0),
        Effect::ReportRecoveryFailed(C0),
        Effect::ReportBootFailed {
            id: C0,
            checkpoint: "self-test",
            kind: BootFailureKind::DeviceFatal,
        },
        Effect::ReportUpdateDeferred,
        Effect::ReportUpdateAborted,
    ] {
        assert_eq!(driver.execute(effect), Ok(None));
    }

    // `FlashViolation` is read from the guard, not mirrored from an effect,
    // so it is the one report no `Effect::Report*` produces.
    assert_eq!(driver.board().report_sink.seen, every_report()[..5]);
}

// An Isolable component is contained and reported, and the platform keeps
// running: executing a report returns no error, so it never reaches the
// fail-closed path.
#[test]
fn reporting_an_isolated_component_does_not_lock_the_platform() {
    let mut driver = PlatformDriver::<MockBoard, 2>::new(Board {
        verifier: XorVerifier {
            fault: false,
            svn: 0,
            manifest_writable: None,
        },
        ..mock_board()
    });
    let mut chain = heapless::Vec::<_, 2>::new();
    chain
        .push((C0, ComponentAttrs::passive_required()))
        .unwrap();
    chain
        .push((C1, ComponentAttrs::passive_isolable()))
        .unwrap();
    let mut orch = Orchestrator::<2, 6>::new(chain.try_into().unwrap(), 3);

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));
    assert_eq!(orch.state(), State::Ready, "both components verified");

    orch.dispatch(&mut driver, Event::CorruptionDetected(C1));

    assert_eq!(orch.state(), State::Ready, "contained, not locked");
    assert_eq!(driver.board().report_sink.seen, [Report::Isolated(C1)]);
}

// ---------------------------------------------------------------------------
// Flash guard: arming, bus handover, violations.
// ---------------------------------------------------------------------------

const BMC_WRITABLE: &[(u32, u32)] = &[(0x0200_0000, 0x020f_ffff)];
const MANIFEST_WRITABLE: &[(u32, u32)] = &[(0x0010_0000, 0x001f_ffff), (0x0030_0000, 0x0030_ffff)];

fn guarded(
    guard: MockGuard,
    regions: RegionSource,
    violations: ViolationPolicy,
) -> GuardBinding<MockGuard> {
    GuardBinding::Guarded {
        guard,
        regions,
        violations,
    }
}

/// A 1-component driver whose device is behind `guard`.
fn guarded_driver(
    guard: MockGuard,
    regions: RegionSource,
    violations: ViolationPolicy,
) -> PlatformDriver<MockBoard, 1> {
    PlatformDriver::new(Board {
        flash_guards: [guarded(guard, regions, violations)],
        ..mock_board()
    })
}

// The effect's contract: a component with no guard arms nothing and
// succeeds, so one reducer serves boards with and without filters.
#[test]
fn arm_without_a_guard_is_a_no_op() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(mock_board());

    assert_eq!(driver.execute(Effect::ArmFlashGuard(C0)), Ok(None));
    assert_eq!(
        driver.arm_flash_guard(C1),
        Err(DriverError::UnknownComponent)
    );
}

// Table rules reach the guard verbatim: the table's spans are the writable
// ones and nothing is unreadable.
#[test]
fn arm_loads_table_rules_into_the_guard() {
    let mut driver = guarded_driver(
        MockGuard::new(),
        RegionSource::Table(BMC_WRITABLE),
        ViolationPolicy::Recover,
    );

    driver.arm_flash_guard(C0).unwrap();

    assert_eq!(
        guard_of(&driver, 0).armed,
        std::vec![(BMC_WRITABLE.to_vec(), std::vec::Vec::new())]
    );
}

// Manifest rules come from the verifier, which is the only authenticated
// reading of the manifest; without an authenticated image on record the
// arm fails closed rather than arming an empty or stale layout.
#[test]
fn arm_from_manifest_uses_the_verifier_spans() {
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        verifier: XorVerifier {
            fault: false,
            svn: MOCK_SVN,
            manifest_writable: Some(MANIFEST_WRITABLE.to_vec()),
        },
        flash_guards: [guarded(
            MockGuard::new(),
            RegionSource::Manifest,
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });

    driver.arm_flash_guard(C0).unwrap();

    assert_eq!(
        guard_of(&driver, 0).armed,
        std::vec![(MANIFEST_WRITABLE.to_vec(), std::vec::Vec::new())]
    );
}

#[test]
fn arm_from_manifest_without_an_authenticated_image_fails_closed() {
    let mut driver = guarded_driver(
        MockGuard::new(),
        RegionSource::Manifest,
        ViolationPolicy::Recover,
    );

    assert_eq!(
        driver.arm_flash_guard(C0),
        Err(DriverError::GuardRulesUnavailable)
    );
    assert!(guard_of(&driver, 0).armed.is_empty());
}

// The bus changes hands on the safe side of the reset line both ways: the
// device gets the flash before it leaves reset, and the RoT gets it back
// only once the device is held.
#[test]
fn bus_handover_brackets_the_reset_line() {
    let trace = trace();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [MockReset::traced(&trace)],
        flash_guards: [guarded(
            MockGuard::traced(&trace),
            RegionSource::Table(BMC_WRITABLE),
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });

    driver.release_reset(C0).unwrap();
    driver.assert_reset(C0).unwrap();

    assert_eq!(
        *trace.borrow(),
        std::vec![
            Step::Connect(Owner::Device),
            Step::Release,
            Step::Hold,
            Step::Connect(Owner::Rot),
        ]
    );
}

// End to end through the reducer: verify, arm, hand over, release, in
// that order, from one PowerGood.
#[test]
fn guard_is_armed_before_the_device_is_released() {
    let trace = trace();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [MockReset::traced(&trace)],
        flash_guards: [guarded(
            MockGuard::traced(&trace),
            RegionSource::Table(BMC_WRITABLE),
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });
    let mut orch = orchestrator();

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Ready);
    assert_eq!(
        *trace.borrow(),
        std::vec![Step::Arm, Step::Connect(Owner::Device), Step::Release]
    );
}

// A guard that cannot be proven armed does not get a device released
// behind it: the SM fails closed and the device never leaves reset.
#[test]
fn failed_arm_fails_closed_before_release() {
    let trace = trace();
    let control = MockReset::traced(&trace);
    let held = control.held.clone();
    let mut guard = MockGuard::traced(&trace);
    guard.fail_arm = true;
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [control],
        flash_guards: [guarded(
            guard,
            RegionSource::Table(BMC_WRITABLE),
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });
    let mut orch = orchestrator();

    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));

    assert_eq!(orch.state(), State::Locked);
    assert!(held.get(), "never left reset");
    assert!(
        trace.borrow().is_empty(),
        "nothing actuated after the failed arm"
    );
}

#[test]
fn failed_bus_handover_fails_closed_before_release() {
    let control = MockReset::new();
    let held = control.held.clone();
    let mut guard = MockGuard::new();
    guard.fail = true;
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [control],
        flash_guards: [guarded(
            guard,
            RegionSource::Table(BMC_WRITABLE),
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });

    assert_eq!(driver.release_reset(C0), Err(DriverError::FlashGuardFault));
    assert!(held.get(), "never left reset");
}

const LOG: &[Violation] = &[
    Violation::BlockedRead(0x0000_0000),
    Violation::BlockedWrite(0x0000_4000),
    Violation::BlockedOpcode(0xc7),
];

// Under Recover a blocked write is evidence: it becomes FlashTampered, and
// every entry still reaches the sink. Reads and opcodes are report-only.
#[test]
fn blocked_write_under_recover_becomes_flash_tampered() {
    let mut driver = guarded_driver(
        MockGuard::logging(LOG),
        RegionSource::Table(BMC_WRITABLE),
        ViolationPolicy::Recover,
    );

    assert_eq!(
        driver.poll_flash_guards(),
        Ok(Some(Event::FlashTampered(C0)))
    );
    assert_eq!(driver.poll_flash_guards(), Ok(None));
    assert_eq!(
        driver.board().report_sink.seen,
        LOG.iter()
            .map(|&violation| Report::FlashViolation { id: C0, violation })
            .collect::<std::vec::Vec<_>>()
    );
}

// Under Log the same log changes nothing: reported, never escalated.
#[test]
fn violations_under_log_policy_are_report_only() {
    let mut driver = guarded_driver(
        MockGuard::logging(LOG),
        RegionSource::Table(BMC_WRITABLE),
        ViolationPolicy::Log,
    );

    assert_eq!(driver.poll_flash_guards(), Ok(None));
    assert_eq!(driver.board().report_sink.seen.len(), LOG.len());
}

// A guard that cannot be read is a fault, not an empty log.
#[test]
fn unreadable_guard_log_is_a_fault() {
    let mut guard = MockGuard::new();
    guard.fail = true;
    let mut driver = guarded_driver(
        guard,
        RegionSource::Table(BMC_WRITABLE),
        ViolationPolicy::Recover,
    );

    assert_eq!(
        driver.poll_flash_guards(),
        Err(DriverError::FlashGuardFault)
    );
}

// Tampering runs the corruption path. For an `Isolable` device that means it
// is gated: held in reset, with the bus back at the RoT, and reported. (A
// `Required` device enters recovery instead, which ends at
// `RecoverComponent`; that executor is not composed yet and fails closed, as
// `boot_failure_fails_closed_without_recovery` documents.)
#[test]
fn tampering_on_an_isolable_device_holds_it_and_takes_the_bus_back() {
    let trace = trace();
    let control = MockReset::traced(&trace);
    let held = control.held.clone();
    let mut driver = PlatformDriver::<MockBoard, 1>::new(Board {
        boot_controls: [control],
        flash_guards: [guarded(
            MockGuard::traced(&trace),
            RegionSource::Table(BMC_WRITABLE),
            ViolationPolicy::Recover,
        )],
        ..mock_board()
    });
    let mut chain = heapless::Vec::<_, 1>::new();
    chain
        .push((C0, ComponentAttrs::passive_isolable()))
        .unwrap();
    let mut orch = Orchestrator::<1, 4>::new(chain.try_into().unwrap(), 3);
    orch.dispatch(&mut driver, Event::PowerGood(PowerOnResult::Provisioned));
    assert_eq!(orch.state(), State::Ready);
    assert!(!held.get());
    trace.borrow_mut().clear();

    match &mut driver.board_mut().flash_guards[0] {
        GuardBinding::Guarded { guard, .. } => {
            guard
                .violations
                .push_back(Violation::BlockedWrite(0x0000_4000));
        }
        GuardBinding::Unguarded => unreachable!(),
    }
    let tampered = driver
        .poll_flash_guards()
        .unwrap()
        .expect("blocked write escalates");
    orch.dispatch(&mut driver, tampered);

    assert_eq!(orch.state(), State::Ready, "contained, not locked");
    assert!(held.get(), "held again");
    assert_eq!(
        *trace.borrow(),
        std::vec![Step::Hold, Step::Connect(Owner::Rot)],
        "quiesced, then the RoT owns the flash"
    );
    assert_eq!(
        driver.board().report_sink.seen,
        [
            Report::FlashViolation {
                id: C0,
                violation: Violation::BlockedWrite(0x0000_4000),
            },
            Report::Isolated(C0),
        ]
    );
}
