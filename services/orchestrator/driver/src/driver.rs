// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! The [`PlatformDriver`]: one executor method per [`Effect`] variant, routed from
//! the SM through the [`Platform`] impl.

use openprot_orchestrator_sm::{
    BootFailureKind, ComponentId, ComponentKind, Effect, EffectError, Event, Platform,
};

use crate::board::{
    Board, BoardCapabilities, GuardBinding, ImageSource, RegionSource, Report, ReportSink,
    SvnFloorBinding, Verdict, Verifier, ViolationPolicy,
};
use orchestrator_capabilities::{
    BootControl, BootWatch, FailureCause, FlashGuard, GuardRules, Owner, Svn, SvnFloor, Violation,
    WalkVerdict,
};

/// Why the driver could not carry out an effect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriverError {
    /// The effect names a component the driver has no device for.
    UnknownComponent,
    /// The component's image source could not be opened.
    ImageUnavailable,
    /// Verify was asked for a component whose image was never staged.
    NotStaged,
    /// The verifier could not perform the check (a failed image is a
    /// [`Verdict`], not an error).
    VerifierFault,
    /// The component's boot control could not actuate the reset line.
    BootControlFault,
    /// A floor commit was asked for a component with no verified image —
    /// the SVN to advance to is unknown; fail closed.
    NoVerifiedImage,
    /// The component's SVN floor could not be advanced.
    SvnFloorFault,
    /// The component's rules come from its manifest, but the verifier has no
    /// authenticated image on record to take them from; fail closed.
    GuardRulesUnavailable,
    /// The component's flash guard could not be armed, could not hand over
    /// the bus, or could not be read.
    FlashGuardFault,
}

impl core::fmt::Display for DriverError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            DriverError::UnknownComponent => "no device for this component id",
            DriverError::ImageUnavailable => "image source could not be opened",
            DriverError::NotStaged => "no image staged for this component",
            DriverError::VerifierFault => "verifier could not perform the check",
            DriverError::BootControlFault => "boot control could not actuate the reset",
            DriverError::NoVerifiedImage => "no verified image to commit the floor to",
            DriverError::SvnFloorFault => "svn floor could not be advanced",
            DriverError::GuardRulesUnavailable => {
                "no authenticated manifest to take the flash guard rules from"
            }
            DriverError::FlashGuardFault => "flash guard could not be armed, switched or read",
        })
    }
}

impl core::error::Error for DriverError {}

/// The effect executors. Everything device-specific lives in the [`Board`];
/// the driver's own fields are bookkeeping.
pub struct PlatformDriver<B: BoardCapabilities, const N: usize> {
    board: Board<B, N>,
    /// Component whose image is staged (source opened) for verification.
    staged: Option<ComponentId>,
    /// `watching[i]`: `ComponentId(i)` is out of reset with a walk in
    /// flight. Set on `ReleaseReset`, cleared on `AssertReset` and on a
    /// terminal verdict. Only watched walks are polled, so a finished or
    /// quiesced walk emits no stale event.
    watching: [bool; N],
    /// `verified_svn[i]` is the manifest SVN of `ComponentId(i)`'s last
    /// authenticated image — the only value a floor commit may trust.
    /// `None` until a verification passes; cleared again on rejection.
    verified_svn: [Option<Svn>; N],
}

impl<B: BoardCapabilities, const N: usize> PlatformDriver<B, N> {
    pub fn new(board: Board<B, N>) -> Self {
        // ComponentId is a u8, so ids for N > 256 components would wrap.
        const { assert!(N <= 256) };
        Self {
            board,
            staged: None,
            watching: [false; N],
            verified_svn: [None; N],
        }
    }

    /// The board wiring, read-only, for the tests: they observe a
    /// capability after it moved into the driver, instead of every mock
    /// smuggling out a shared handle. Real consumers get targeted queries
    /// when they exist — not this.
    #[cfg(test)]
    pub(crate) fn board(&self) -> &Board<B, N> {
        &self.board
    }

    /// Mutable twin of [`board`](Self::board), for a test that scripts a
    /// capability after it moved into the driver.
    #[cfg(test)]
    pub(crate) fn board_mut(&mut self) -> &mut Board<B, N> {
        &mut self.board
    }

    /// `id`'s image source. Takes the array rather than `&mut self` so the
    /// caller can borrow `board.verifier` alongside the returned image.
    fn source(images: &mut [B::Image; N], id: ComponentId) -> Result<&mut B::Image, DriverError> {
        images
            .get_mut(id.get() as usize)
            .ok_or(DriverError::UnknownComponent)
    }

    /// Stage `id`'s image: open its source so
    /// [`verify_firmware`](Self::verify_firmware) can read it.
    pub fn stage_firmware(&mut self, id: ComponentId) -> Result<(), DriverError> {
        self.staged = None;
        let source = Self::source(&mut self.board.images, id)?;
        source.open().map_err(|_| DriverError::ImageUnavailable)?;
        self.staged = Some(id);
        Ok(())
    }

    /// Judge the staged image via the [`Verifier`] and return the verdict:
    /// `Event::VerificationPassed(id)` or `Event::VerificationFailed(id)`.
    pub fn verify_firmware(&mut self, id: ComponentId) -> Result<Event, DriverError> {
        // Id validity first: an unknown component is UnknownComponent even
        // though it can never be staged.
        let source = Self::source(&mut self.board.images, id)?;
        if self.staged != Some(id) {
            return Err(DriverError::NotStaged);
        }
        let verdict = self
            .board
            .verifier
            .verify(id, source)
            .map_err(|_| DriverError::VerifierFault)?;
        let idx = id.get() as usize;
        Ok(match verdict {
            Verdict::Authenticated { svn } => {
                self.verified_svn[idx] = Some(svn);
                Event::VerificationPassed(id)
            }
            Verdict::Rejected => {
                self.verified_svn[idx] = None;
                Event::VerificationFailed(id)
            }
        })
    }

    /// Advance `id`'s anti-rollback floor to its verified image's SVN.
    /// A self-managed component keeps its own floor; the commit is a
    /// no-op. A target at or below the current floor is the capability's
    /// documented no-op, so a replayed commit is harmless.
    pub fn commit_svn_floor(&mut self, id: ComponentId) -> Result<(), DriverError> {
        let idx = id.get() as usize;
        let SvnFloorBinding::Erot(floor) = self
            .board
            .svn_floors
            .get_mut(idx)
            .ok_or(DriverError::UnknownComponent)?
        else {
            return Ok(());
        };
        let svn = self.verified_svn[idx].ok_or(DriverError::NoVerifiedImage)?;
        floor.advance(svn).map_err(|_| DriverError::SvnFloorFault)
    }

    /// `id`'s reset actuator.
    fn boot_control(&mut self, id: ComponentId) -> Result<&mut B::BootControl, DriverError> {
        self.board
            .boot_controls
            .get_mut(id.get() as usize)
            .ok_or(DriverError::UnknownComponent)
    }

    /// Arm `id`'s flash guard with the rules for its verified image: the
    /// table's spans, or the spans the verifier read from the manifest.
    /// A component with no guard is a no-op, as the effect's contract says.
    /// `Ok` means the guard reports the rules in force and read back; a
    /// guard that cannot be proven armed fails the effect, and the SM never
    /// releases a device behind it.
    pub fn arm_flash_guard(&mut self, id: ComponentId) -> Result<(), DriverError> {
        let Board {
            flash_guards,
            verifier,
            ..
        } = &mut self.board;
        let binding = flash_guards
            .get_mut(id.get() as usize)
            .ok_or(DriverError::UnknownComponent)?;
        let GuardBinding::Guarded { guard, regions, .. } = binding else {
            return Ok(());
        };
        let rules = match *regions {
            RegionSource::Table(writable) => GuardRules {
                writable,
                unreadable: &[],
            },
            RegionSource::Manifest => verifier
                .guard_rules(id)
                .ok_or(DriverError::GuardRulesUnavailable)?,
        };
        guard.arm(&rules).map_err(|_| DriverError::FlashGuardFault)
    }

    /// Hand `id`'s flash to `owner`, if a guard fronts it.
    fn hand_bus(&mut self, id: ComponentId, owner: Owner) -> Result<(), DriverError> {
        match self
            .board
            .flash_guards
            .get_mut(id.get() as usize)
            .ok_or(DriverError::UnknownComponent)?
        {
            GuardBinding::Unguarded => Ok(()),
            GuardBinding::Guarded { guard, .. } => guard
                .connect(owner)
                .map_err(|_| DriverError::FlashGuardFault),
        }
    }

    /// Release `id` from reset and arm its boot walk;
    /// [`poll_boot_walks`](Self::poll_boot_walks) feeds the verdict back
    /// as `ComponentReady(id)`/`Booted(id)`/`BootFailed { id, .. }`. Arms on every
    /// release: a retry re-release starts a fresh walk.
    ///
    /// The bus is handed to the device before its reset is released. The
    /// guard has been filtering since `ArmFlashGuard`, so there is no window
    /// in which the device sees an unfiltered bus, and no fetch from a flash
    /// the RoT still owns.
    pub fn release_reset(&mut self, id: ComponentId) -> Result<(), DriverError> {
        self.hand_bus(id, Owner::Device)?;
        self.boot_control(id)?
            .release()
            .map_err(|_| DriverError::BootControlFault)?;
        let idx = id.get() as usize;
        // In bounds: boot_control(id) above already rejected unknown ids.
        self.board.boot_watches[idx].arm();
        self.watching[idx] = true;
        Ok(())
    }

    /// Hold `id` in reset — a durable quiesce, not a pulse; at-rest
    /// verification and the recovery re-walk depend on it. Also stops the
    /// boot walk: a held device produces no boot signal, so polling it
    /// could only yield a stale `BootFailed`.
    ///
    /// Once the device is held, its flash goes back to the RoT: the
    /// at-rest verification and any recovery write that follow run on the
    /// RoT's own controller.
    pub fn assert_reset(&mut self, id: ComponentId) -> Result<(), DriverError> {
        self.boot_control(id)?
            .hold_in_reset()
            .map_err(|_| DriverError::BootControlFault)?;
        self.watching[id.get() as usize] = false;
        self.hand_bus(id, Owner::Rot)
    }

    /// Drains every guard's violation log, handing each entry to the report
    /// sink, and returns the first blocked write of a component whose policy
    /// is [`ViolationPolicy::Recover`] as `FlashTampered(id)`. Blocked reads
    /// and opcodes are report-only under both policies. Returns at the first
    /// event; drain by calling until `Ok(None)`.
    ///
    /// A guard that cannot be read is a fault, not an empty log: the caller
    /// decides whether a blind guard is tolerable.
    pub fn poll_flash_guards(&mut self) -> Result<Option<Event>, DriverError> {
        let Board {
            flash_guards,
            report_sink,
            ..
        } = &mut self.board;
        for (idx, binding) in flash_guards.iter_mut().enumerate() {
            let GuardBinding::Guarded {
                guard, violations, ..
            } = binding
            else {
                continue;
            };
            let id: ComponentId = (idx as u8).into();
            while let Some(violation) = guard
                .take_violation()
                .map_err(|_| DriverError::FlashGuardFault)?
            {
                report_sink.report(Report::FlashViolation { id, violation });
                if *violations == ViolationPolicy::Recover
                    && matches!(violation, Violation::BlockedWrite(_))
                {
                    return Ok(Some(Event::FlashTampered(id)));
                }
            }
        }
        Ok(None)
    }

    /// Polls every watched walk at `now_millis` and returns the first
    /// terminal verdict as its event: [`WalkVerdict::Complete`] becomes
    /// `ComponentReady(id)` (`Active`) or `Booted(id)` (`Passive`),
    /// [`WalkVerdict::Failed`] becomes `BootFailed { id, checkpoint, kind }`.
    /// The finished walk stops being watched;
    /// each verdict is delivered once.
    ///
    /// Returns at the first event; drain by calling until
    /// [`BootWalkPoll::event`] is `None`. Only that last poll carries a
    /// complete [`next_deadline_millis`](BootWalkPoll::next_deadline_millis)
    /// — the earliest deadline among the still-waiting walks.
    pub fn poll_boot_walks(&mut self, now_millis: u64) -> BootWalkPoll {
        let mut next_deadline_millis: Option<u64> = None;
        for idx in 0..N {
            if !self.watching[idx] {
                continue;
            }
            let id: ComponentId = (idx as u8).into();
            match self.board.boot_watches[idx].poll(now_millis) {
                WalkVerdict::Waiting { deadline_millis } => {
                    next_deadline_millis = Some(match next_deadline_millis {
                        Some(d) => d.min(deadline_millis),
                        None => deadline_millis,
                    });
                }
                WalkVerdict::Complete => {
                    self.watching[idx] = false;
                    let event = match self.board.component_kinds[idx] {
                        ComponentKind::Active => Event::ComponentReady(id),
                        ComponentKind::Passive => Event::Booted(id),
                    };
                    return BootWalkPoll {
                        event: Some(event),
                        next_deadline_millis,
                    };
                }
                WalkVerdict::Failed { checkpoint, cause } => {
                    self.watching[idx] = false;
                    let kind = match cause {
                        FailureCause::TimedOut => BootFailureKind::TimedOut,
                        FailureCause::DeviceRetriable => BootFailureKind::DeviceRetriable,
                        FailureCause::DeviceFatal => BootFailureKind::DeviceFatal,
                    };
                    return BootWalkPoll {
                        event: Some(Event::BootFailed {
                            id,
                            checkpoint,
                            kind,
                        }),
                        next_deadline_millis,
                    };
                }
            }
        }
        BootWalkPoll {
            event: None,
            next_deadline_millis,
        }
    }

    /// Hands one report to the board's sink. Cannot fail, so reporting stays
    /// off the fail-closed path; reports arrive in the order the SM emitted
    /// them.
    pub fn report(&mut self, report: Report) {
        self.board.report_sink.report(report);
    }
}

/// One [`PlatformDriver::poll_boot_walks`] round.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BootWalkPoll {
    /// The first terminal verdict's event; `None` when every watched walk
    /// is still waiting.
    pub event: Option<Event>,
    /// Earliest deadline among walks seen waiting this round. Complete only
    /// when [`event`](Self::event) is `None`: an early return skips the
    /// walks after the finished one.
    pub next_deadline_millis: Option<u64>,
}

impl<B: BoardCapabilities, const N: usize> Platform for PlatformDriver<B, N> {
    /// Routes each effect to its executor. Exhaustive: a new [`Effect`]
    /// variant must get an executor before this compiles. Synchronous
    /// results (the verification verdict) come back as the returned event;
    /// every executor error reports as [`EffectError`] — the SM treats all
    /// actuation failures the same, fail-closed.
    fn execute(&mut self, effect: Effect) -> Result<Option<Event>, EffectError> {
        match effect {
            Effect::ReadFirmware(id) => self.stage_firmware(id).map(|_| None),
            Effect::VerifyFirmware(id) => self.verify_firmware(id).map(Some),
            Effect::ArmFlashGuard(id) => self.arm_flash_guard(id).map(|_| None),
            Effect::ReleaseReset(id) => self.release_reset(id).map(|_| None),
            Effect::AssertReset(id) => self.assert_reset(id).map(|_| None),
            Effect::CommitSvnFloor(id) => self.commit_svn_floor(id).map(|_| None),
            // Reports carry no error, so they never reach the fail-closed
            // group below.
            Effect::ReportIsolated(id) => {
                self.report(Report::Isolated(id));
                Ok(None)
            }
            Effect::ReportRecoveryFailed(id) => {
                self.report(Report::RecoveryFailed(id));
                Ok(None)
            }
            Effect::ReportUpdateDeferred => {
                self.report(Report::UpdateDeferred);
                Ok(None)
            }
            Effect::ReportUpdateAborted => {
                self.report(Report::UpdateAborted);
                Ok(None)
            }
            Effect::ReportBootFailed {
                id,
                checkpoint,
                kind,
            } => {
                self.report(Report::BootFailed {
                    id,
                    checkpoint,
                    kind,
                });
                Ok(None)
            }
            // No board capability is composed for these seams yet, so they
            // fail closed here instead of behind stub methods. Each group
            // gains an executor when its capability joins
            // [`BoardCapabilities`], as BootControl did above: recovery
            // sourcing for RecoverComponent; update staging, authentication
            // and trial activation for the update quartet; evidence signing
            // for SignAttestation; the terminal latch for LatchLockdown.
            Effect::RecoverComponent { .. }
            | Effect::AuthenticateUpdate
            | Effect::StageUpdate
            | Effect::ActivateUpdate
            | Effect::DiscardStaged
            | Effect::SignAttestation
            | Effect::LatchLockdown => return Err(EffectError),
            // Emit is consumed by the orchestrator; receiving one is a
            // driver bug.
            Effect::Emit(_) => return Err(EffectError),
        }
        .map_err(|_| EffectError)
    }
}
