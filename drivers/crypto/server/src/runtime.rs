// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Crypto server dispatch loop.
//!
//! Strictly simpler than the USART runtime: crypto ops are whole-object and
//! run-to-completion, so there is NO `PendingRead`, NO `DispatchOutcome`, and
//! NO IRQ-park branch (sketch §3.1, §3.2). The single-threaded loop processing
//! one request to completion before the next *is* the serialization that
//! replaces the reference's busy flag, and it also serialises the symmetric vs
//! public-key class — both backends are owned by this one loop.

use crypto_api::backend::{PublicKeyBackend, SymmetricBackend};
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

use crate::{MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE, dispatch_request};

/// Run the crypto server dispatch loop forever.
///
/// The caller is responsible for populating `wg` ahead of time. The
/// convention this runtime relies on:
///
/// - For each IPC channel the binary serves, register it with its own
///   handle as `user_data`:
///   `wait_group_add(wg, ch, Signals::READABLE, ch as usize)`.
///
/// The loop then routes wake-ups using `wait_return.user_data` directly:
/// every wake-up is a channel and `user_data` is the channel handle to
/// read/respond on. One `crypto` channel carries BOTH capability classes —
/// routing to the symmetric vs public-key backend happens inside
/// `dispatch_request` by `header.engine`, not at the transport. This keeps
/// the runtime topology-agnostic — adding another client task is one more
/// `wait_group_add` call in the binary.
///
/// Unlike the USART runtime there is no IRQ branch: completion (including the
/// bounded engine poll) happens entirely inside `dispatch_request`, and the
/// engine is released before `channel_respond`.
pub fn run<Y: SymmetricBackend, P: PublicKeyBackend>(
    sym: &mut Y,
    pk: &mut P,
    wg: u32,
) -> ! {
    let mut request_buf = [0u8; MAX_REQUEST_SIZE];
    let mut response_buf = [0u8; MAX_RESPONSE_SIZE];

    let wait_mask = Signals::READABLE;

    loop {
        let Ok(wait_return) = syscall::object_wait(wg, wait_mask, Instant::MAX) else {
            continue;
        };

        if !wait_return.pending_signals.contains(Signals::READABLE) {
            continue;
        }

        let channel = wait_return.user_data as u32;
        let Ok(req_len) = syscall::channel_read(channel, 0, &mut request_buf) else {
            continue;
        };

        let resp_len = dispatch_request(sym, pk, &request_buf[..req_len], &mut response_buf);

        // Run-to-completion: there is always exactly one response.
        let _ = syscall::channel_respond(channel, &response_buf[..resp_len]);
    }
}
