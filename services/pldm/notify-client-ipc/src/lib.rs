// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Production IPC transport for `PldmLink`.
//!
//! The **only** IPC-coupled, kernel-tagged piece of the orchestrator's client
//! path. It implements `notify_api::Transport` over a Pigweed channel; all
//! wire marshalling stays in the host-buildable `notify_client`. Wiring:
//!
//! ```rust,ignore
//! use notify_client::PldmLink;
//! use notify_client_ipc::IpcTransport;
//! use userspace::time::Duration;
//! let mut pldm = PldmLink::new(IpcTransport::new(handle::PLDM_NOTIFY, Duration::from_millis(50)));
//! ```
//!
//! Swapping this for `notify_server::loopback::LoopbackTransport` (host)
//! exercises the same `PldmLink` code with no kernel — that is the point of
//! the seam.
//!
//! ## Why this is not `Instant::MAX`
//!
//! `i2c_client_ipc` parks on `Instant::MAX`, and that is correct *there*: an
//! i2c client has no deadline of its own to miss. The orchestrator does. It is
//! the supervisor for boot-progress watchdogs and the anti-rollback commit
//! window, and it reaches PLDM — the component that parses untrusted Update
//! Agent traffic — over this channel. An unbounded `channel_transact` would
//! subordinate those deadlines to a peer at the untrusted edge: a wedged PLDM
//! would stall the supervisor and its watchdogs would never fire. So every
//! round-trip here is bounded, and a deadline that elapses is reported as
//! [`TransportError::Timeout`] so the run loop can declare the peer unhealthy.
//! See `docs/src/design/orchestrator/pldm-orchestrator-ipc-alt.md`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use notify_api::{Transport, TransportError};
use pw_status::Error;
use userspace::syscall;
use userspace::time::{Clock, Duration, SystemClock};

/// Cross-process transport: one bounded `channel_transact` per round-trip.
pub struct IpcTransport {
    handle: u32,
    timeout: Duration,
}

impl IpcTransport {
    /// Bind to the notify channel `handle` (from the app's generated `handle`
    /// module), bounding every round-trip to `timeout`.
    ///
    /// `timeout` is the supervisor's patience with PLDM, not a transfer
    /// budget: no bulk data crosses this channel, so it should be sized for a
    /// healthy peer's turnaround, and exceeding it is evidence the peer is
    /// gone rather than merely busy.
    ///
    /// Size it at or above [`MIN_TRANSACT_TIMEOUT_MILLIS`], which allows for
    /// PLDM answering from its terminus loop up to
    /// [`MAX_SERVICE_INTERVAL_MILLIS`] later. A tighter deadline times out
    /// against a healthy peer that is merely parked on its MCTP poll, and the
    /// health verdict is one-way.
    ///
    /// [`MIN_TRANSACT_TIMEOUT_MILLIS`]: notify_api::MIN_TRANSACT_TIMEOUT_MILLIS
    /// [`MAX_SERVICE_INTERVAL_MILLIS`]: notify_api::MAX_SERVICE_INTERVAL_MILLIS
    pub const fn new(handle: u32, timeout: Duration) -> Self {
        Self { handle, timeout }
    }
}

impl Transport for IpcTransport {
    fn transact(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, TransportError> {
        // Fail closed: if the deadline cannot be represented we do not fall
        // back to an unbounded wait, we decline to make the call.
        let Some(deadline) = SystemClock::now().checked_add_duration(self.timeout) else {
            return Err(TransportError::Failed);
        };

        match syscall::channel_transact(self.handle, req, resp, deadline) {
            Ok(n) => Ok(n),
            // The bounded deadline elapsed: a silent peer, not a failed call.
            Err(Error::DeadlineExceeded) => Err(TransportError::Timeout),
            Err(_) => Err(TransportError::Failed),
        }
    }
}
