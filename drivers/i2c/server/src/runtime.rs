// Licensed under the Apache-2.0 license

//! Reusable I2C server runtime — wait_group + IPC dispatch loop.
//!
//! Same shape as [`drivers/usart/server/src/runtime.rs`](../../usart/server/src/runtime.rs):
//! the binary glue populates the wait_group, then calls [`run`].
//!
//! **Always-on notifications.** When the system image declares an I2C
//! IRQ object, the runtime's IRQ branch unconditionally:
//!   1. drains slave RX for `bus_id` into the backend's buffer
//!   2. raises `Signals::USER` on `notify_channel` (the IPC channel
//!      that the client is listening on)
//!   3. acks the IRQ
//!
//! Clients that want to be woken on RX activity do
//! `object_wait(channel, Signals::USER, …)` and then issue a
//! `SlaveReceive` IPC to retrieve the buffered bytes. The USER signal
//! is sticky until the client clears it (typically by reading and
//! reissuing the wait).
//!
//! Pass `notify_channel = 0` to skip the peer signal (no listener).

use i2c_api::wire::{MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE};
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

use crate::{I2cBackend, dispatch_request};

/// Run the I2C server dispatch loop forever.
///
/// The caller is responsible for populating `wg` ahead of time:
///
/// - For each IPC channel the binary serves, register it with its own
///   handle as `user_data`:
///   `wait_group_add(wg, ch, Signals::READABLE, ch as usize)`.
/// - If the system image declares an I2C IRQ, register it with
///   `irq_signals` and `irq` as its `user_data`:
///   `wait_group_add(wg, irq, irq_signals, irq as usize)`.
///   Pass `irq = 0` and `irq_signals = Signals::empty()` to disable
///   the IRQ branch entirely.
///
/// `bus_id` is the controller this server task owns — used to drain
/// slave RX on each IRQ.
///
pub fn run<B: I2cBackend>(
    backend: &mut B,
    wg: u32,
    irq: u32,
    irq_signals: Signals,
    bus_id: u8,
    notify_channel: u32,
) -> ! {
    let mut request_buf = [0u8; MAX_REQUEST_SIZE];
    let mut response_buf = [0u8; MAX_RESPONSE_SIZE];

    let wait_mask = Signals::READABLE | irq_signals;

    loop {
        let Ok(wait_return) = syscall::object_wait(wg, wait_mask, Instant::MAX) else {
            continue;
        };

        if irq != 0
            && wait_return.user_data as u32 == irq
            && wait_return.pending_signals.contains(irq_signals)
        {
            let acked = wait_return.pending_signals & irq_signals;
            // Always-on drain + notify: pull pending slave RX into the
            // backend buffer, raise USER on the listener channel so a
            // client blocked in `object_wait(channel, USER, …)` wakes,
            // then ack the IRQ.
            let _ = backend.drain_slave_rx(bus_id);
            if notify_channel != 0 {
                let _ = syscall::object_set_peer_user_signal(notify_channel, true);
            }
            let _ = syscall::interrupt_ack(irq, acked);
            continue;
        }

        if !wait_return.pending_signals.contains(Signals::READABLE) {
            continue;
        }

        let channel = wait_return.user_data as u32;
        let Ok(req_len) = syscall::channel_read(channel, 0, &mut request_buf) else {
            continue;
        };

        let resp_len = dispatch_request(backend, &request_buf[..req_len], &mut response_buf);
        let _ = syscall::channel_respond(channel, &response_buf[..resp_len]);
    }
}
