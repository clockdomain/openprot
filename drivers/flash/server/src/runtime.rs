// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

use flash_api::backend::FlashBackend;
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

use crate::{DispatchOutcome, MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE, dispatch_request};

/// Run the flash server dispatch loop forever.
///
/// The caller is responsible for populating `wg` ahead of time. The
/// convention this runtime relies on, identical to the usart server:
///
/// - For each IPC channel the binary serves, register it with its own
///   handle as `user_data`:
///   `wait_group_add(wg, ch, Signals::READABLE, ch as usize)`.
///
/// The loop routes wake-ups using `wait_return.user_data` as the channel
/// handle. Adding another client task is one more `wait_group_add` call
/// in the binary.
///
/// Note: flash v1 has no IRQ-driven completion path. Backends that
/// cannot complete synchronously should return
/// `BackendError::WouldBlock`; the response carries `FlashError::WouldBlock`
/// and the client is responsible for retrying.
pub fn run<B: FlashBackend>(backend: &mut B, wg: u32) -> ! {
    let mut request_buf = [0u8; MAX_REQUEST_SIZE];
    let mut response_buf = [0u8; MAX_RESPONSE_SIZE];

    loop {
        let Ok(wait_return) = syscall::object_wait(wg, Signals::READABLE, Instant::MAX) else {
            continue;
        };

        if !wait_return.pending_signals.contains(Signals::READABLE) {
            continue;
        }

        let channel = wait_return.user_data as u32;
        let Ok(req_len) = syscall::channel_read(channel, 0, &mut request_buf) else {
            continue;
        };

        match dispatch_request(backend, &request_buf[..req_len], &mut response_buf) {
            DispatchOutcome::Respond(resp_len) => {
                let _ = syscall::channel_respond(channel, &response_buf[..resp_len]);
            }
        }
    }
}
