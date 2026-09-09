// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Orchestrator side of the notify-channel QEMU test.
//!
//! Runs the real client path — `PldmLink` marshalling over the bounded
//! `IpcTransport` — and the real policy code (`classify_poll`,
//! `NotifySupervisor`) against a live PLDM process on a live kernel. The host
//! tests already cover the mapping; what only a kernel can settle is whether
//! the seam underneath it behaves: that the nudge is not lost, that a
//! round-trip actually ends at its deadline, and that a silent peer is
//! condemned rather than waited on forever.
//!
//! Reports through `debug_shutdown(Ok|Err)`, which the target turns into the
//! `TEST_RESULT:` sentinel the QEMU runner greps for.

#![no_main]
#![no_std]

use app_notify_orch::handle;
use notify_api::{Decision, Phase, TransportError, MIN_TRANSACT_TIMEOUT_MILLIS};
use notify_client::{ClientError, PldmLink};
use notify_client_ipc::IpcTransport;
use openprot_orchestrator_pldm_adapter::notify::{classify_poll, NotifySupervisor, PollOutcome};
use openprot_orchestrator_sm::Event;
use userspace::entry;
use userspace::syscall::{self, Signals};
use userspace::time::{Clock, Duration, SystemClock};

/// The supervisor's patience with PLDM under QEMU.
///
/// Deliberately far above the protocol's own
/// [`MIN_TRANSACT_TIMEOUT_MILLIS`] (50 ms), which is sized for real silicon.
/// Emulated, a single round-trip is dominated by console writes and scheduling
/// rather than by the exchange: measured at roughly 200 ms for the first one.
/// Holding the test to the hardware number would only prove that QEMU is slow.
///
/// What the test still proves is the property that matters — that the deadline
/// is *bounded and enforced*: the silent-peer scenario below has to end at this
/// value rather than hang, whatever the value is.
const TRANSACT_TIMEOUT: Duration = Duration::from_millis(10 * MIN_TRANSACT_TIMEOUT_MILLIS as u64);

/// Generous upper bound on waiting for PLDM's nudge. Not a design value — if
/// the seam works the wait returns almost immediately, and this only keeps a
/// broken build from hanging until the runner's own timeout.
const NUDGE_TIMEOUT: Duration = Duration::from_millis(2000);

macro_rules! fail {
    ($msg:literal) => {{
        pw_log::error!($msg);
        let _ = syscall::debug_shutdown(Err(pw_status::Error::Internal));
        #[expect(clippy::empty_loop)]
        loop {}
    }};
}

#[entry]
fn entry() {
    let mut pldm = PldmLink::new(IpcTransport::new(handle::NOTIFY, TRANSACT_TIMEOUT));
    let mut supervisor = NotifySupervisor::new();

    // Registration part 1, kernel-level. USER carries PLDM's nudge. JOINABLE
    // is the process object: nothing restarts PLDM yet, but registering it
    // turns "the peer died" into a named failure instead of a hang, and it is
    // the hook restart-and-resubscribe will hang off.
    if syscall::wait_group_add(
        handle::WG,
        handle::NOTIFY,
        Signals::USER,
        handle::NOTIFY as usize,
    )
    .is_err()
    {
        fail!("wait_group_add(notify, USER) failed");
    }
    if syscall::wait_group_add(
        handle::WG,
        handle::PLDM_PROCESS,
        Signals::JOINABLE,
        handle::PLDM_PROCESS as usize,
    )
    .is_err()
    {
        fail!("wait_group_add(pldm_process, JOINABLE) failed");
    }

    // Registration part 2, protocol-level.
    let t0 = SystemClock::now().ticks();
    match pldm.subscribe() {
        Ok(()) => {}
        Err(e) => {
            let waited = SystemClock::now().ticks().saturating_sub(t0);
            match e {
                ClientError::Transport(TransportError::Timeout) => {
                    pw_log::error!(
                        "ORCH: subscribe hit its deadline after {} ticks",
                        waited as u32
                    )
                }
                ClientError::Transport(TransportError::Failed) => {
                    pw_log::error!(
                        "ORCH: subscribe transport failed after {} ticks",
                        waited as u32
                    )
                }
                ClientError::ServerError(_) => pw_log::error!("ORCH: subscribe rejected by server"),
                _ => pw_log::error!("ORCH: subscribe got a malformed response"),
            }
            fail!("subscribe failed");
        }
    }
    pw_log::info!("ORCH: subscribed");

    // Nothing latched yet.
    if !matches!(classify_poll(pldm.poll()), PollOutcome::Idle) {
        fail!("expected an idle poll before PLDM latched anything");
    }

    // PLDM latches immediately after answering that poll, so the signal may
    // already be asserted before this call parks. Returning promptly either
    // way is the level-triggered guarantee holding.
    let deadline = SystemClock::now() + NUDGE_TIMEOUT;
    let woke = match syscall::object_wait(handle::WG, Signals::USER | Signals::JOINABLE, deadline) {
        Ok(w) => w,
        Err(_) => fail!("no nudge from PLDM before the deadline"),
    };
    if woke.user_data == handle::PLDM_PROCESS as usize {
        fail!("PLDM process exited instead of nudging");
    }
    pw_log::info!("ORCH: nudged");

    // Drain it and check what the run loop would actually do with it.
    let reaction = supervisor.on_poll(classify_poll(pldm.poll()));
    if reaction.dispatch != Some(Event::UpdateRequest) {
        fail!("the nudge did not drain as UpdateRequest");
    }
    if !reaction.rearm {
        fail!("a healthy peer must keep the poll cadence armed");
    }

    // Answer the veto, then report a phase — both are Orchestrator -> PLDM
    // round-trips over the same bounded transport.
    if pldm.decide(Decision::Accepted).is_err() {
        fail!("decide(Accepted) failed");
    }
    if pldm.push_status(Phase::Staging).is_err() {
        fail!("push_status(Staging) failed");
    }
    pw_log::info!("ORCH: decided and reported");

    // PLDM is silent from here. This call reaching its deadline at all is the
    // property under test — an unbounded transact would hang the supervisor
    // and this test would time out instead of failing.
    let reaction = supervisor.on_poll(classify_poll(pldm.poll()));
    if supervisor.is_healthy() {
        fail!("a silent peer must be declared unhealthy");
    }
    if reaction.dispatch != Some(Event::UpdateRejected) {
        fail!("condemning the peer must unwind the in-flight update");
    }
    if reaction.rearm {
        fail!("a condemned peer must not be polled again");
    }
    pw_log::info!("ORCH: silent peer condemned, update unwound");

    pw_log::info!("notify channel QEMU test PASSED");
    let _ = syscall::debug_shutdown(Ok(()));
    #[expect(clippy::empty_loop)]
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    #[expect(clippy::empty_loop)]
    loop {}
}
