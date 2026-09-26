// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! I3C MCTP receiver — inbound transport binding.
//!
//! Decodes frames latched by the i3c service into raw MCTP packets suitable
//! for `Server::inbound()`. The counterpart to
//! `services/mctp/transport-i2c`'s `MctpI2cReceiver`.

use crate::encap::{decode_frame, MctpI3cHeader};

/// Decodes inbound I3C private writes into raw MCTP packets.
///
/// One instance per i3c target carrying MCTP traffic. Decoding needs no local
/// address — the controller has already directed each private write at this
/// target's dynamic address — so the receiver carries only the PEC setting.
pub struct MctpI3cReceiver {
    pec: bool,
}

impl MctpI3cReceiver {
    /// Create a receiver.
    ///
    /// `pec` must match what the controller sends; I3C protects the transfer
    /// at the link layer, so a controller may legitimately omit it.
    pub fn new(pec: bool) -> Self {
        Self { pec }
    }

    /// Decode one inbound frame into a raw MCTP packet.
    ///
    /// Strips the four-byte transport header, verifies the PEC when enabled,
    /// and checks the byte count against the frame length. Returns the packet
    /// and the decoded header.
    ///
    /// The header's destination is returned rather than filtered on: the
    /// controller directs a private write at one dynamic address, so the
    /// hardware has already done that filtering. Callers that want to assert
    /// it anyway have `header.dest`.
    pub fn decode<'a>(&self, data: &'a [u8]) -> Result<(&'a [u8], MctpI3cHeader), mctp::Error> {
        decode_frame(data, self.pec)
    }
}
