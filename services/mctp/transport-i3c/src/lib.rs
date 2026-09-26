// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! I3C transport binding for the MCTP server.
//!
//! Mirrors `services/mctp/transport-i2c`, but over an I3C target rather than an
//! I2C bus. `mctp-estack` provides no I3C encapsulation, so [`encap`] supplies
//! the DSP0233 header handling; fragmentation and reassembly stay in the MCTP
//! stack, which is transport-independent.

#![cfg_attr(not(test), no_std)]

pub mod encap;
mod receiver;
mod sender;

pub use encap::{
    decode_frame, mctp_i3c_ipc_mtu, MctpI3cEncap, MctpI3cHeader, MCTP_I3C_COMMAND_CODE,
    MCTP_I3C_HEADER, MCTP_I3C_IPC_FRAME, MCTP_I3C_IPC_MAXMTU, MCTP_I3C_IPC_PACKET, MCTP_I3C_MAXMTU,
};
pub use receiver::MctpI3cReceiver;
pub use sender::I3cSender;

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u8 = 0x08;
    const PEER: u8 = 0x1d;

    #[test]
    fn header_round_trips() {
        let h = MctpI3cHeader {
            dest: PEER,
            source: OWN,
            byte_count: 5,
        };
        let wire = h.encode().expect("encode");
        assert_eq!(wire, [PEER << 1, MCTP_I3C_COMMAND_CODE, 5, (OWN << 1) | 1]);
        assert_eq!(MctpI3cHeader::decode(&wire).expect("decode"), h);
    }

    #[test]
    fn header_rejects_non_7bit_addresses() {
        assert!(MctpI3cHeader {
            dest: 0x80,
            source: OWN,
            byte_count: 1
        }
        .encode()
        .is_err());
        assert!(MctpI3cHeader {
            dest: PEER,
            source: 0xff,
            byte_count: 1
        }
        .encode()
        .is_err());
    }

    #[test]
    fn header_rejects_oversize_byte_count() {
        assert!(MctpI3cHeader {
            dest: PEER,
            source: OWN,
            byte_count: 256
        }
        .encode()
        .is_err());
    }

    #[test]
    fn header_rejects_bad_framing() {
        let good = [PEER << 1, MCTP_I3C_COMMAND_CODE, 5, (OWN << 1) | 1];

        // Write bit set on the destination.
        let mut bad = good;
        bad[0] |= 1;
        assert!(MctpI3cHeader::decode(&bad).is_err());

        // Wrong command code.
        let mut bad = good;
        bad[1] = 0x0e;
        assert!(MctpI3cHeader::decode(&bad).is_err());

        // Source byte missing its fixed low bit.
        let mut bad = good;
        bad[3] &= !1;
        assert!(MctpI3cHeader::decode(&bad).is_err());

        // Short header.
        assert!(MctpI3cHeader::decode(&good[..3]).is_err());
    }

    #[test]
    fn encap_round_trips_without_pec() {
        let encap = MctpI3cEncap::new(OWN);
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let mut out = [0u8; 64];

        let n = encap
            .encode(PEER, &payload, false, &mut out)
            .expect("encode");
        assert_eq!(n, MCTP_I3C_HEADER + payload.len());

        // Decode from the peer's point of view: it sees itself as destination.
        let peer = MctpI3cEncap::new(PEER);
        let (inner, header) = peer.decode(&out[..n], false).expect("decode");
        assert_eq!(inner, &payload);
        assert_eq!(header.dest, PEER);
        assert_eq!(header.source, OWN);
    }

    #[test]
    fn encap_round_trips_with_pec() {
        let encap = MctpI3cEncap::new(OWN);
        let payload = [0x01, 0x02, 0x03];
        let mut out = [0u8; 64];

        let n = encap
            .encode(PEER, &payload, true, &mut out)
            .expect("encode");
        assert_eq!(n, MCTP_I3C_HEADER + payload.len() + 1);

        let peer = MctpI3cEncap::new(PEER);
        let (inner, _) = peer.decode(&out[..n], true).expect("decode");
        assert_eq!(inner, &payload);
    }

    #[test]
    fn decode_rejects_corrupt_pec() {
        let encap = MctpI3cEncap::new(OWN);
        let mut out = [0u8; 64];
        let n = encap
            .encode(PEER, &[0xaa, 0xbb], true, &mut out)
            .expect("encode");

        out[n - 1] ^= 0xff;
        assert!(MctpI3cEncap::new(PEER).decode(&out[..n], true).is_err());
    }

    #[test]
    fn decode_rejects_byte_count_mismatch() {
        let encap = MctpI3cEncap::new(OWN);
        let mut out = [0u8; 64];
        let n = encap
            .encode(PEER, &[0x11, 0x22, 0x33], false, &mut out)
            .expect("encode");

        // Claim one byte more than is present.
        out[2] += 1;
        assert!(MctpI3cEncap::new(PEER).decode(&out[..n], false).is_err());
    }

    #[test]
    fn encode_rejects_undersized_buffer() {
        let encap = MctpI3cEncap::new(OWN);
        let payload = [0u8; 8];
        let mut out = [0u8; MCTP_I3C_HEADER + 4];
        assert!(encap.encode(PEER, &payload, false, &mut out).is_err());
    }

    #[test]
    fn encode_rejects_oversize_payload() {
        let encap = MctpI3cEncap::new(OWN);
        let payload = [0u8; MCTP_I3C_MAXMTU + 1];
        let mut out = [0u8; 512];
        assert!(encap.encode(PEER, &payload, false, &mut out).is_err());
    }

    #[test]
    fn pec_matches_known_answer() {
        // Same vector as the i3c host harness's own PEC test
        // (target/veer/tests/i3c_host), which pins this polynomial.
        assert_eq!(encap::smbus_pec(&[0x10, 0x01, 0x02, 0x03, 0x04]), 0xd1);
    }

    #[test]
    fn decode_rejects_empty() {
        assert!(MctpI3cEncap::new(OWN).decode(&[], false).is_err());
    }
}
