// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! MCTP server process, MCTP-over-I3C transport.
//!
//! The I3C analog of the ast10x0 `tests/mctp/server/main.rs` IPC dispatch loop:
//! it serves MCTP app clients over a Pigweed channel and carries their traffic
//! over I3C via `//services/mctp/transport-i3c`. Two event sources:
//!
//!   user_data=0  MCTP channel READABLE  -> a client request
//!   user_data=1  I3C channel USER       -> an inbound frame the i3c server
//!                                          latched from its interrupt path
//!
//! Unlike the I2C server this process does no slave configuration: the i3c
//! server owns and enables the hardware. It only sends (staging a private-read
//! response) and receives (collecting a latched private write).

#![no_main]
#![no_std]

use i3c_client::I3cClient;
use i3c_client_ipc::IpcTransport;
use mctp_server_codegen::handle;
use openprot_mctp_api::wire::{
    self, MctpOp, MctpRequestHeader, MAX_PAYLOAD_SIZE, MAX_REQUEST_SIZE, MAX_RESPONSE_SIZE,
};
use openprot_mctp_api::{Handle, ResponseCode};
use openprot_mctp_server::dispatch::{self, DispatchOutcome};
use openprot_mctp_transport_i3c::{I3cSender, MctpI3cReceiver};
use pw_status::Result;
use userspace::process_entry;
use userspace::syscall::{self, Signals};
use userspace::time::{Clock, Duration, Instant, SystemClock};

const OWN_EID: u8 = 8;
/// The controller's 7-bit I3C dynamic address — the link-layer destination for
/// staged responses. Must match the source address the host uses in its
/// MCTP-over-I3C requests.
const REMOTE_I3C_ADDR: u8 = 0x0a;
/// I3C already protects the transfer at the link layer; the host harness still
/// appends a PEC, so decode/encode with PEC on.
const PEC: bool = true;
const I3C_RX_MAX: usize = i3c_api::MAX_PAYLOAD;

fn mctp_server_loop() -> Result<()> {
    pw_log::info!("MCTP-over-I3C server starting");

    let sender = I3cSender::new(
        I3cClient::new(IpcTransport::new(handle::I3C)),
        REMOTE_I3C_ADDR,
        PEC,
    );
    // A second client on the same i3c channel, used to collect inbound frames.
    let mut i3c_rx_client = I3cClient::new(IpcTransport::new(handle::I3C));
    let i3c_receiver = MctpI3cReceiver::new(PEC);

    let mut server = openprot_mctp_server::Server::<_, 16>::new(mctp::Eid(OWN_EID), 0, sender);

    let mut request_buf = [0u8; MAX_REQUEST_SIZE];
    let mut response_buf = [0u8; MAX_RESPONSE_SIZE];
    let mut recv_buf = [0u8; MAX_PAYLOAD_SIZE];
    let mut i3c_rx_buf = [0u8; I3C_RX_MAX];

    struct PendingRecv {
        handle: Handle,
        deadline: Instant,
    }
    let mut pending_recv: Option<PendingRecv> = None;

    // user_data=0 -> IPC from an MCTP client; user_data=1 -> inbound i3c frame.
    syscall::wait_group_add(handle::WG, handle::MCTP, Signals::READABLE, 0usize)?;
    syscall::wait_group_add(handle::WG, handle::I3C, Signals::USER, 1usize)?;

    loop {
        let wait_deadline = pending_recv
            .as_ref()
            .map(|pending| pending.deadline)
            .unwrap_or(Instant::MAX);
        let ev = match syscall::object_wait(
            handle::WG,
            Signals::READABLE | Signals::USER,
            wait_deadline,
        ) {
            Ok(ev) => ev,
            Err(pw_status::Error::DeadlineExceeded) => {
                if pending_recv.take().is_some() {
                    let resp = wire::MctpResponseHeader::error(ResponseCode::TimedOut);
                    response_buf[..wire::MctpResponseHeader::SIZE]
                        .copy_from_slice(&resp.to_bytes());
                    let _ = syscall::channel_respond(
                        handle::MCTP,
                        &response_buf[..wire::MctpResponseHeader::SIZE],
                    );
                    let _ = syscall::wait_group_add(
                        handle::WG,
                        handle::MCTP,
                        Signals::READABLE,
                        0usize,
                    );
                }
                continue;
            }
            Err(err) => return Err(err),
        };

        if ev.user_data == 1 {
            // Inbound i3c frame: collect the latched private write and feed the
            // decoded MCTP packet to the router.
            match i3c_rx_client.recv(&mut i3c_rx_buf) {
                Ok(Some(n)) => {
                    if let Ok((pkt, _)) = i3c_receiver.decode(&i3c_rx_buf[..n]) {
                        let _ = server.inbound(pkt);
                    } else {
                        pw_log::error!("i3c frame decode failed");
                    }
                }
                Ok(None) => {}
                Err(_) => pw_log::error!("i3c recv failed"),
            }
            // Satisfy a deferred blocking-recv now that inbound data was processed.
            if let Some(pending) = pending_recv.as_ref() {
                if let Some(meta) = server.try_recv(pending.handle, &mut recv_buf) {
                    let payload = &recv_buf[..meta.payload_size];
                    let response_len = wire::encode_recv_response(
                        &mut response_buf,
                        meta.msg_type,
                        meta.msg_ic,
                        meta.remote_eid,
                        meta.msg_tag,
                        payload,
                    )
                    .unwrap_or_else(|_| {
                        wire::encode_error_response(&mut response_buf, ResponseCode::InternalError)
                            .unwrap_or(0)
                    });
                    syscall::channel_respond(handle::MCTP, &response_buf[..response_len])?;
                    pending_recv = None;
                    syscall::wait_group_add(handle::WG, handle::MCTP, Signals::READABLE, 0usize)?;
                }
            }
        } else {
            let len = syscall::channel_read(handle::MCTP, 0, &mut request_buf)?;
            if pending_recv.is_some() {
                let resp = wire::MctpResponseHeader::error(ResponseCode::InternalError);
                response_buf[..wire::MctpResponseHeader::SIZE].copy_from_slice(&resp.to_bytes());
                syscall::channel_respond(
                    handle::MCTP,
                    &response_buf[..wire::MctpResponseHeader::SIZE],
                )?;
                continue;
            }

            if len < MctpRequestHeader::SIZE {
                let resp = wire::MctpResponseHeader::error(ResponseCode::BadArgument);
                response_buf[..wire::MctpResponseHeader::SIZE].copy_from_slice(&resp.to_bytes());
                syscall::channel_respond(
                    handle::MCTP,
                    &response_buf[..wire::MctpResponseHeader::SIZE],
                )?;
                continue;
            }

            // Recv op: try immediately; defer the response if no message is ready.
            if MctpRequestHeader::from_bytes(&request_buf[..len])
                .and_then(|h| h.operation())
                .is_some_and(|op| matches!(op, MctpOp::Recv))
            {
                let header = MctpRequestHeader::from_bytes(&request_buf[..len]).unwrap();
                let recv_handle = Handle(header.handle);
                let payload = wire::get_request_payload(&request_buf[..len]);
                if payload.len() < 4 {
                    let resp = wire::MctpResponseHeader::error(ResponseCode::BadArgument);
                    response_buf[..wire::MctpResponseHeader::SIZE]
                        .copy_from_slice(&resp.to_bytes());
                    syscall::channel_respond(
                        handle::MCTP,
                        &response_buf[..wire::MctpResponseHeader::SIZE],
                    )?;
                    continue;
                }

                let timeout_millis = u32::from_le_bytes(payload[..4].try_into().unwrap());
                match server.try_recv(recv_handle, &mut recv_buf) {
                    Some(meta) => {
                        let payload = &recv_buf[..meta.payload_size];
                        let response_len = wire::encode_recv_response(
                            &mut response_buf,
                            meta.msg_type,
                            meta.msg_ic,
                            meta.remote_eid,
                            meta.msg_tag,
                            payload,
                        )
                        .unwrap_or_else(|_| {
                            wire::encode_error_response(
                                &mut response_buf,
                                ResponseCode::InternalError,
                            )
                            .unwrap_or(0)
                        });
                        syscall::channel_respond(handle::MCTP, &response_buf[..response_len])?;
                    }
                    None => {
                        let deadline = if timeout_millis == 0 {
                            Instant::MAX
                        } else {
                            SystemClock::now()
                                .checked_add_duration(Duration::from_millis(timeout_millis as u64))
                                .unwrap_or(Instant::MAX)
                        };
                        pending_recv = Some(PendingRecv {
                            handle: recv_handle,
                            deadline,
                        });
                        let _ = syscall::wait_group_remove(handle::WG, handle::MCTP);
                    }
                }
            } else {
                let response_len = match dispatch::dispatch_mctp_op(
                    &request_buf[..len],
                    &mut response_buf,
                    &mut server,
                    &mut recv_buf,
                    0,
                ) {
                    DispatchOutcome::Reply(n) => n,
                    DispatchOutcome::Pending { .. } => unreachable!("Recv handled above"),
                };
                syscall::channel_respond(handle::MCTP, &response_buf[..response_len])?;
            }
        }
    }
}

#[process_entry("mctp_server")]
fn entry() {
    if let Err(e) = mctp_server_loop() {
        pw_log::error!("mctp_server exiting with error");
        let _ = syscall::debug_shutdown(Err(e));
    }
    #[expect(clippy::empty_loop)]
    loop {}
}
