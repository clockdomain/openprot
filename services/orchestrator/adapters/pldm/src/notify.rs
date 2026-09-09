// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Translation between the PLDM notify channel and orchestrator-sm.
//!
//! The run loop that owns both sides is kernel-tagged (it blocks in
//! `object_wait`), but every *decision* it makes about a drained
//! [`Pending`] is pure mapping and lives here, on the host, where it is
//! unit-tested. The loop is left with syscalls and sequencing.
//!
//! Three questions are answered here:
//!
//! - Which [`Pending`] values become orchestrator-sm [`Event`]s
//!   ([`pending_to_event`]) — and, just as importantly, which do not.
//! - Which [`Effect`]s the supervisor reports back to PLDM as a [`Phase`]
//!   ([`phase_for_effect`]).
//! - What one `PldmLink::poll()` result means for the loop
//!   ([`classify_poll`]), including the bounded-deadline timeout that makes
//!   PLDM unhealthy.

use notify_api::{Pending, Phase, TransportError};
use notify_client::ClientError;
use openprot_orchestrator_sm::{Effect, Event};

/// Map a drained [`Pending`] onto the orchestrator-sm event it authorizes, if
/// any.
///
/// **The Update Agent is untrusted, and this function is the boundary that
/// keeps it that way.** No `Pending` maps to [`Event::UpdateVerified`]: that
/// event asserts the staged image passed authentication, a verdict only the
/// orchestrator's own crypto path may produce (it arrives as the return value
/// of [`Effect::AuthenticateUpdate`]). A wire message from the edge must never
/// be able to claim it.
///
/// The variants that map to nothing are not oversights:
///
/// - [`Pending::Offer`] carries staging parameters (target, total). The core
///   already emits [`Effect::StageUpdate`] on entry to `Updating`; validating
///   the target and reserving staging is the platform's job, not a transition.
/// - [`Pending::Complete`] says the transfer finished, which is a fact about
///   bytes, not about authenticity — see above.
/// - [`Pending::Activate`] is the UA asking; the core activates on its own
///   [`Event::UpdateVerified`], via [`Effect::ActivateUpdate`].
///
/// [`Pending::Abort`] maps to [`Event::UpdateRejected`] because that is the
/// event which, in `Updating`, emits [`Effect::DiscardStaged`] and returns the
/// machine to `Ready` — exactly what a cancel must do. The core has no
/// separate "aborted" event, and inventing one would mean touching every
/// handler; the conflation is safe in the direction that matters, since both
/// paths discard the staged image rather than keep it.
///
/// The match is exhaustive on purpose. A new [`Pending`] variant breaks this
/// build until someone decides, here, whether the untrusted edge may use it to
/// drive a transition — which is exactly the decision that should never be
/// made by a wildcard.
pub fn pending_to_event(pending: Pending) -> Option<Event> {
    match pending {
        Pending::UpdateRequested => Some(Event::UpdateRequest),
        Pending::Abort => Some(Event::UpdateRejected),
        Pending::Offer { .. } | Pending::Complete { .. } | Pending::Activate => None,
    }
}

/// Map an [`Effect`] the supervisor is executing onto the [`Phase`] it reports
/// to PLDM, so the Update Agent can watch progress over MCTP.
///
/// Only the update-lifecycle effects have a phase; boot-chain effects
/// (`ReleaseReset`, `VerifyFirmware`, the reports, …) are invisible to the UA
/// and map to `None` rather than being forced into a status byte.
///
/// [`Effect::DiscardStaged`] reports [`Phase::Failed`]: from the UA's side the
/// staged image is gone and the cycle is over. On a UA-initiated cancel that
/// reads as redundant rather than wrong — the UA already knows it cancelled.
pub fn phase_for_effect(effect: Effect) -> Option<Phase> {
    match effect {
        Effect::StageUpdate => Some(Phase::Staging),
        Effect::AuthenticateUpdate => Some(Phase::Verifying),
        Effect::ActivateUpdate => Some(Phase::Activating),
        Effect::DiscardStaged => Some(Phase::Failed),
        Effect::ReadFirmware(_)
        | Effect::VerifyFirmware(_)
        | Effect::ReleaseReset(_)
        | Effect::AssertReset(_)
        | Effect::SignAttestation
        | Effect::CommitSvnFloor(_)
        | Effect::RecoverComponent { .. }
        | Effect::ReportIsolated(_)
        | Effect::ReportRecoveryFailed(_)
        | Effect::ReportUpdateDeferred
        | Effect::ReportUpdateAborted
        | Effect::LatchLockdown
        | Effect::Emit(_) => None,
    }
}

/// What one `PldmLink::poll()` round-trip means to the run loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    /// PLDM had nothing latched. Re-arm the cadence and go back to waiting.
    Idle,
    /// PLDM latched an event that drives the core. Dispatch it.
    Dispatch(Event),
    /// PLDM latched an event the platform handles without the core (staging
    /// parameters, transfer completion, an activation request). Carried
    /// through so the loop can act on the payload; see [`pending_to_event`]
    /// for why these do not become transitions.
    Runtime(Pending),
    /// The round-trip did not complete before its bounded deadline. PLDM is
    /// unhealthy: unwind any in-flight update with
    /// [`unhealthy_unwind_event`], stop re-arming the poll cadence, and leave
    /// the watchdogs running.
    Unhealthy,
    /// PLDM answered inside its deadline but the exchange was not usable — a
    /// server-side error status or a malformed frame. The peer is alive, so
    /// this is not (yet) an unhealthy peer; the loop logs and keeps polling.
    Failed(ClientError),
}

/// Classify one `PldmLink::poll()` result.
///
/// The only failure that condemns the peer is
/// [`TransportError::Timeout`] — the bounded deadline elapsing is the
/// supervisor's evidence that PLDM is not answering at all. Everything else
/// means bytes came back, so the peer is alive and merely wrong; treating a
/// malformed frame as "dead" would let a single bad response tear down a
/// healthy update.
pub fn classify_poll(result: Result<Option<Pending>, ClientError>) -> PollOutcome {
    match result {
        Ok(None) => PollOutcome::Idle,
        Ok(Some(pending)) => match pending_to_event(pending) {
            Some(event) => PollOutcome::Dispatch(event),
            None => PollOutcome::Runtime(pending),
        },
        Err(ClientError::Transport(TransportError::Timeout)) => PollOutcome::Unhealthy,
        Err(e) => PollOutcome::Failed(e),
    }
}

/// The event that unwinds an in-flight update once PLDM is declared unhealthy.
///
/// Step 8 of the prototype plan calls for driving [`Effect::DiscardStaged`] and
/// releasing staging. The loop does *not* execute that effect behind the core's
/// back: doing so would discard the image while the machine sat in `Updating`
/// forever, waiting for a verdict from a peer that is gone. Dispatching
/// [`Event::UpdateRejected`] produces the same `DiscardStaged` *through* the
/// core, so staging is released and the machine returns to `Ready`. Outside
/// `Updating` the event is inert, which is what makes it safe to dispatch on
/// every unhealthy transition.
pub fn unhealthy_unwind_event() -> Event {
    Event::UpdateRejected
}

/// What the run loop should do after one poll, once the supervisor has taken
/// the peer's health into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reaction {
    /// Event to feed orchestrator-sm, if any.
    pub dispatch: Option<Event>,
    /// Whether to re-arm the poll cadence. `false` once the peer is unhealthy:
    /// the loop stops waking to poll a peer that has stopped answering, and
    /// its watchdogs keep running on their own deadlines regardless.
    pub rearm: bool,
}

/// Tracks whether the PLDM peer is answering, and turns a [`PollOutcome`] into
/// the loop's next move.
///
/// Health is one-way in this prototype: a peer that misses its bounded
/// deadline stays unhealthy. Re-admitting it would mean re-subscribing and
/// reconciling whatever update state it thinks it has, which is a recovery
/// design, not a timeout policy — and silently resuming a peer that went away
/// mid-update is exactly the case where guessing is worst.
#[derive(Debug, Default)]
pub struct NotifySupervisor {
    unhealthy: bool,
}

impl NotifySupervisor {
    /// A supervisor whose peer is presumed healthy until it misses a deadline.
    pub const fn new() -> Self {
        Self { unhealthy: false }
    }

    /// Whether the peer is still considered able to answer.
    pub fn is_healthy(&self) -> bool {
        !self.unhealthy
    }

    /// Fold one poll outcome into the supervisor's health and report the
    /// loop's next move.
    ///
    /// The unhealthy transition happens once: the first timeout dispatches
    /// [`unhealthy_unwind_event`] to release staging through the core, and
    /// every later poll (there should be none — the cadence is not re-armed)
    /// is inert.
    pub fn on_poll(&mut self, outcome: PollOutcome) -> Reaction {
        match outcome {
            PollOutcome::Unhealthy => {
                let first = !self.unhealthy;
                self.unhealthy = true;
                Reaction {
                    dispatch: first.then(unhealthy_unwind_event),
                    rearm: false,
                }
            }
            _ if self.unhealthy => Reaction {
                dispatch: None,
                rearm: false,
            },
            PollOutcome::Dispatch(event) => Reaction {
                dispatch: Some(event),
                rearm: true,
            },
            PollOutcome::Idle | PollOutcome::Runtime(_) | PollOutcome::Failed(_) => Reaction {
                dispatch: None,
                rearm: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_requested_is_the_intake_event() {
        assert_eq!(
            pending_to_event(Pending::UpdateRequested),
            Some(Event::UpdateRequest)
        );
    }

    #[test]
    fn abort_discards_through_the_core() {
        assert_eq!(
            pending_to_event(Pending::Abort),
            Some(Event::UpdateRejected),
            "abort must produce the event that emits DiscardStaged"
        );
    }

    #[test]
    fn no_wire_message_can_assert_verification() {
        // The security property this boundary exists for: the untrusted edge
        // must never be able to claim the staged image authenticated.
        for pending in [
            Pending::UpdateRequested,
            Pending::Offer {
                target: 0x8000_0000,
                total: 4096,
            },
            Pending::Complete { written: 4096 },
            Pending::Activate,
            Pending::Abort,
        ] {
            assert_ne!(
                pending_to_event(pending),
                Some(Event::UpdateVerified),
                "{pending:?} must not assert UpdateVerified"
            );
        }
    }

    #[test]
    fn transfer_shaped_events_stay_out_of_the_core() {
        for pending in [
            Pending::Offer {
                target: 1,
                total: 2,
            },
            Pending::Complete { written: 2 },
            Pending::Activate,
        ] {
            assert_eq!(pending_to_event(pending), None);
        }
    }

    #[test]
    fn update_effects_map_to_phases() {
        assert_eq!(phase_for_effect(Effect::StageUpdate), Some(Phase::Staging));
        assert_eq!(
            phase_for_effect(Effect::AuthenticateUpdate),
            Some(Phase::Verifying)
        );
        assert_eq!(
            phase_for_effect(Effect::ActivateUpdate),
            Some(Phase::Activating)
        );
        assert_eq!(phase_for_effect(Effect::DiscardStaged), Some(Phase::Failed));
    }

    #[test]
    fn boot_chain_effects_have_no_phase() {
        use openprot_orchestrator_sm::ComponentId;
        let id = ComponentId::new(0);
        for effect in [
            Effect::ReleaseReset(id),
            Effect::VerifyFirmware(id),
            Effect::CommitSvnFloor(id),
            Effect::LatchLockdown,
            Effect::ReportUpdateDeferred,
        ] {
            assert_eq!(phase_for_effect(effect), None);
        }
    }

    #[test]
    fn nothing_latched_is_idle() {
        assert_eq!(classify_poll(Ok(None)), PollOutcome::Idle);
    }

    #[test]
    fn latched_intake_is_dispatched() {
        assert_eq!(
            classify_poll(Ok(Some(Pending::UpdateRequested))),
            PollOutcome::Dispatch(Event::UpdateRequest)
        );
    }

    #[test]
    fn latched_transfer_event_is_runtime_only() {
        let offer = Pending::Offer {
            target: 0x1000,
            total: 64,
        };
        assert_eq!(classify_poll(Ok(Some(offer))), PollOutcome::Runtime(offer));
    }

    #[test]
    fn bounded_deadline_timeout_condemns_the_peer() {
        assert_eq!(
            classify_poll(Err(ClientError::Transport(TransportError::Timeout))),
            PollOutcome::Unhealthy
        );
    }

    #[test]
    fn a_live_but_wrong_peer_is_not_unhealthy() {
        // Bytes came back, so the peer is answering: not the unhealthy path.
        for err in [
            ClientError::InvalidResponse,
            ClientError::Transport(TransportError::Failed),
            ClientError::ServerError(notify_api::NotifyError::InternalError),
        ] {
            assert_eq!(classify_poll(Err(err)), PollOutcome::Failed(err));
        }
    }

    #[test]
    fn unhealthy_unwind_is_the_discarding_event() {
        assert_eq!(unhealthy_unwind_event(), Event::UpdateRejected);
    }

    #[test]
    fn a_healthy_peer_keeps_the_cadence_armed() {
        let mut sup = NotifySupervisor::new();
        assert_eq!(
            sup.on_poll(PollOutcome::Idle),
            Reaction {
                dispatch: None,
                rearm: true
            }
        );
        assert!(sup.is_healthy());
    }

    #[test]
    fn a_dispatched_event_still_re_arms() {
        let mut sup = NotifySupervisor::new();
        assert_eq!(
            sup.on_poll(PollOutcome::Dispatch(Event::UpdateRequest)),
            Reaction {
                dispatch: Some(Event::UpdateRequest),
                rearm: true
            }
        );
    }

    #[test]
    fn a_silent_peer_unwinds_the_update_and_stops_the_cadence() {
        // The step-8 path: a PLDM that never answers must not leave the
        // machine parked in `Updating`, and must not be polled again.
        let mut sup = NotifySupervisor::new();
        let reaction = sup.on_poll(PollOutcome::Unhealthy);
        assert_eq!(
            reaction,
            Reaction {
                dispatch: Some(Event::UpdateRejected),
                rearm: false
            },
            "the first timeout must release staging through the core"
        );
        assert!(!sup.is_healthy());
    }

    #[test]
    fn the_unwind_is_dispatched_only_once() {
        let mut sup = NotifySupervisor::new();
        let _ = sup.on_poll(PollOutcome::Unhealthy);
        assert_eq!(
            sup.on_poll(PollOutcome::Unhealthy),
            Reaction {
                dispatch: None,
                rearm: false
            }
        );
    }

    #[test]
    fn an_unhealthy_peer_is_not_re_admitted_by_a_late_answer() {
        let mut sup = NotifySupervisor::new();
        let _ = sup.on_poll(PollOutcome::Unhealthy);
        // Even a well-formed event from a condemned peer neither dispatches
        // nor restarts the cadence.
        assert_eq!(
            sup.on_poll(PollOutcome::Dispatch(Event::UpdateRequest)),
            Reaction {
                dispatch: None,
                rearm: false
            }
        );
        assert!(!sup.is_healthy());
    }
}
