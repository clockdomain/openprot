// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! The orchestrator's single timed event loop.
//!
//! One `object_wait`, one deadline, one place events enter the core. The
//! deadline comes from [`BootWatchdogs::wait_deadline`], so the boot-progress
//! and commit watchdogs and the PLDM poll cadence all expire through the same
//! park — there is no second timer task and no IPC on the arm path.
//!
//! ## Why the supervisor polls
//!
//! The loop is the *client* on the notify channel: it subscribes, then pulls.
//! That inversion (PLDM is the server, like i2c and mctp) is what lets the
//! commit window and the boot watchdogs stay the orchestrator's own
//! obligations. A wedged PLDM costs a poll's bounded timeout; it cannot hold
//! the supervisor past its next deadline, because the deadline is in the same
//! `object_wait` that the nudge would wake.
//!
//! Everything policy-shaped here — which `Pending` becomes an [`Event`], which
//! [`Effect`](openprot_orchestrator_sm::Effect) is reported as a `Phase`, when
//! a peer is unhealthy — lives in `openprot_orchestrator_pldm_adapter::notify`
//! and is host-tested there. This module is syscalls and sequencing.
//!
//! ## Scope
//!
//! The boot and commit watchdogs are drained here but armed by the platform
//! driver as it executes `ReleaseReset` / `ActivateUpdate`; wiring the driver
//! to `watchdogs` is not part of this prototype. Pass the same
//! [`BootWatchdogs`] to both once it is.

use notify_api::{Phase, Transport, TransportError};
use notify_client::{ClientError, PldmLink};
use openprot_orchestrator_pldm_adapter::notify::{
    classify_poll, phase_for_effect, NotifySupervisor, PollOutcome,
};
use openprot_orchestrator_sm::{Event, Orchestrator, Platform};
use userspace::syscall::{self, Signals};
use userspace::time::Duration;

use crate::runtime::{BootWatchdogs, Wake};

/// Run the orchestrator forever.
///
/// `wg` is the process's WaitGroup and `notify_channel` the IPC channel to the
/// PLDM firmware device. Registration is two steps, matching the design doc:
/// a kernel-level subscription (`wait_group_add` for `Signals::USER`) so this
/// loop can wake on PLDM's nudge, and a protocol-level `Subscribe` so PLDM
/// knows to raise it.
///
/// `poll_interval` is the cadence at which the loop polls PLDM even in the
/// absence of a nudge. It is a backstop, not the primary path: a healthy PLDM
/// nudges, and the cadence covers a nudge that was never raised because the
/// peer never got that far.
pub fn run<T, P, const N: usize, const E: usize>(
    wg: u32,
    notify_channel: u32,
    pldm: &mut PldmLink<T>,
    orch: &mut Orchestrator<N, E>,
    platform: &mut P,
    watchdogs: &mut BootWatchdogs<N>,
    poll_interval: Duration,
) -> !
where
    T: Transport,
    P: Platform,
{
    // Registration part 1: kernel-level. The USER signal is level-triggered,
    // so an event PLDM latches before the first park is not lost.
    if syscall::wait_group_add(wg, notify_channel, Signals::USER, notify_channel as usize).is_err()
    {
        pw_log::error!("orchestrator: wait_group_add on the notify channel failed");
    }

    let mut supervisor = NotifySupervisor::new();

    // Registration part 2: protocol-level. A peer that will not accept the
    // subscription is already not answering, so classify the failure through
    // the same health path rather than looping on it.
    if let Err(e) = pldm.subscribe() {
        pw_log::error!("orchestrator: PLDM subscribe failed");
        let _ = supervisor.on_poll(classify_poll(Err(e)));
    }
    if supervisor.is_healthy() {
        watchdogs.arm_poll(poll_interval);
    }

    loop {
        // The single park. Its deadline is whichever obligation comes first —
        // a boot watchdog, the commit window, or the next poll.
        let woke = syscall::object_wait(wg, Signals::USER, watchdogs.wait_deadline());

        // Drain every deadline that has passed, whatever woke us. A signal
        // arriving just before a watchdog expires must not defer the watchdog.
        // The poll is only *noted* here, not performed: re-arming the cadence
        // mid-drain would make a zero interval spin this loop forever.
        let mut poll_due = false;
        while let Some(wake) = watchdogs.poll_expired() {
            match wake {
                Wake::Event(event) => {
                    dispatch_reporting_status(orch, platform, pldm, &mut supervisor, event);
                }
                Wake::PollDue => poll_due = true,
            }
        }

        // A nudge means PLDM has something latched for us. A nudge and an
        // expired cadence in the same tick are the same errand, so they
        // coalesce into one round-trip rather than burning two deadlines.
        // An unhealthy peer is never polled again, whatever it signals: step 8
        // says stop retrying a dead peer, and the cadence being cancelled only
        // covers the timer half of that.
        let nudged = matches!(woke, Ok(w) if w.pending_signals.contains(Signals::USER));
        if (poll_due || nudged) && supervisor.is_healthy() {
            poll_pldm(
                pldm,
                &mut supervisor,
                orch,
                platform,
                watchdogs,
                poll_interval,
            );
        }
    }
}

/// Poll PLDM once and act on the outcome: dispatch what the core should see,
/// and re-arm the cadence unless the peer has been declared unhealthy.
fn poll_pldm<T, P, const N: usize, const E: usize>(
    pldm: &mut PldmLink<T>,
    supervisor: &mut NotifySupervisor,
    orch: &mut Orchestrator<N, E>,
    platform: &mut P,
    watchdogs: &mut BootWatchdogs<N>,
    poll_interval: Duration,
) where
    T: Transport,
    P: Platform,
{
    let reaction = supervisor.on_poll(classify_poll(pldm.poll()));
    if let Some(event) = reaction.dispatch {
        dispatch_reporting_status(orch, platform, pldm, supervisor, event);
    }
    if reaction.rearm {
        watchdogs.arm_poll(poll_interval);
    } else {
        // A dead peer is not polled again; the watchdogs keep their own
        // deadlines, so the loop still wakes for everything that matters.
        watchdogs.cancel_poll();
        pw_log::warn!("orchestrator: PLDM unhealthy, poll cadence stopped");
    }
}

/// Dispatch one event, reporting each update-lifecycle effect to PLDM as it is
/// executed.
///
/// The status push happens *before* the effect runs: the Update Agent is
/// watching for the phase to begin, and an effect that fails is followed by
/// the core's own fail-closed path anyway.
///
/// A status push is an Orchestrator → PLDM `transact` like any other, so it is
/// bounded and can time out. When it does, the peer is condemned here too —
/// the unwind is then dispatched without status reporting, which terminates:
/// no further pushes, so no further timeouts.
fn dispatch_reporting_status<T, P, const N: usize, const E: usize>(
    orch: &mut Orchestrator<N, E>,
    platform: &mut P,
    pldm: &mut PldmLink<T>,
    supervisor: &mut NotifySupervisor,
    event: Event,
) where
    T: Transport,
    P: Platform,
{
    let mut push_timed_out = false;
    orch.dispatch_with(event, |effect| {
        if let Some(phase) = phase_for_effect(effect)
            && push_status_timed_out(pldm, phase)
        {
            push_timed_out = true;
        }
        platform.execute(effect)
    });

    if push_timed_out {
        let reaction = supervisor.on_poll(PollOutcome::Unhealthy);
        if let Some(unwind) = reaction.dispatch {
            pw_log::error!("orchestrator: PLDM status push timed out, unwinding update");
            orch.dispatch(platform, unwind);
        }
    }
}

/// Push one phase, reporting only whether the peer failed to answer in time.
/// A rejected or malformed answer means PLDM is alive and merely unhappy with
/// a status byte, which is not grounds for tearing down an update.
fn push_status_timed_out<T: Transport>(pldm: &mut PldmLink<T>, phase: Phase) -> bool {
    matches!(
        pldm.push_status(phase),
        Err(ClientError::Transport(TransportError::Timeout))
    )
}
