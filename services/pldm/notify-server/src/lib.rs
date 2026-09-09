// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Server side of the PLDM notify channel.
//!
//! [`dispatch`] is a pure function (no IPC) generic over nothing but
//! [`NotifyState`], so it is unit-testable on the host and reused unchanged
//! by the kernel-tagged `notify-server-runtime` (Phase 2), which wraps it in
//! the Pigweed wait/respond loop and raises `Signals::USER` on latch.

#![no_std]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]
// Tests use .unwrap() on zerocopy::Ref of fixed-size buffers we just wrote —
// safe by construction, but clippy can't see that.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod loopback;

use notify_api::{
    Decision, NotifyError, NotifyOp, NotifyRequestHeader, NotifyResponseHeader, Pending, Phase,
};

/// PLDM-side state for one Orchestrator peer: whether it has subscribed, and
/// the (at most one) latched event awaiting `Poll`.
#[derive(Default)]
pub struct NotifyState {
    pub notify_armed: bool,
    pub latched: Option<Pending>,
}

impl NotifyState {
    pub const fn new() -> Self {
        Self {
            notify_armed: false,
            latched: None,
        }
    }

    /// Latch `pending` for the next `Poll` to drain. Called by the (Phase 2)
    /// terminus loop when a UA event lands; PLDM raises the peer signal
    /// alongside this, gated on `notify_armed`.
    pub fn latch(&mut self, pending: Pending) {
        self.latched = Some(pending);
    }
}

fn encode_error(response: &mut [u8], err: NotifyError) -> usize {
    let hdr = NotifyResponseHeader::error(err);
    response[..NotifyResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    NotifyResponseHeader::SIZE
}

fn encode_ok(response: &mut [u8], payload_len: usize) -> usize {
    let hdr = NotifyResponseHeader::success(payload_len as u16);
    response[..NotifyResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    NotifyResponseHeader::SIZE + payload_len
}

/// Decode one wire request, apply it to `state`, and encode the response
/// into `response`. Returns the number of bytes written (always
/// `>= NotifyResponseHeader::SIZE`). Never panics on malformed input.
pub fn dispatch(state: &mut NotifyState, request: &[u8], response: &mut [u8]) -> usize {
    if request.len() < NotifyRequestHeader::SIZE {
        return encode_error(response, NotifyError::InvalidOperation);
    }
    let Ok(hdr) =
        zerocopy::Ref::<_, NotifyRequestHeader>::from_bytes(&request[..NotifyRequestHeader::SIZE])
    else {
        return encode_error(response, NotifyError::InvalidOperation);
    };
    let Ok(op) = hdr.operation() else {
        return encode_error(response, NotifyError::InvalidOperation);
    };
    let payload = &request[NotifyRequestHeader::SIZE..];

    match op {
        NotifyOp::Subscribe => {
            state.notify_armed = true;
            encode_ok(response, 0)
        }
        NotifyOp::Poll => {
            // Clear-before-drain: `take` clears the latch and yields its value
            // as one atomic step, so there is no window where a fresh event
            // could be silently dropped between "clear" and "drain" (mirrors
            // the i2c server-runtime's clear-USER-before-drain ordering).
            match state.latched.take() {
                Some(pending) => {
                    let mut buf = [0u8; Pending::MAX_ENCODED_LEN];
                    match pending.encode(&mut buf) {
                        Ok(n) if NotifyResponseHeader::SIZE + n <= response.len() => {
                            response[NotifyResponseHeader::SIZE..NotifyResponseHeader::SIZE + n]
                                .copy_from_slice(&buf[..n]);
                            encode_ok(response, n)
                        }
                        Ok(_) => encode_error(response, NotifyError::BufferTooSmall),
                        Err(e) => encode_error(response, e),
                    }
                }
                None => encode_error(response, NotifyError::NoPending),
            }
        }
        NotifyOp::Decision => match payload.first().copied().map(Decision::try_from) {
            Some(Ok(_decision)) => encode_ok(response, 0),
            Some(Err(e)) => encode_error(response, e),
            None => encode_error(response, NotifyError::InvalidOperation),
        },
        NotifyOp::PushStatus => match payload.first().copied().map(Phase::try_from) {
            Some(Ok(_phase)) => encode_ok(response, 0),
            Some(Err(e)) => encode_error(response, e),
            None => encode_error(response, NotifyError::InvalidOperation),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(
        state: &mut NotifyState,
        op: NotifyOp,
        payload: &[u8],
    ) -> (NotifyResponseHeader, [u8; 32]) {
        let hdr = NotifyRequestHeader::new(op, payload.len() as u16);
        let mut req = [0u8; 32];
        req[..NotifyRequestHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
        req[NotifyRequestHeader::SIZE..NotifyRequestHeader::SIZE + payload.len()]
            .copy_from_slice(payload);
        let req_len = NotifyRequestHeader::SIZE + payload.len();

        let mut resp = [0u8; 32];
        let n = dispatch(state, &req[..req_len], &mut resp);
        let rhdr = *zerocopy::Ref::<_, NotifyResponseHeader>::from_bytes(
            &resp[..NotifyResponseHeader::SIZE],
        )
        .unwrap();
        assert!(n >= NotifyResponseHeader::SIZE);
        (rhdr, resp)
    }

    #[test]
    fn subscribe_arms_and_acks() {
        let mut state = NotifyState::new();
        let (rhdr, _) = call(&mut state, NotifyOp::Subscribe, &[]);
        assert!(rhdr.is_success());
        assert!(state.notify_armed);
    }

    #[test]
    fn poll_drains_latched_event_then_reports_no_pending() {
        let mut state = NotifyState::new();
        state.latch(Pending::UpdateRequested);

        let (rhdr, resp) = call(&mut state, NotifyOp::Poll, &[]);
        assert!(rhdr.is_success());
        let n = rhdr.payload_length();
        assert_eq!(
            Pending::decode(&resp[NotifyResponseHeader::SIZE..NotifyResponseHeader::SIZE + n]),
            Ok(Pending::UpdateRequested)
        );

        let (rhdr, _) = call(&mut state, NotifyOp::Poll, &[]);
        assert!(!rhdr.is_success());
        assert_eq!(rhdr.error_code(), Some(NotifyError::NoPending));
    }

    #[test]
    fn decision_and_push_status_validate_and_ack() {
        let mut state = NotifyState::new();
        let (rhdr, _) = call(&mut state, NotifyOp::Decision, &[Decision::Accepted as u8]);
        assert!(rhdr.is_success());

        let (rhdr, _) = call(&mut state, NotifyOp::PushStatus, &[Phase::Staging as u8]);
        assert!(rhdr.is_success());

        let (rhdr, _) = call(&mut state, NotifyOp::Decision, &[0xFF]);
        assert_eq!(rhdr.error_code(), Some(NotifyError::InvalidOperation));
    }

    #[test]
    fn short_request_is_rejected_not_panicked() {
        let mut state = NotifyState::new();
        let mut resp = [0u8; 32];
        let n = dispatch(&mut state, &[0u8; 1], &mut resp);
        assert_eq!(n, NotifyResponseHeader::SIZE);
        let rhdr = zerocopy::Ref::<_, NotifyResponseHeader>::from_bytes(
            &resp[..NotifyResponseHeader::SIZE],
        )
        .unwrap();
        assert_eq!(rhdr.error_code(), Some(NotifyError::InvalidOperation));
    }
}
