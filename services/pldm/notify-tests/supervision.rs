// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Host end-to-end for the supervisor's half of the notify channel: real
//! `PldmLink` marshalling -> a transport -> the orchestrator's classification
//! and health policy. Everything the kernel-tagged run loop decides, minus the
//! syscalls it decides it with.
//!
//! The loop itself (`openprot_orchestrator_server::supervisor::run`) is a thin
//! sequencer over exactly these calls, so a regression in the policy shows up
//! here rather than only under QEMU.

use notify_api::{Decision, Pending, Transport, TransportError};
use notify_client::PldmLink;
use notify_server::loopback::LoopbackTransport;
use notify_server::NotifyState;
use openprot_orchestrator_pldm_adapter::notify::{
    classify_poll, NotifySupervisor, PollOutcome, Reaction,
};
use openprot_orchestrator_sm::Event;

/// A PLDM that never answers: every round-trip burns its bounded deadline.
struct SilentPldm;

impl Transport for SilentPldm {
    fn transact(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, TransportError> {
        Err(TransportError::Timeout)
    }
}

/// The healthy path, end to end: PLDM latches an accepted `RequestUpdate`, the
/// supervisor polls it out and turns it into the core's intake event.
#[test]
fn a_nudged_update_request_reaches_the_core() {
    let mut state = NotifyState::new();
    let mut supervisor = NotifySupervisor::new();

    {
        let mut link = PldmLink::new(LoopbackTransport::new(&mut state));
        link.subscribe().unwrap();
        // Nothing latched yet: idle, and the cadence keeps running.
        assert_eq!(
            supervisor.on_poll(classify_poll(link.poll())),
            Reaction {
                dispatch: None,
                rearm: true
            }
        );
    }

    state.latch(Pending::UpdateRequested);

    let mut link = PldmLink::new(LoopbackTransport::new(&mut state));
    let reaction = supervisor.on_poll(classify_poll(link.poll()));
    assert_eq!(
        reaction,
        Reaction {
            dispatch: Some(Event::UpdateRequest),
            rearm: true
        }
    );
    assert!(supervisor.is_healthy());
    link.decide(Decision::Accepted).unwrap();
}

/// Transfer-shaped events are drained and acted on by the platform, but never
/// become transitions — the untrusted edge does not drive the core directly.
#[test]
fn transfer_events_drain_without_touching_the_core() {
    let mut state = NotifyState::new();
    let mut supervisor = NotifySupervisor::new();
    let offer = Pending::Offer {
        target: 0x8000_0000,
        total: 65536,
    };
    state.latch(offer);

    let mut link = PldmLink::new(LoopbackTransport::new(&mut state));
    let outcome = classify_poll(link.poll());
    assert_eq!(outcome, PollOutcome::Runtime(offer));
    assert_eq!(
        supervisor.on_poll(outcome),
        Reaction {
            dispatch: None,
            rearm: true
        }
    );
}

/// The step-8 path. A PLDM that never answers must not leave the machine
/// parked in `Updating`: the bounded deadline elapses, the supervisor
/// condemns the peer, and the unwind event releases staging through the core.
#[test]
fn a_silent_pldm_is_condemned_and_unwinds_the_update() {
    let mut supervisor = NotifySupervisor::new();
    let mut link = PldmLink::new(SilentPldm);

    let reaction = supervisor.on_poll(classify_poll(link.poll()));
    assert_eq!(
        reaction,
        Reaction {
            dispatch: Some(Event::UpdateRejected),
            rearm: false
        },
        "a silent peer must release staging and stop the cadence"
    );
    assert!(!supervisor.is_healthy());
}

/// A dead peer is polled once, not forever: the cadence is never re-armed, and
/// the unwind is not re-dispatched on any later pass.
#[test]
fn a_dead_peer_is_not_polled_again() {
    let mut supervisor = NotifySupervisor::new();
    let mut link = PldmLink::new(SilentPldm);

    let first = supervisor.on_poll(classify_poll(link.poll()));
    assert_eq!(first.dispatch, Some(Event::UpdateRejected));

    for _ in 0..3 {
        let again = supervisor.on_poll(classify_poll(link.poll()));
        assert_eq!(
            again,
            Reaction {
                dispatch: None,
                rearm: false
            },
            "the unwind is dispatched once and the cadence stays stopped"
        );
    }
}

/// A status push is an Orchestrator -> PLDM round-trip like any other, so it
/// is bounded too: a silent peer surfaces the same timeout the loop folds into
/// the health path.
#[test]
fn a_status_push_to_a_silent_peer_times_out() {
    use notify_api::Phase;
    use notify_client::ClientError;

    let mut link = PldmLink::new(SilentPldm);
    assert_eq!(
        link.push_status(Phase::Staging),
        Err(ClientError::Transport(TransportError::Timeout))
    );
}
