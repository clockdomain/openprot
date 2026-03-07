// Licensed under the Apache-2.0 license

//! Wire-protocol dispatch integration test.
//!
//! Exercises the full IPC path: encode request → `dispatch_mctp_op` → decode
//! response. This verifies that the wire protocol + dispatch layer + Server
//! work together correctly, simulating what happens when `IpcMctpClient`
//! talks to the MCTP server process over a Pigweed IPC channel.

use std::cell::RefCell;

use mctp::{Eid, Tag};
use mctp_stack::fragment::{Fragmenter, SendOutput};
use mctp_stack::Sender;
use openprot_mctp_api::wire;
use openprot_mctp_server::{dispatch::dispatch_mctp_op, Server};

// ---------------------------------------------------------------------------
// Mock transport (same as echo.rs)
// ---------------------------------------------------------------------------

struct BufferSender<'a> {
    packets: &'a RefCell<Vec<Vec<u8>>>,
}

impl Sender for BufferSender<'_> {
    fn send_vectored(
        &mut self,
        mut fragmenter: Fragmenter,
        payload: &[&[u8]],
    ) -> mctp::Result<Tag> {
        loop {
            let mut buf = [0u8; 255];
            match fragmenter.fragment_vectored(payload, &mut buf) {
                SendOutput::Packet(p) => {
                    self.packets.borrow_mut().push(p.to_vec());
                }
                SendOutput::Complete { tag, .. } => return Ok(tag),
                SendOutput::Error { err, .. } => return Err(err),
            }
        }
    }

    fn get_mtu(&self) -> usize {
        255
    }
}

fn transfer<S: Sender, const N: usize>(
    packets: &RefCell<Vec<Vec<u8>>>,
    dest: &mut Server<S, N>,
) {
    for pkt in packets.borrow().iter() {
        dest.inbound(pkt).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test SetEid + GetEid via dispatch.
#[test]
fn dispatch_set_get_eid() {
    let buf = RefCell::new(Vec::new());
    let sender = BufferSender { packets: &buf };
    let mut server: Server<_, 16> = Server::new(Eid(0), 0, sender);

    let mut req = [0u8; 64];
    let mut resp = [0u8; 64];
    let mut recv_buf = [0u8; 255];

    // SetEid(42)
    let req_len = wire::encode_set_eid(&mut req, 42).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());

    // GetEid → 42
    let req_len = wire::encode_get_eid(&mut req).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());
    assert_eq!(header.eid, 42);
}

/// Test Listener + Req + Send + Recv via dispatch (full echo round-trip).
#[test]
fn dispatch_echo_roundtrip() {
    // Server A (echo responder, EID 8)
    let buf_a = RefCell::new(Vec::new());
    let sender_a = BufferSender { packets: &buf_a };
    let mut server_a: Server<_, 16> = Server::new(Eid(8), 0, sender_a);

    // Server B (requester, EID 42)
    let buf_b = RefCell::new(Vec::new());
    let sender_b = BufferSender { packets: &buf_b };
    let mut server_b: Server<_, 16> = Server::new(Eid(42), 0, sender_b);

    let mut req = [0u8; 128];
    let mut resp = [0u8; 128];
    let mut recv_buf = [0u8; 255];

    // Register listener on A for MsgType(1)
    let req_len = wire::encode_listener(&mut req, 1).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_a, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());
    let listener_handle = header.handle;

    // Register req on B targeting EID 8
    let req_len = wire::encode_req(&mut req, 8).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_b, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());
    let req_handle = header.handle;

    // B sends a message via dispatch
    let payload = b"dispatch echo!";
    let req_len = wire::encode_send(
        &mut req,
        Some(req_handle),
        1,
        None,
        None,
        false,
        payload,
    )
    .unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_b, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());

    // Transfer B → A
    transfer(&buf_b, &mut server_a);

    // A receives via dispatch
    let req_len = wire::encode_recv(&mut req, listener_handle, 0).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_a, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());
    assert_eq!(header.msg_type, 1);
    assert_eq!(header.eid, 42); // remote EID
    let recv_payload = wire::get_response_payload(&resp[..resp_len], &header).unwrap();
    assert_eq!(recv_payload, payload);

    // A echoes back via dispatch (response: no handle, set eid + tag)
    let req_len = wire::encode_send(
        &mut req,
        None,
        header.msg_type,
        Some(header.eid),
        Some(header.tag),
        false,
        recv_payload,
    )
    .unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_a, &mut recv_buf);
    let send_header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(send_header.is_success());

    // Transfer A → B
    transfer(&buf_a, &mut server_b);

    // B receives the echo via dispatch
    let req_len = wire::encode_recv(&mut req, req_handle, 0).unwrap();
    let resp_len = dispatch_mctp_op(&req[..req_len], &mut resp, &mut server_b, &mut recv_buf);
    let header = wire::decode_response_header(&resp[..resp_len]).unwrap();
    assert!(header.is_success());
    assert_eq!(header.msg_type, 1);
    assert_eq!(header.eid, 8); // from server A
    let echo_payload = wire::get_response_payload(&resp[..resp_len], &header).unwrap();
    assert_eq!(echo_payload, payload);
}
