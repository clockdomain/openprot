// Licensed under the Apache-2.0 license

//! MCTP Server — IPC Dispatch Loop
//!
//! Userspace service that receives MCTP requests over a Pigweed IPC channel,
//! dispatches them to the MCTP server core, and responds with results.
//!
//! # Architecture
//!
//! ```text
//! ┌─ Client ──────────────────────────┐
//! │ channel_transact(request)         │
//! └──────────────┬────────────────────┘
//!                │ IPC channel
//!                ▼
//! ┌─ This Server ─────────────────────┐
//! │ object_wait(READABLE)             │
//! │ channel_read → MctpRequestHeader  │
//! │ dispatch_mctp_op(op, server)      │
//! │ channel_respond ← MctpRespHeader  │
//! └──────────────┬────────────────────┘
//!                │ mctp-stack Router
//!                ▼
//! ┌─ I2C Transport ──────────────────┐
//! │ I2cSender → I2C Server IPC      │
//! └──────────────────────────────────┘
//! ```
//!
//! # IPC Pattern
//!
//! Follows the same loop as `services/i2c/server/src/main.rs`:
//!
//! 1. `object_wait(handle, READABLE)` — block until a client sends a request
//! 2. `channel_read(handle)` — read the raw request bytes
//! 3. Parse `MctpRequestHeader`, dispatch via `dispatch_mctp_op`
//! 4. `channel_respond(handle)` — send response header + data
//!
//! # Handle Binding
//!
//! The IPC handle is provided by the `app_package` Bazel rule, which generates
//! `app_mctp_server::handle::MCTP` from the system configuration.

#![no_main]
#![no_std]

use openprot_mctp_api::wire::{MctpRequestHeader, MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE, MAX_PAYLOAD_SIZE};
use openprot_mctp_api::ResponseCode;
use openprot_mctp_server::dispatch;

use pw_status::Result;
use userspace::entry;
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

use app_mctp_server::handle;

// ---------------------------------------------------------------------------
// Server loop
// ---------------------------------------------------------------------------

fn mctp_server_loop() -> Result<()> {
    pw_log::info!("MCTP server starting");

    // TODO(Phase 6): Initialize transport binding.
    // The I2C sender needs an IpcI2cClient bound to the I2C server channel.
    // For now, we create a stub sender that will be replaced with a real
    // I2cSender<IpcI2cClient> once the I2C channel handle is wired up.
    //
    // let i2c_client = i2c_client::IpcI2cClient::new(handle::I2C);
    // let sender = I2cSender::new(i2c_client, BusIndex::BUS_0, OWN_I2C_ADDR);
    // let mut server = Server::new(Eid(OWN_EID), 0, sender);

    // For initial bring-up, use a no-op sender so the IPC dispatch loop
    // can be tested without I2C hardware.
    let mut server = openprot_mctp_server::Server::<NoopSender, 16>::new(
        mctp::Eid(8), // default EID
        0,
        NoopSender,
    );

    let mut request_buf = [0u8; MAX_REQUEST_SIZE];
    let mut response_buf = [0u8; MAX_RESPONSE_SIZE];
    let mut recv_buf = [0u8; MAX_PAYLOAD_SIZE];

    loop {
        // Block until a client sends a request
        syscall::object_wait(handle::MCTP, Signals::READABLE, Instant::MAX)?;

        // Read the request
        let len = syscall::channel_read(handle::MCTP, 0, &mut request_buf)?;

        if len < MctpRequestHeader::SIZE {
            // Truncated request — respond with error
            let resp = openprot_mctp_api::wire::MctpResponseHeader::error(ResponseCode::BadArgument);
            response_buf[..openprot_mctp_api::wire::MctpResponseHeader::SIZE]
                .copy_from_slice(&resp.to_bytes());
            syscall::channel_respond(
                handle::MCTP,
                &response_buf[..openprot_mctp_api::wire::MctpResponseHeader::SIZE],
            )?;
            continue;
        }

        // Dispatch and respond
        let response_len = dispatch::dispatch_mctp_op(
            &request_buf[..len],
            &mut response_buf,
            &mut server,
            &mut recv_buf,
        );
        syscall::channel_respond(handle::MCTP, &response_buf[..response_len])?;
    }
}

// ---------------------------------------------------------------------------
// No-op sender for initial bring-up
// ---------------------------------------------------------------------------

/// Stub sender that does nothing. Used for IPC dispatch testing before
/// the I2C transport is wired up.
struct NoopSender;

impl mctp_lib::Sender for NoopSender {
    fn send_vectored(
        &mut self,
        _fragmenter: mctp_lib::fragment::Fragmenter,
        _payload: &[&[u8]],
    ) -> mctp::Result<mctp::Tag> {
        Err(mctp::Error::TxFailure)
    }

    fn get_mtu(&self) -> usize {
        64
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[entry]
fn entry() -> ! {
    if let Err(e) = mctp_server_loop() {
        pw_log::error!("MCTP server error: {}", e as u32);
        let _ = syscall::debug_shutdown(Err(e));
    }
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
