// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! IPC front end for the PLDM notify channel.
//!
//! The **only** kernel-tagged piece of the notify server path; it wraps the
//! host-buildable `notify_server::dispatch` in the Pigweed channel calls, the
//! same way `i2c_server_runtime` wraps `i2c_server::dispatch`.
//!
//! ## Why this is not a `run()` loop
//!
//! `i2c_server_runtime::run` owns its process and parks in `object_wait`. This
//! one cannot: the PLDM process is already inside
//! [`FirmwareDevice::run_terminus`], which must stay free to service the
//! Update Agent over MCTP. So [`NotifyChannel`] plugs into the terminus loop
//! through the [`FdEventSink`] seam instead — [`notify`](FdEventSink::notify)
//! latches FD state-machine edges, and [`service`](FdEventSink::service) gets
//! one non-blocking pass at the orchestrator's channel per iteration.
//!
//! Both directions stay non-blocking on purpose. PLDM never waits on the
//! supervisor, and the supervisor's nudge is a dataless signal it never waits
//! on either; the only blocking direction in this design is
//! Orchestrator → PLDM, bounded, in `notify_client_ipc`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use notify_api::{
    NotifyOp, NotifyRequestHeader, Pending, MAX_BUF_SIZE, MAX_SERVICE_INTERVAL_MILLIS,
};
use notify_server::{dispatch, NotifyState};
use openprot_pldm_service::firmware_device::{FdEvent, FdEventSink};
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

/// A deadline already in the past: `object_wait` then reports what is pending
/// right now and returns instead of parking. Using a fixed past instant rather
/// than "now" avoids the one-tick window where a freshly sampled `now` is not
/// yet strictly exceeded and the call would block after all.
const POLL_NOW: Instant = Instant::from_ticks(0);

/// PLDM's side of the notify channel to one Orchestrator peer.
///
/// Owns the channel handle and the [`NotifyState`] the host-testable
/// `dispatch` operates on.
pub struct NotifyChannel {
    channel: u32,
    state: NotifyState,
}

impl NotifyChannel {
    /// Bind to the orchestrator's notify channel `channel` (from the app's
    /// generated `handle` module), with nothing latched and notifications
    /// disarmed until the peer subscribes.
    pub const fn new(channel: u32) -> Self {
        Self {
            channel,
            state: NotifyState::new(),
        }
    }

    /// Latch `pending` for the peer's next `Poll` and, if it has subscribed,
    /// nudge its `USER` signal.
    ///
    /// The nudge is dataless and level-triggered: it tells the supervisor
    /// there is something to come and get, and carries nothing itself. An
    /// event latched just before the peer's `object_wait` is not lost, because
    /// the signal is already asserted when it parks.
    ///
    /// Gated on `notify_armed` so an unsubscribed peer is never signalled —
    /// the latch still happens, so a peer that subscribes later still finds
    /// the event waiting.
    pub fn latch(&mut self, pending: Pending) {
        self.state.latch(pending);
        if self.state.notify_armed
            && syscall::object_set_peer_user_signal(self.channel, true).is_err()
        {
            pw_log::error!("notify: raising peer USER signal failed");
        }
    }

    /// Answer at most one orchestrator request, without blocking. Returns
    /// whether a request was answered.
    ///
    /// One per call rather than draining in a loop: the caller is the PLDM
    /// terminus loop, and an unbounded drain here would let a chatty
    /// supervisor delay the Update Agent's next command. The loop comes back
    /// every iteration, so a backlog clears at loop rate.
    pub fn service_once(&mut self) -> bool {
        // No logging between here and `channel_respond`: this window is inside
        // the peer's bounded deadline, and a console write under QEMU costs
        // more than the whole budget.
        if syscall::object_wait(self.channel, Signals::READABLE, POLL_NOW).is_err() {
            // DeadlineExceeded: nothing waiting. Any other error is equally
            // "no request to answer this pass".
            return false;
        }

        let mut request = [0u8; MAX_BUF_SIZE];
        let req_len = match syscall::channel_read(self.channel, 0, &mut request) {
            Ok(n) => n,
            Err(e) => {
                pw_log::error!("notify: channel_read failed: {}", e as u32);
                return false;
            }
        };
        let Some(req) = request.get(..req_len) else {
            return false;
        };

        // Clear USER at the TOP of a Poll, before `dispatch` drains the latch
        // (mirrors the i2c server-runtime's SlaveReceive ordering). Clearing
        // after the drain would open a window in which an event latched
        // in between is cleared away with the signal that announced it,
        // costing a wakeup. Clearing first is safe in the other direction: a
        // latch landing after this point re-raises USER, so the peer polls
        // again and finds it.
        if matches!(peek_op(req), Some(NotifyOp::Poll))
            && syscall::object_set_peer_user_signal(self.channel, false).is_err()
        {
            pw_log::error!("notify: clearing peer USER signal failed");
        }

        let mut response = [0u8; MAX_BUF_SIZE];
        let resp_len = dispatch(&mut self.state, req, &mut response);
        let Some(resp) = response.get(..resp_len) else {
            return false;
        };
        if let Err(e) = syscall::channel_respond(self.channel, resp) {
            // Carry the status: "failed" alone cannot distinguish a vanished
            // transaction from an oversized response.
            pw_log::error!(
                "notify: channel_respond failed: {} (req {} B, resp {} B)",
                e as u32,
                req_len as u32,
                resp_len as u32
            );
            return false;
        }
        true
    }
}

/// Read just the op code, to decide whether this request is the one that needs
/// its signal cleared before dispatch. Malformed input yields `None`; the
/// dispatch below rejects it properly.
fn peek_op(req: &[u8]) -> Option<NotifyOp> {
    let head = req.get(..NotifyRequestHeader::SIZE)?;
    let header = zerocopy::Ref::<_, NotifyRequestHeader>::from_bytes(head).ok()?;
    header.operation().ok()
}

/// Latches FD lifecycle edges as [`Pending`] events and services the
/// orchestrator's channel once per terminus-loop iteration.
///
/// [`FdEvent`] is `#[non_exhaustive]`; an edge with no `Pending` counterpart
/// is dropped rather than guessed at.
impl FdEventSink for NotifyChannel {
    fn notify(&mut self, event: FdEvent) {
        if matches!(event, FdEvent::UpdateRequested) {
            self.latch(Pending::UpdateRequested);
        }
    }

    fn service(&mut self) {
        let _ = self.service_once();
    }

    /// The supervisor is waiting on a bounded deadline, so cap the terminus
    /// loop's idle MCTP poll: without it the loop parks for the caller's full
    /// `timeout_millis` and a healthy FD looks dead.
    fn max_service_interval_millis(&self) -> Option<u32> {
        Some(MAX_SERVICE_INTERVAL_MILLIS)
    }
}
