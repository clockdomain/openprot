// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! I3C MCTP sender — outbound transport binding.
//!
//! Shaped like `services/mctp/transport-i2c`'s `I2cSender`: the same
//! fragmentation loop, the same error mapping. What differs is dictated by I3C
//! target mode, not by taste:
//!
//! * **This side is a target, not a bus master.** It cannot start a transfer,
//!   so "send" stages a TX and raises an IBI that prompts the controller to
//!   collect it with a private read. That is one `I3cOp::Send`, and the whole
//!   framed packet goes in it — unlike the embedded-hal I2C path, nothing
//!   prepends a destination byte, so nothing is trimmed off the front here.
//! * **The source address is runtime state.** It is assigned by ENTDAA, so it
//!   is read back per message rather than fixed at construction.

use i3c_api::Transport;
use i3c_client::I3cClient;
use mctp::Result;
use mctp_lib::fragment::SendOutput;

use crate::encap::{mctp_i3c_ipc_mtu, MctpI3cEncap, MCTP_I3C_IPC_FRAME, MCTP_I3C_IPC_PACKET};

/// I3C MCTP sender.
///
/// Implements [`mctp_lib::Sender`] to fragment and frame MCTP packets for an
/// I3C target, handing each finished frame to the i3c service.
pub struct I3cSender<T> {
    client: I3cClient<T>,
    /// Destination dynamic address. Single-peer for now, matching the I2C
    /// binding; a full EID → address neighbour table is tracked in
    /// <https://github.com/OpenPRoT/mctp-lib/issues/4>.
    remote_addr: u8,
    /// Whether to append an SMBus PEC. I3C protects the transfer at the link
    /// layer, so this is negotiated rather than assumed.
    pec: bool,
}

impl<T: Transport> I3cSender<T> {
    /// Create a sender over an i3c client.
    ///
    /// * `client` — the i3c target service client
    /// * `remote_addr` — the controller's 7-bit dynamic address
    /// * `pec` — append and expect an SMBus PEC
    pub const fn new(client: I3cClient<T>, remote_addr: u8, pec: bool) -> Self {
        Self {
            client,
            remote_addr,
            pec,
        }
    }

    /// Borrow the underlying client — to `recv` inbound frames on the same
    /// channel, or to read the dynamic address.
    pub fn client(&mut self) -> &mut I3cClient<T> {
        &mut self.client
    }

    /// This target's current dynamic address.
    ///
    /// Read fresh rather than cached: ENTDAA runs whenever the controller
    /// re-enumerates the bus, and a stale value would put the wrong source
    /// address in every outbound header — a fault that is invisible locally
    /// because the frames still encode and send cleanly.
    fn own_addr(&mut self) -> Result<u8> {
        match self.client.dynamic_address() {
            Ok(Some(addr)) => Ok(addr),
            // On the bus but not yet enumerated: nothing can be addressed from
            // here until the controller assigns an address.
            Ok(None) => Err(mctp::Error::Unreachable),
            Err(_) => Err(mctp::Error::TxFailure),
        }
    }
}

impl<T: Transport> mctp_lib::Sender for I3cSender<T> {
    fn send_vectored(
        &mut self,
        mut fragmenter: mctp_lib::fragment::Fragmenter,
        payload: &[&[u8]],
    ) -> Result<mctp::Tag> {
        let encoder = MctpI3cEncap::new(self.own_addr()?);
        let addr = self.remote_addr;
        let pec = self.pec;

        // Fragments are staged one at a time with back-pressure: `client.send`
        // is a blocking round-trip, and the i3c server holds a second `Send`
        // until the controller has read the first (signalled by
        // TargetEvent::ResponseRead). So this loop cannot outrun the controller
        // and overrun the target TX queue — the wait is in the server, not here.
        loop {
            let mut pkt = [0u8; MCTP_I3C_IPC_PACKET];
            match fragmenter.fragment_vectored(payload, &mut pkt) {
                SendOutput::Packet(p) => {
                    let mut out = [0u8; MCTP_I3C_IPC_FRAME];
                    let n = encoder.encode(addr, p, pec, &mut out)?;
                    let frame = out.get(..n).ok_or(mctp::Error::TxFailure)?;
                    if self.client.send(frame).is_err() {
                        pw_log::error!(
                            "i3c send failed: addr=0x{:02x} len=0x{:04x}",
                            addr as u32,
                            n as u32
                        );
                        return Err(mctp::Error::TxFailure);
                    }
                }
                SendOutput::Complete { tag, .. } => break Ok(tag),
                SendOutput::Error { err, .. } => break Err(err),
            }
        }
    }

    fn get_mtu(&self) -> usize {
        // The framed size depends on whether a PEC is appended, so the MTU
        // does too — a fixed value would under-fill every frame by one byte
        // when PEC is off.
        mctp_i3c_ipc_mtu(self.pec)
    }
}
