// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Host-buildable core of the i3c server: the target-owning [`Server`] state
//! and the request [`dispatch`] against the [`I3cTarget`] facade.
//!
//! No syscalls live here, so the request handling and the single-frame inbound
//! latch are verified in host tests. The kernel-tagged `i3c_server_runtime`
//! wraps this in the Pigweed WaitGroup loop and supplies the IRQ/IPC syscalls.

#![no_std]

pub mod loopback;

pub use loopback::LoopbackTransport;

use i3c_api::{decode_request, encode_response, I3cOp, I3cStatus, HEADER, MAX_PAYLOAD};
use openprot_hal_blocking::i3c_hardware::I3cTarget;

/// The I3C target the server owns, plus its IPC channel, IRQ handle, the
/// single-frame inbound latch, and the outbound "response staged, awaiting
/// read" flag.
///
/// One frame is latched at a time: MCTP over I3C is request/response, so the
/// client `Recv`s before the next inbound frame. A frame arriving while one is
/// still latched overwrites it — the bus is never blocked.
///
/// Outbound has back-pressure. A `Send` stages a response for the controller's
/// next private read and sets [`tx_pending`](Server::tx_pending); the flag
/// clears only when the controller has read it, signalled by
/// [`notify_response_read`](Server::notify_response_read) on a
/// [`TargetEvent::ResponseRead`]. The runtime uses the flag to hold a second
/// `Send` until the first has been read, so fragments are not staged faster
/// than the controller drains them. `dispatch` itself always stages (the
/// gating is the runtime's), so the in-process loopback path is unaffected.
///
/// [`TargetEvent::ResponseRead`]: openprot_hal_blocking::i3c_hardware::TargetEvent::ResponseRead
pub struct Server<T> {
    /// IPC channel handle carrying the transport protocol.
    pub channel: u32,
    /// IRQ handle for the I3C controller.
    pub irq: u32,
    /// The controller driver implementing the facade.
    pub target: T,
    rx: [u8; MAX_PAYLOAD],
    rx_len: usize,
    rx_ready: bool,
    tx_pending: bool,
}

impl<T> Server<T> {
    /// Bind the server to one channel, one IRQ, and one target driver.
    pub const fn new(channel: u32, irq: u32, target: T) -> Self {
        Self {
            channel,
            irq,
            target,
            rx: [0u8; MAX_PAYLOAD],
            rx_len: 0,
            rx_ready: false,
            tx_pending: false,
        }
    }

    /// Whether an inbound frame is latched and waiting for a `Recv`.
    pub fn has_frame(&self) -> bool {
        self.rx_ready
    }

    /// Whether a staged transmit is still awaiting the controller's private
    /// read. The runtime holds a further `Send` while this is set.
    pub fn tx_pending(&self) -> bool {
        self.tx_pending
    }

    /// Clear the outbound back-pressure flag: the controller has read the staged
    /// response. Called by the runtime on [`TargetEvent::ResponseRead`].
    ///
    /// [`TargetEvent::ResponseRead`]: openprot_hal_blocking::i3c_hardware::TargetEvent::ResponseRead
    pub fn notify_response_read(&mut self) {
        self.tx_pending = false;
    }
}

impl<T: I3cTarget> Server<T> {
    /// Drain one inbound frame into the latch after a [`TargetEvent::InboundReady`].
    ///
    /// Returns `true` if a frame was latched. A frame overwrites any previously
    /// latched but unread frame.
    pub fn latch_inbound(&mut self) -> Result<bool, T::Error> {
        match self.target.read_frame(&mut self.rx)? {
            Some(n) => {
                self.rx_len = n.min(MAX_PAYLOAD);
                self.rx_ready = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

fn status_only(resp: &mut [u8], status: I3cStatus) -> usize {
    match encode_response(status, &[], resp) {
        Some(n) => n,
        None => match resp.first_mut() {
            Some(head) => {
                *head = status as u8;
                1
            }
            None => 0,
        },
    }
}

/// Dispatch one client request against the target, writing the response into
/// `resp` and returning its length.
///
/// `Recv` consumes the latch. This function performs no syscalls; the runtime
/// manages the `USER` notification around it.
pub fn dispatch<T: I3cTarget>(srv: &mut Server<T>, req: &[u8], resp: &mut [u8]) -> usize {
    let Some((op, payload)) = decode_request(req) else {
        return status_only(resp, I3cStatus::InvalidOp);
    };
    match op {
        I3cOp::Send => {
            if payload.len() > MAX_PAYLOAD {
                return status_only(resp, I3cStatus::TooLong);
            }
            match srv.target.send(payload) {
                Ok(()) => {
                    // Staged for the controller's next private read; hold further
                    // sends until it has been read (cleared by the runtime on
                    // ResponseRead).
                    srv.tx_pending = true;
                    status_only(resp, I3cStatus::Ok)
                }
                Err(_) => status_only(resp, I3cStatus::Internal),
            }
        }
        I3cOp::Recv => {
            if !srv.rx_ready {
                return status_only(resp, I3cStatus::NoData);
            }
            let cap = resp.len().saturating_sub(HEADER);
            let n = srv.rx_len.min(cap);
            let out = encode_response(I3cStatus::Ok, srv.rx.get(..n).unwrap_or(&[]), resp);
            srv.rx_ready = false;
            srv.rx_len = 0;
            out.unwrap_or_else(|| status_only(resp, I3cStatus::Internal))
        }
        I3cOp::DynamicAddress => match srv.target.dynamic_address() {
            Some(addr) => encode_response(I3cStatus::Ok, &[addr.as_u8()], resp)
                .unwrap_or_else(|| status_only(resp, I3cStatus::Internal)),
            None => status_only(resp, I3cStatus::Unassigned),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i3c_api::{decode_response, encode_request, MAX_FRAME};
    use openprot_hal_blocking::i3c_hardware::{DynamicAddress, TargetEvent};

    struct FakeTarget {
        addr: Option<u8>,
        inbound: Option<[u8; 4]>,
        sent: [u8; MAX_PAYLOAD],
        sent_len: usize,
    }

    impl Default for FakeTarget {
        fn default() -> Self {
            Self {
                addr: None,
                inbound: None,
                sent: [0u8; MAX_PAYLOAD],
                sent_len: 0,
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FakeError;

    impl I3cTarget for FakeTarget {
        type Error = FakeError;
        fn enable(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn on_interrupt(&mut self) -> Result<TargetEvent, Self::Error> {
            if self.inbound.is_some() {
                Ok(TargetEvent::InboundReady)
            } else {
                Ok(TargetEvent::None)
            }
        }
        fn read_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
            match self.inbound.take() {
                Some(frame) if frame.len() <= buf.len() => {
                    buf[..frame.len()].copy_from_slice(&frame);
                    Ok(Some(frame.len()))
                }
                Some(_) => Err(FakeError),
                None => Ok(None),
            }
        }
        fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
            let n = data.len().min(MAX_PAYLOAD);
            self.sent[..n].copy_from_slice(&data[..n]);
            self.sent_len = n;
            Ok(())
        }
        fn dynamic_address(&self) -> Option<DynamicAddress> {
            self.addr.and_then(|a| DynamicAddress::try_from(a).ok())
        }
    }

    fn srv() -> Server<FakeTarget> {
        Server::new(1, 2, FakeTarget::default())
    }

    #[test]
    fn send_dispatches_to_target() {
        let mut s = srv();
        let mut req = [0u8; MAX_FRAME];
        let mut resp = [0u8; MAX_FRAME];
        let n = encode_request(I3cOp::Send, b"pong", &mut req).unwrap();
        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(decode_response(&resp[..rn]), Some((I3cStatus::Ok, &[][..])));
        assert_eq!(&s.target.sent[..s.target.sent_len], b"pong");
    }

    #[test]
    fn send_sets_tx_pending_until_response_read() {
        let mut s = srv();
        let mut req = [0u8; MAX_FRAME];
        let mut resp = [0u8; MAX_FRAME];
        assert!(!s.tx_pending(), "nothing staged yet");

        let n = encode_request(I3cOp::Send, b"pong", &mut req).unwrap();
        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(decode_response(&resp[..rn]), Some((I3cStatus::Ok, &[][..])));
        assert!(s.tx_pending(), "a staged response is awaiting the read");

        // The controller reads it; the runtime clears the flag.
        s.notify_response_read();
        assert!(!s.tx_pending(), "back-pressure released after the read");
    }

    #[test]
    fn recv_without_frame_is_nodata() {
        let mut s = srv();
        let mut req = [0u8; MAX_FRAME];
        let mut resp = [0u8; MAX_FRAME];
        let n = encode_request(I3cOp::Recv, &[], &mut req).unwrap();
        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::NoData, &[][..]))
        );
    }

    #[test]
    fn latch_then_recv_returns_frame_once() {
        let mut s = srv();
        s.target.inbound = Some(*b"ping");

        // Simulate the IRQ path draining the frame into the latch.
        assert_eq!(s.on_interrupt_event(), TargetEvent::InboundReady);
        assert_eq!(s.latch_inbound(), Ok(true));
        assert!(s.has_frame());

        let mut req = [0u8; MAX_FRAME];
        let mut resp = [0u8; MAX_FRAME];
        let n = encode_request(I3cOp::Recv, &[], &mut req).unwrap();

        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::Ok, &b"ping"[..]))
        );
        assert!(!s.has_frame());

        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::NoData, &[][..]))
        );
    }

    #[test]
    fn dynamic_address_reports_assignment() {
        let mut s = srv();
        let mut req = [0u8; MAX_FRAME];
        let mut resp = [0u8; MAX_FRAME];
        let n = encode_request(I3cOp::DynamicAddress, &[], &mut req).unwrap();

        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::Unassigned, &[][..]))
        );

        s.target.addr = Some(0x42);
        let rn = dispatch(&mut s, &req[..n], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::Ok, &[0x42][..]))
        );
    }

    #[test]
    fn unknown_opcode_is_invalid() {
        let mut s = srv();
        let mut resp = [0u8; MAX_FRAME];
        let rn = dispatch(&mut s, &[0xFF], &mut resp);
        assert_eq!(
            decode_response(&resp[..rn]),
            Some((I3cStatus::InvalidOp, &[][..]))
        );
    }

    // Test-only helper mirroring what the runtime reads from `on_interrupt`.
    impl Server<FakeTarget> {
        fn on_interrupt_event(&mut self) -> TargetEvent {
            self.target.on_interrupt().unwrap()
        }
    }
}
