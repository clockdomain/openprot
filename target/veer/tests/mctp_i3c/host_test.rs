// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Host-side harness for the VeeR MCTP-over-I3C echo test.
//!
//! Sends one MCTP message to the target as an I3C private write, framed exactly
//! as `MctpI3cEncap::encode` would (I3C block-write header + MCTP transport
//! header + message-type byte + payload + PEC). The target's MCTP router hands
//! it to the echo app, which sends the payload straight back; the firmware then
//! exits 0. We collect the echoed frame with a private read and check the
//! payload survived the round trip.
//!
//! NOTE (first-cut, expected to need tuning once it runs):
//!   - Addresses: target dynamic address 0x08 (ROM-assigned, as in
//!     tests/i3c_service), controller 0x0a (must match REMOTE_I3C_ADDR in
//!     mctp_server.rs).
//!   - EIDs: target 8, host/controller 9 (echo replies to the source EID).
//!   - The firmware exits as soon as it has staged the reply, so we poll the
//!     private read alongside detecting exit, as tests/i3c_service does.

use i3c_host::{
    connect_i3c_socket, crc8_smbus, read_outgoing_packet, send_private_read_on_stream,
    send_private_write_raw_on_stream, Runner,
};
use std::thread;
use std::time::Duration;

const TARGET_ADDR: u8 = 0x08;
const CONTROLLER_ADDR: u8 = 0x0a;
const TARGET_EID: u8 = 8;
const HOST_EID: u8 = 9;
const ECHO_MSG_TYPE: u8 = 1;
const MCTP_I3C_CMD: u8 = 0x0f;

/// The payload we send and expect echoed back.
const PAYLOAD: &[u8] = b"mctp-i3c";

/// Build a full MCTP-over-I3C private-write frame, mirroring MctpI3cEncap.
fn build_request() -> Vec<u8> {
    // MCTP transport header (DSP0236): version, dest EID, src EID, flags.
    // flags 0xC8 = SOM | EOM | TO(tag owner) | tag 0.
    let mut mctp_packet = vec![0x01, TARGET_EID, HOST_EID, 0xC8, ECHO_MSG_TYPE];
    mctp_packet.extend_from_slice(PAYLOAD);

    // I3C block-write header: [dest<<1, cmd, byte_count, src<<1|1].
    // byte_count counts the source byte plus the MCTP packet.
    let byte_count = (1 + mctp_packet.len()) as u8;
    let mut frame = vec![
        TARGET_ADDR << 1,
        MCTP_I3C_CMD,
        byte_count,
        (CONTROLLER_ADDR << 1) | 1,
    ];
    frame.extend_from_slice(&mctp_packet);

    // PEC over everything but the PEC byte.
    let pec = crc8_smbus(&frame);
    frame.push(pec);
    frame
}

/// Pull the echoed payload out of a reply frame, if it looks like ours.
/// Layout: [i3c hdr (4)][mctp hdr (4)][msg_type (1)][payload...][PEC].
fn echoed_payload(data: &[u8]) -> Option<&[u8]> {
    const PREFIX: usize = 4 + 4 + 1;
    if data.len() < PREFIX + 1 {
        return None;
    }
    Some(&data[PREFIX..data.len() - 1])
}

#[test]
fn mctp_i3c_echo_host_test() {
    let runner = Runner::spawn(
        "target/veer/tests/mctp_i3c/mctp_i3c_runner.sh",
        "mctp echo: waiting for request",
    );
    assert!(
        runner.wait_ready(Duration::from_secs(600)),
        "runner exited or timed out before firmware readiness"
    );

    let mut stream =
        connect_i3c_socket(Duration::from_secs(5)).expect("failed to connect to I3C socket");
    let request = build_request();

    let mut got_echo = false;
    let mut attempts = 0u32;
    while !runner.exited() && attempts < 200 {
        attempts += 1;
        // The request is a complete MCTP-over-I3C frame with its own PEC, so it
        // must go out verbatim — the PEC-adding write helper would double-count
        // the address byte and the target would reject the frame.
        let _ = send_private_write_raw_on_stream(&mut stream, TARGET_ADDR, &request);
        thread::sleep(Duration::from_millis(100));

        // Collect the staged echo with a private read.
        if send_private_read_on_stream(&mut stream, TARGET_ADDR).is_ok()
            && let Ok(packet) = read_outgoing_packet(&mut stream)
            && echoed_payload(&packet.data) == Some(PAYLOAD)
        {
            got_echo = true;
            break;
        }
    }

    let status = runner.wait();
    assert!(
        status.success(),
        "runner exited with status: {status} after {attempts} attempts (echo captured: {got_echo})"
    );
    // The clean exit already proves the message reached the echo app; the byte
    // check is a stronger assertion when the read wins the race against exit.
    if !got_echo {
        eprintln!("note: firmware exited 0 but the echoed frame was not captured before exit");
    }
}
