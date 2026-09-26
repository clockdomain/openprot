// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! MCTP-over-I3C encapsulation (DSP0233).
//!
//! `mctp-estack` ships `i2c`, `serial` and `usb` bindings but no I3C one, so
//! the header handling lives here. It is deliberately shaped like
//! `mctp_estack::i2c` so the two read side by side.
//!
//! # Wire format
//!
//! MCTP-over-I3C reuses the SMBus/I2C block-write framing of DSP0237, carried
//! in an I3C private write:
//!
//! ```text
//!   0        1        2        3        4 ..            N
//! +--------+--------+--------+--------+-----------------+-------+
//! | dest<<1| 0x0F   | bytecnt| src<<1 | MCTP packet ... | PEC   |
//! +--------+--------+--------+--------+-----------------+-------+
//!   ^ addr   ^ cmd    ^ count  ^ addr|1                   ^ optional
//! ```
//!
//! Two things differ from I2C and are the reason this is not a straight reuse:
//!
//! * **Addresses are I3C dynamic addresses.** They are assigned by the bus
//!   controller during ENTDAA rather than fixed in hardware, so `own_addr` is
//!   whatever the controller handed this target -- read back at runtime, not a
//!   build-time constant.
//! * **PEC is optional and separately negotiated.** I3C already protects the
//!   transfer at the link layer, so a controller may omit it. Both `decode`
//!   and `encode` take an explicit `pec` flag rather than assuming.
//!
//! The MCTP packet payload itself is transport-independent, so fragmentation
//! and reassembly stay in `mctp-estack`; this module only adds and removes the
//! four-byte transport header.

use mctp::{Error, Result};
use openprot_mctp_api::MCTP_HEADER_LEN;

/// SMBus PEC: CRC-8 with polynomial 0x07, zero seed, no reflection.
///
/// Inlined rather than pulled from `smbus-pec`, which is only a transitive
/// dependency here; the same routine is used by the I3C host harness in
/// `target/veer/tests/i3c_host`, whose known-answer test pins it.
pub fn smbus_pec(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &value in data {
        crc ^= value;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The DSP0237 MCTP command code, reused unchanged by DSP0233.
pub const MCTP_I3C_COMMAND_CODE: u8 = 0x0f;

/// Size of the encapsulation header, in bytes.
pub const MCTP_I3C_HEADER: usize = 4;

/// DSP0233 protocol ceiling: the byte-count field is a `u8` and counts the
/// source-address byte alongside the packet, so this bounds what a *wire frame*
/// may legally claim — the limit [`MctpI3cEncap::encode`] and `decode` enforce
/// on the byte count.
///
/// It is **not** the deliverable limit. What the i3c IPC transport can actually
/// carry is the tighter [`mctp_i3c_ipc_mtu`]; a frame within `MCTP_I3C_MAXMTU`
/// but over that bound is well-formed on the wire yet too large for one
/// `I3cOp::Send`. Callers that send over the i3c service size against the IPC
/// bound, not this one.
pub const MCTP_I3C_MAXMTU: usize = u8::MAX as usize - 1;

/// Largest MCTP *payload* per packet that fits one i3c IPC frame, given whether
/// a PEC byte is appended. This is what `I3cSender::get_mtu` reports.
///
/// The i3c service carries one framed packet per `I3cOp::Send`, capped at
/// `i3c_api::MAX_PAYLOAD` — the caliptra i3c-core private write limit, which is
/// hardware and cannot move. `Fragmenter::fragment_vectored` emits a packet of
/// `mtu + MCTP_HEADER_LEN` bytes, so the MCTP transport header is already
/// inside what [`MctpI3cEncap::encode`] frames:
///
/// ```text
/// 4 (I3C header) + 4 (MCTP header) + payload + pec <= 250
///   => payload <= 250 - 4 - 4 - pec   (241 with PEC, 242 without)
/// ```
///
/// Derived from `i3c_api::MAX_PAYLOAD` rather than a literal so it tracks the
/// hardware limit, and takes `pec` so it does not silently assume one.
pub const fn mctp_i3c_ipc_mtu(pec: bool) -> usize {
    i3c_api::MAX_PAYLOAD - MCTP_I3C_HEADER - MCTP_HEADER_LEN - pec as usize
}

/// The i3c IPC MTU with a PEC appended (241) — the smaller of the two cases,
/// used where a single figure is wanted (tests, docs).
pub const MCTP_I3C_IPC_MAXMTU: usize = mctp_i3c_ipc_mtu(true);

/// Buffer size `Fragmenter::fragment_vectored` requires — it refuses to emit
/// into anything smaller than `mtu + MCTP_HEADER_LEN`. Sized for the larger,
/// no-PEC MTU so one buffer serves both cases.
pub const MCTP_I3C_IPC_PACKET: usize = mctp_i3c_ipc_mtu(false) + MCTP_HEADER_LEN;

/// Largest whole framed packet the transport ever hands to `I3cOp::Send`.
pub const MCTP_I3C_IPC_FRAME: usize = i3c_api::MAX_PAYLOAD;

/// A decoded MCTP-over-I3C transport header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MctpI3cHeader {
    /// 7-bit destination dynamic address, excluding the R/W# bit.
    pub dest: u8,
    /// 7-bit source dynamic address, excluding the fixed low bit.
    pub source: u8,
    /// Bytes following this field, up to but excluding any PEC.
    pub byte_count: usize,
}

impl MctpI3cHeader {
    /// Encode to the four-byte on-wire header.
    ///
    /// Fails with [`Error::BadArgument`] if either address is not 7-bit or the
    /// byte count does not fit in a `u8`.
    pub fn encode(&self) -> Result<[u8; MCTP_I3C_HEADER]> {
        if self.dest > 0x7f || self.source > 0x7f {
            return Err(Error::BadArgument);
        }
        if self.byte_count > u8::MAX as usize {
            return Err(Error::BadArgument);
        }
        Ok([
            self.dest << 1,
            MCTP_I3C_COMMAND_CODE,
            self.byte_count as u8,
            (self.source << 1) | 1,
        ])
    }

    /// Decode and validate a four-byte header.
    ///
    /// Fails with [`Error::InvalidInput`] if the write bit, command code, or
    /// source low bit are wrong.
    pub fn decode(header: &[u8]) -> Result<Self> {
        let [dest, cmd, byte_count, source]: [u8; MCTP_I3C_HEADER] =
            header.try_into().map_err(|_| Error::BadArgument)?;
        if dest & 1 != 0 {
            return Err(Error::InvalidInput);
        }
        if cmd != MCTP_I3C_COMMAND_CODE {
            return Err(Error::InvalidInput);
        }
        if source & 1 != 1 {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            dest: dest >> 1,
            source: source >> 1,
            byte_count: byte_count as usize,
        })
    }
}

/// Strip the MCTP-over-I3C transport header from an inbound private write.
///
/// When `pec` is set the trailing PEC byte is verified and removed first.
/// Returns the inner MCTP packet and the decoded header.
///
/// Decoding needs no local address — the controller has already directed the
/// private write at one target's dynamic address — so this is a free function,
/// not a method. Fails if the PEC is wrong, the header is malformed, or the
/// byte-count field disagrees with the actual packet length.
pub fn decode_frame(mut packet: &[u8], pec: bool) -> Result<(&[u8], MctpI3cHeader)> {
    if packet.is_empty() {
        return Err(Error::InvalidInput);
    }

    if pec {
        let (packet_pec, rest) = packet.split_last().ok_or(Error::InvalidInput)?;
        if smbus_pec(rest) != *packet_pec {
            return Err(Error::InvalidInput);
        }
        packet = rest;
    }

    let header = MctpI3cHeader::decode(packet.get(..MCTP_I3C_HEADER).ok_or(Error::InvalidInput)?)?;

    // byte_count covers everything after the count field except the PEC:
    // the source byte plus the MCTP packet. Adding back dest, command and
    // count gives the full framed length.
    if header.byte_count + 3 != packet.len() {
        return Err(Error::InvalidInput);
    }

    Ok((&packet[MCTP_I3C_HEADER..], header))
}

/// Adds and removes the MCTP-over-I3C transport header.
///
/// `own_addr` is this target's I3C dynamic address. Unlike the I2C case it is
/// not known until the controller has run ENTDAA, so construct this after the
/// address has been read back from the hardware.
#[derive(Debug, Clone)]
pub struct MctpI3cEncap {
    own_addr: u8,
}

impl MctpI3cEncap {
    /// Bind to a dynamic address.
    pub fn new(own_addr: u8) -> Self {
        Self { own_addr }
    }

    /// This target's dynamic address.
    pub fn own_addr(&self) -> u8 {
        self.own_addr
    }

    /// Strip the transport header from an inbound private write.
    ///
    /// A thin wrapper over the free [`decode_frame`]; decoding needs no local
    /// address, so it does not read `self`. Kept as a method for symmetry with
    /// [`MctpI3cEncap::encode`].
    pub fn decode<'f>(&self, packet: &'f [u8], pec: bool) -> Result<(&'f [u8], MctpI3cHeader)> {
        decode_frame(packet, pec)
    }

    /// Frame one MCTP packet for an outbound private write.
    ///
    /// Writes the header, the payload, and -- when `pec` is set -- the trailing
    /// PEC into `out`, returning the framed length.
    ///
    /// Fails with [`Error::NoSpace`] if `out` is too small, or
    /// [`Error::BadArgument`] if the payload exceeds [`MCTP_I3C_MAXMTU`].
    pub fn encode(&self, dest: u8, payload: &[u8], pec: bool, out: &mut [u8]) -> Result<usize> {
        if payload.len() > MCTP_I3C_MAXMTU {
            return Err(Error::BadArgument);
        }

        let pec_extra = usize::from(pec);
        let framed = MCTP_I3C_HEADER + payload.len() + pec_extra;
        if out.len() < framed {
            return Err(Error::NoSpace);
        }

        let header = MctpI3cHeader {
            dest,
            source: self.own_addr,
            // source byte + payload
            byte_count: payload.len() + 1,
        }
        .encode()?;

        out.get_mut(..MCTP_I3C_HEADER)
            .ok_or(Error::NoSpace)?
            .copy_from_slice(&header);
        out.get_mut(MCTP_I3C_HEADER..MCTP_I3C_HEADER + payload.len())
            .ok_or(Error::NoSpace)?
            .copy_from_slice(payload);

        if pec {
            let body = out
                .get(..MCTP_I3C_HEADER + payload.len())
                .ok_or(Error::NoSpace)?;
            let crc = smbus_pec(body);
            *out.get_mut(framed - 1).ok_or(Error::NoSpace)? = crc;
        }

        Ok(framed)
    }
}
