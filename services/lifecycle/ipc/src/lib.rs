// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! # Lifecycle IPC Event Queue
//!
//! The on-target [`EventQueue`] for the lifecycle state machine, backed by a
//! `pw_kernel` IPC channel. This is the concrete binding the run-loop in
//! `openprot_lifecycle_sm` is generic over: it turns "block until the next
//! lifecycle event" into a channel wait + read, and "emit a follow-up event"
//! into a local buffer drained ahead of the next channel read.
//!
//! It composes two layers:
//!
//! - the generic, host-tested wire codec and in-process buffer from
//!   `openprot_ipc_event_queue` ([`WireCodec`], [`PendingQueue`]); and
//! - the `pw_kernel` `userspace` syscalls (`object_wait`, `channel_read`) that
//!   only build on-target.
//!
//! Because the syscall layer is target-only, this crate is target-only too; its
//! host-testable logic (the `Event` ↔ bytes mapping) is covered by unit tests
//! that exercise the [`WireCodec`] impl directly, with no channel.
//!
//! ## Event routing
//!
//! Follow-up events a handler emits (e.g. [`Event::VerifyDone`]) are produced
//! inside the lifecycle task; [`push`](IpcEventQueue::push) buffers them in the
//! [`PendingQueue`] rather than round-tripping them through the kernel.
//! [`recv`](IpcEventQueue::recv) drains that buffer first, and only blocks on
//! the channel once it is empty — so external producers (commands, watchdog,
//! reset IRQ) reach the machine through the channel while a handler's own
//! follow-up is serviced without a syscall.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use openprot_ipc_event_queue::PendingQueue;
use openprot_lifecycle_api::wire;
use openprot_lifecycle_sm::{Event, EventQueue};
use pw_status::Error as PwError;
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

/// Maximum number of un-drained follow-up events buffered locally.
///
/// A lifecycle handler emits at most one follow-up event per step, so a single
/// slot would suffice; the small headroom tolerates an external injection
/// landing between a follow-up being pushed and drained.
const PENDING_CAPACITY: usize = 4;

/// Size of the buffer used to read one encoded event off the channel.
///
/// An [`Event`] encodes to a few bytes (see [`wire`]); this leaves ample room
/// for a versioned or extended encoding without re-tuning callers.
const READ_BUFFER_LEN: usize = 16;

/// An [`EventQueue`] backed by a `pw_kernel` IPC channel.
///
/// `recv` blocks on the channel's `USER` signal (the `K_FOREVER` semantics of
/// the original `k_fifo_get`) until a producer makes an event available;
/// `push` buffers handler follow-ups in process.
pub struct IpcEventQueue {
    /// Channel handle to wait on and read inbound events from.
    ipc_handle: u32,
    /// In-process buffer of follow-up events, drained before the channel.
    pending: PendingQueue<Event, PENDING_CAPACITY>,
}

impl IpcEventQueue {
    /// Create a queue over the given `pw_kernel` channel handle.
    ///
    /// The handle is the codegen constant for the lifecycle task's inbound
    /// channel (e.g. `lifecycle_codegen::handle::LIFECYCLE_IPC_HANDLER`).
    pub const fn new(ipc_handle: u32) -> Self {
        Self {
            ipc_handle,
            pending: PendingQueue::new(),
        }
    }

    /// Block on the channel until one event can be read and decoded.
    ///
    /// Loops past spurious wakeups and undecodable messages: an unrecognized or
    /// truncated message is *discarded* (it is not a fatal channel error,
    /// mirroring the "discard the event and stay put" semantics of the
    /// transition table) and the wait is retried. Only a genuine syscall
    /// failure returns `Err`.
    fn recv_from_channel(&mut self) -> Result<Event, PwError> {
        loop {
            // K_FOREVER: block until a producer signals an event is available.
            syscall::object_wait(self.ipc_handle, Signals::USER, Instant::MAX)?;

            let mut buf = [0u8; READ_BUFFER_LEN];
            let len = syscall::channel_read(self.ipc_handle, 0, &mut buf[..])?;

            match wire::decode_event(&buf[..len]) {
                Ok(event) => return Ok(event),
                // Truncated, unknown tag, or any other codec error: drop the
                // message and wait for the next, mirroring the transition
                // table's "discard the event and stay put" semantics.
                Err(_) => continue,
            }
        }
    }
}

impl EventQueue for IpcEventQueue {
    type Error = PwError;

    fn recv(&mut self) -> Result<Event, PwError> {
        // Drain locally-produced follow-ups before blocking on the channel, so
        // a handler's own outcome is serviced ahead of any external event.
        if let Some(event) = self.pending.pop() {
            return Ok(event);
        }
        self.recv_from_channel()
    }

    fn push(&mut self, event: Event) -> Result<(), PwError> {
        // Follow-ups are produced in-process; buffer rather than round-trip
        // through the kernel. A full buffer means more follow-ups are pending
        // than the machine can emit per step, which is a logic invariant
        // violation rather than a transport fault.
        self.pending.push(event).map_err(|_| PwError::ResourceExhausted)
    }

    fn try_recv(&mut self) -> Result<Option<Event>, PwError> {
        // Only locally-buffered follow-ups are returned without blocking;
        // inbound channel events are left for `recv` so a driver running the
        // machine to quiescence does not consume an external event mid-request.
        Ok(self.pending.pop())
    }
}
