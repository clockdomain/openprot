// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! I3C client process.
//!
//! Stands in for the not-yet-written `services/i3c/client-ipc` crate: it drives
//! the transport protocol from `i3c_api` directly over a pw_kernel channel.
//!
//! Walks the full `dispatch()` surface of `//services/i3c/server`, in order:
//!
//! 1. `Recv` before any frame has arrived -> `NoData` (the empty latch).
//! 2. `DynamicAddress` -> the address the Caliptra ROM assigned.
//! 3. park in `object_wait(CHANNEL, USER)`, then `Recv` -> the host's payload.
//! 4. a second `Recv` -> `NoData` again, proving `Recv` consumed the latch.
//! 5. `Send` -> stages a TX the host collects with a private read.
//!
//! Step 3 is also the wake this test exists to exercise: the server raises USER
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

/// The payload the firmware stages for the host to collect by private read.
const REPLY: &[u8] = &[0xa5, 0x5a, 0xc3, 0x3c];

macro_rules! fail {
    ($msg:literal) => {{
        pw_log::error!($msg);
        let _ = syscall::debug_shutdown(Err(pw_status::Error::Internal));
        loop {}
    }};
}

/// One request/response round trip over the IPC channel.
///
/// Returns the status byte and the response payload length; the payload itself
/// is left in `resp` so the caller can inspect it without a borrow conflict.
fn transact(op: I3cOp, payload: &[u8], req: &mut [u8], resp: &mut [u8]) -> (I3cStatus, usize) {
    let Some(req_len) = encode_request(op, payload, req) else {
        fail!("encode_request failed");
    };
    let Ok(resp_len) =
        syscall::channel_transact(handle::CHANNEL, &req[..req_len], resp, Instant::MAX)
    else {
        fail!("channel_transact failed");
    };
    // `decode_response` borrows `resp`; copy out what we need and drop it.
    let Some((status, body)) = decode_response(&resp[..resp_len]) else {
        fail!("decode_response failed");
    };
    let body_len = body.len();
    // Move the body to the front so the caller can read it after the borrow ends.
    let header = resp_len - body_len;
    resp.copy_within(header..resp_len, 0);
    (status, body_len)
}

#[process_entry("client")]
fn entry() {
    let mut req = [0u8; MAX_FRAME];
    let mut resp = [0u8; MAX_FRAME];

    // 1. Nothing has arrived on the bus yet: the latch must read empty.
    let (status, _) = transact(I3cOp::Recv, &[], &mut req, &mut resp);
    if status != I3cStatus::NoData {
        fail!("expected NoData from Recv before any frame");
    }

    // 2. The ROM assigns the dynamic address during bus enumeration; the server
    //    reads it back out of the standby-controller registers.
    let (status, n) = transact(I3cOp::DynamicAddress, &[], &mut req, &mut resp);
    match status {
        I3cStatus::Ok if n == 1 => {
            pw_log::info!("i3c service: dynamic address {:#04x}", resp[0] as u32);
        }
        I3cStatus::Unassigned => fail!("dynamic address unassigned"),
        _ => fail!("unexpected status from DynamicAddress"),
    }

    pw_log::info!("i3c service: waiting for private write");

    // 3. Park until the server signals -- from its IRQ path -- that a frame has
    //    been latched. This is the cross-process wake under test.
    loop {
        if syscall::object_wait(handle::CHANNEL, Signals::USER, Instant::MAX).is_err() {
            fail!("object_wait failed");
        }

        let (status, n) = transact(I3cOp::Recv, &[], &mut req, &mut resp);
        match status {
            // USER raced ahead of the latch; park again rather than failing.
            I3cStatus::NoData => continue,
            I3cStatus::Ok => {
                if n < EXPECTED.len() || resp.get(..EXPECTED.len()) != Some(EXPECTED) {
                    pw_log::error!("i3c service: payload mismatch len={}", n as u32);
                    let _ = syscall::debug_shutdown(Err(pw_status::Error::DataLoss));
                    loop {}
                }
                break;
            }
            _ => fail!("unexpected status from Recv"),
        }
    }
    pw_log::info!("i3c service: received expected payload");

    // 4. `Recv` consumes the latch, so an immediate second read must be empty.
    let (status, _) = transact(I3cOp::Recv, &[], &mut req, &mut resp);
    if status != I3cStatus::NoData {
        fail!("expected NoData from second Recv: latch was not consumed");
    }

    // 5. Stage a transmit. The host collects it with a private read, which is
    //    what proves the Send path reached the hardware.
    let (status, _) = transact(I3cOp::Send, REPLY, &mut req, &mut resp);
    if status != I3cStatus::Ok {
        fail!("Send failed");
    }
    pw_log::info!("i3c service: staged reply for private read");

    let _ = syscall::debug_shutdown(Ok(()));
    loop {}
}
