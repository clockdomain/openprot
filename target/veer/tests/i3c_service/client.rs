// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! I3C client process.
//!
//! Stands in for the not-yet-written `services/i3c/client-ipc` crate: it drives
//! the transport protocol from `i3c_api` directly over a pw_kernel channel.
//!
//! The wake this test exists to exercise happens here. This process parks in
//! `object_wait(CHANNEL, USER)` with nothing else to do. The server raises USER
//! from inside its I3C interrupt handling, so the process made runnable by that
//! interrupt is a *different* process from the one that was interrupted.

#![no_main]
#![no_std]

use client_codegen::handle;
use i3c_api::{decode_response, encode_request, I3cOp, I3cStatus, MAX_FRAME};
use userspace::process_entry;
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

/// The host sends exactly this private-write payload.
const EXPECTED: &[u8] = &[0x01, 0x02, 0x03, 0x04];

macro_rules! fail {
    ($msg:literal) => {{
        pw_log::error!($msg);
        let _ = syscall::debug_shutdown(Err(pw_status::Error::Internal));
        loop {}
    }};
}

#[process_entry("client")]
fn entry() {
    pw_log::info!("i3c service: waiting for private write");

    let mut req = [0u8; MAX_FRAME];
    let mut resp = [0u8; MAX_FRAME];

    loop {
        // Park until the server signals -- from its IRQ path -- that a frame
        // has been latched. This is the wake under test.
        if syscall::object_wait(handle::CHANNEL, Signals::USER, Instant::MAX).is_err() {
            fail!("object_wait failed");
        }

        let Some(req_len) = encode_request(I3cOp::Recv, &[], &mut req) else {
            fail!("encode_request failed");
        };

        let Ok(resp_len) =
            syscall::channel_transact(handle::CHANNEL, &req[..req_len], &mut resp, Instant::MAX)
        else {
            fail!("channel_transact failed");
        };

        let Some((status, payload)) = decode_response(&resp[..resp_len]) else {
            fail!("decode_response failed");
        };

        match status {
            // No frame latched yet: the USER signal raced ahead of the latch.
            // Park again rather than treating it as a failure.
            I3cStatus::NoData => continue,
            I3cStatus::Ok => {
                if payload.len() >= EXPECTED.len() && &payload[..EXPECTED.len()] == EXPECTED {
                    pw_log::info!("i3c service: received expected payload");
                    let _ = syscall::debug_shutdown(Ok(()));
                    loop {}
                }
                pw_log::error!("i3c service: payload mismatch len={}", payload.len() as u32);
                let _ = syscall::debug_shutdown(Err(pw_status::Error::DataLoss));
                loop {}
            }
            _ => fail!("unexpected status"),
        }
    }
}
