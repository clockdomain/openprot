// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! MCTP over I3C, end to end on the host.
//!
//! `Server::send()` → `I3cSender` → the real `i3c_server::dispatch` → a mock
//! `I3cTarget` that records every staged frame, then back out through
//! `MctpI3cReceiver`. Every layer but the hardware is the production one.

use std::cell::RefCell;

use i3c_api::MAX_PAYLOAD as I3C_MAX_PAYLOAD;
use i3c_client::I3cClient;
use i3c_server::{LoopbackTransport, Server as I3cServer};
use mctp::Eid;
use openprot_hal_blocking::i3c_hardware::{DynamicAddress, I3cTarget, TargetEvent};
use openprot_mctp_server::Server;
use openprot_mctp_transport_i3c::{
    I3cSender, MctpI3cReceiver, MCTP_I3C_HEADER, MCTP_I3C_IPC_MAXMTU,
};

const OWN_ADDR: u8 = 0x08;
const REMOTE_ADDR: u8 = 0x1d;
const OWN_EID: u8 = 8;
const REMOTE_EID: u8 = 48;
const MSG_TYPE: u8 = 0x05; // SPDM

/// A target that records every frame staged for transmission.
struct CaptureTarget<'a> {
    addr: Option<u8>,
    sends: &'a RefCell<Vec<Vec<u8>>>,
}

#[derive(Debug, PartialEq, Eq)]
struct CaptureError;

impl I3cTarget for CaptureTarget<'_> {
    type Error = CaptureError;

    fn enable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn on_interrupt(&mut self) -> Result<TargetEvent, Self::Error> {
        Ok(TargetEvent::None)
    }

    fn read_frame(&mut self, _buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.sends.borrow_mut().push(data.to_vec());
        Ok(())
    }

    fn dynamic_address(&self) -> Option<DynamicAddress> {
        self.addr.and_then(|a| DynamicAddress::try_from(a).ok())
    }
}

type Sender<'a> = I3cSender<LoopbackTransport<CaptureTarget<'a>>>;

fn server(sends: &RefCell<Vec<Vec<u8>>>, addr: Option<u8>, pec: bool) -> Server<Sender<'_>, 16> {
    let target = CaptureTarget { addr, sends };
    let client = I3cClient::new(LoopbackTransport::new(I3cServer::new(1, 2, target)));
    Server::new(Eid(OWN_EID), 0, I3cSender::new(client, REMOTE_ADDR, pec))
}

#[test]
fn sender_receiver_roundtrip() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), true);

    let payload = b"hello mctp";
    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), MSG_TYPE, None, None, false, payload)
        .expect("send");

    let frames = sends.borrow();
    assert_eq!(frames.len(), 1, "short payload should be one frame");

    // Decode from the controller's side. Unlike the I2C binding nothing is
    // trimmed off the front: I3cOp::Send carries the whole framed packet.
    let rx = MctpI3cReceiver::new(true);
    let (pkt, header) = rx.decode(&frames[0]).expect("decode");

    assert_eq!(header.source, OWN_ADDR, "source address");
    assert_eq!(header.dest, REMOTE_ADDR, "destination address");

    // MCTP transport header (4) then the message-type byte, then the payload.
    assert!(pkt.len() >= 5, "packet too short: {}", pkt.len());
    assert_eq!(pkt[4] & 0x7f, MSG_TYPE, "message type");
    assert_eq!(&pkt[5..], payload, "payload");
}

#[test]
fn roundtrip_without_pec() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), false);

    let payload = b"no pec";
    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), MSG_TYPE, None, None, false, payload)
        .expect("send");

    let frames = sends.borrow();
    let rx = MctpI3cReceiver::new(false);
    let (pkt, _) = rx.decode(&frames[0]).expect("decode");
    assert_eq!(&pkt[5..], payload);

    // One byte shorter than the same exchange with a PEC.
    assert_eq!(frames[0].len(), MCTP_I3C_HEADER + pkt.len());
}

#[test]
fn single_fragment_produces_one_send() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), true);

    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), 1, None, None, false, b"short")
        .expect("send");

    assert_eq!(sends.borrow().len(), 1);
}

/// The case the MTU cap exists for: a payload past one fragment must split,
/// and *every* frame must still fit one i3c IPC frame.
#[test]
fn large_payload_fragments_and_every_frame_fits() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), true);

    let payload = vec![0x5au8; MCTP_I3C_IPC_MAXMTU * 3];
    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), MSG_TYPE, None, None, false, &payload)
        .expect("send");

    let frames = sends.borrow();
    assert!(
        frames.len() > 1,
        "payload of {} should have fragmented, got {} frame(s)",
        payload.len(),
        frames.len()
    );

    for (i, frame) in frames.iter().enumerate() {
        assert!(
            frame.len() <= I3C_MAX_PAYLOAD,
            "frame {} is {} bytes, over the {}-byte i3c limit",
            i,
            frame.len(),
            I3C_MAX_PAYLOAD
        );
        // Each frame must survive decoding on its own.
        MctpI3cReceiver::new(true)
            .decode(frame)
            .unwrap_or_else(|e| panic!("frame {i} failed to decode: {e:?}"));
    }
}

/// A payload sized exactly to the MTU is the boundary the cap is derived from.
#[test]
fn mtu_sized_fragment_fills_the_frame_exactly() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), true);

    // One byte of the first fragment is the message-type byte, so an MTU-sized
    // first packet carries MTU-1 bytes of payload.
    let payload = vec![0xa5u8; MCTP_I3C_IPC_MAXMTU - 1];
    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), MSG_TYPE, None, None, false, &payload)
        .expect("send");

    let frames = sends.borrow();
    assert_eq!(frames.len(), 1, "should still be a single fragment");
    assert_eq!(
        frames[0].len(),
        I3C_MAX_PAYLOAD,
        "an MTU-sized fragment should exactly fill the i3c frame"
    );
}

/// A send before the controller has assigned a dynamic address must fail
/// without putting anything on the bus.
///
/// This asserts the observable contract through `Server::send`. The precise
/// `mctp::Error::Unreachable` that `I3cSender` returns (documented on its
/// `own_addr`) is not checked here: the MCTP server maps it, along with several
/// other errors, through a wildcard to `ResponseCode::InternalError`, so the
/// specific variant cannot be recovered at this layer.
#[test]
fn send_before_entdaa_fails_without_touching_the_bus() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, None, true);

    let req = srv.req(REMOTE_EID).expect("req");
    assert!(
        srv.send(Some(req), MSG_TYPE, None, None, false, b"x")
            .is_err(),
        "send without a dynamic address must fail"
    );
    assert!(
        sends.borrow().is_empty(),
        "nothing should reach the bus before an address is assigned"
    );
}

#[test]
fn receiver_rejects_a_corrupt_pec() {
    let sends = RefCell::new(Vec::new());
    let mut srv = server(&sends, Some(OWN_ADDR), true);

    let req = srv.req(REMOTE_EID).expect("req");
    srv.send(Some(req), MSG_TYPE, None, None, false, b"tamper")
        .expect("send");

    let mut frame = sends.borrow()[0].clone();
    let last = frame.len() - 1;
    frame[last] ^= 0xff;

    assert!(MctpI3cReceiver::new(true).decode(&frame).is_err());
}
