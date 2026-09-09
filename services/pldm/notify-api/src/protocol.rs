// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Wire protocol for the PLDM <-> Orchestrator notify channel.
//!
//! PLDM is the server/handler; the Orchestrator is the client/initiator (see
//! `docs/src/design/orchestrator/pldm-orchestrator-ipc-alt.md`). One whole
//! request/response round-trip per op, mirroring the `i2c` wire style:
//!
//! ```text
//! request : [NotifyRequestHeader] [op-specific payload]
//! response: [NotifyResponseHeader] [op-specific payload]
//! ```
//!
//! `Subscribe` and `Poll` carry no request payload. `Decision` and
//! `PushStatus` carry a one-byte payload. `Poll`'s response payload is an
//! encoded [`Pending`] on success, or an empty payload with status
//! [`NotifyError::NoPending`] when nothing is latched.
//!
//! Every enum here is intentionally exhaustive (not `#[non_exhaustive]`),
//! matching `orchestrator-capabilities`: adding a variant is a breaking
//! change, and the compiler forcing every consumer to handle it is the point
//! — most of all the orchestrator's `pending_to_event`, where an unhandled
//! `Pending` must not quietly become "no event".
//!
//! Openness to *unknown wire bytes* is a separate concern, and it is handled
//! where it belongs: the `TryFrom<u8>` impls below reject a byte this build
//! does not know. The wire is open; the types are closed.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Longest PLDM may go between servicing this channel, in milliseconds.
///
/// PLDM answers this channel from its terminus loop, between Update Agent
/// commands, so its responsiveness is bounded by how often that loop comes
/// around. The FD caps its idle MCTP poll at this value
/// (`FdEventSink::max_service_interval_millis`) so a subscribed supervisor is
/// never left waiting longer.
///
/// This is one half of a contract with [`MIN_TRANSACT_TIMEOUT_MILLIS`]; the
/// two live together here, rather than as unrelated constants at each end,
/// because the relationship between them is the thing that matters.
pub const MAX_SERVICE_INTERVAL_MILLIS: u32 = 10;

/// Smallest sane bounded deadline for an Orchestrator -> PLDM round-trip, in
/// milliseconds.
///
/// A round-trip costs up to [`MAX_SERVICE_INTERVAL_MILLIS`] waiting for PLDM's
/// loop to come around, plus the transaction itself. A deadline at or below
/// that interval would time out against a perfectly healthy PLDM that is
/// merely parked on its MCTP poll — and since the supervisor's health verdict
/// is one-way, it would condemn the peer permanently on a schedule, not on a
/// fault. Keep a wide margin: this is the supervisor's patience with an
/// answering peer, not a transfer budget.
pub const MIN_TRANSACT_TIMEOUT_MILLIS: u32 = 50;

/// Max payload bytes after either header (the largest is `Pending::Offer`).
pub const MAX_PAYLOAD_SIZE: usize = 9;

/// One request/response buffer size: header + max payload, rounded up.
pub const MAX_BUF_SIZE: usize = 32;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyOp {
    /// Arm PLDM to nudge this channel's peer signal on future events.
    Subscribe = 0x01,
    /// Drain the latched [`Pending`] event, if any.
    Poll = 0x02,
    /// Answer a pending `UpdateRequested` with [`Decision`].
    Decision = 0x03,
    /// Report update-progress [`Phase`] to PLDM.
    PushStatus = 0x04,
}

impl TryFrom<u8> for NotifyOp {
    type Error = NotifyError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Subscribe),
            0x02 => Ok(Self::Poll),
            0x03 => Ok(Self::Decision),
            0x04 => Ok(Self::PushStatus),
            _ => Err(NotifyError::InvalidOperation),
        }
    }
}

/// Orchestrator's accept/reject answer to a pending `UpdateRequested`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accepted = 0x00,
    Rejected = 0x01,
}

impl TryFrom<u8> for Decision {
    type Error = NotifyError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Accepted),
            0x01 => Ok(Self::Rejected),
            _ => Err(NotifyError::InvalidOperation),
        }
    }
}

/// Update-progress phase the orchestrator pushes to PLDM via `PushStatus`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Verifying = 0x00,
    Staging = 0x01,
    Staged = 0x02,
    Failed = 0x03,
    Activating = 0x04,
    Idle = 0x05,
}

impl TryFrom<u8> for Phase {
    type Error = NotifyError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Verifying),
            0x01 => Ok(Self::Staging),
            0x02 => Ok(Self::Staged),
            0x03 => Ok(Self::Failed),
            0x04 => Ok(Self::Activating),
            0x05 => Ok(Self::Idle),
            _ => Err(NotifyError::InvalidOperation),
        }
    }
}

/// An event PLDM latches for the orchestrator to drain via `Poll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    UpdateRequested,
    Offer { target: u32, total: u32 },
    Complete { written: u32 },
    Activate,
    Abort,
}

impl Pending {
    /// Largest encoded size: 1 kind byte + two `u32` fields (`Offer`).
    pub const MAX_ENCODED_LEN: usize = 9;

    /// Encode into `buf`, returning the number of bytes written.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, NotifyError> {
        let need = match self {
            Self::UpdateRequested | Self::Activate | Self::Abort => 1,
            Self::Complete { .. } => 5,
            Self::Offer { .. } => 9,
        };
        let dst = buf.get_mut(..need).ok_or(NotifyError::BufferTooSmall)?;
        match self {
            Self::UpdateRequested => dst[0] = 0x00,
            Self::Offer { target, total } => {
                dst[0] = 0x01;
                dst[1..5].copy_from_slice(&target.to_le_bytes());
                dst[5..9].copy_from_slice(&total.to_le_bytes());
            }
            Self::Complete { written } => {
                dst[0] = 0x02;
                dst[1..5].copy_from_slice(&written.to_le_bytes());
            }
            Self::Activate => dst[0] = 0x03,
            Self::Abort => dst[0] = 0x04,
        }
        Ok(need)
    }

    /// Decode a slice produced by [`encode`](Self::encode).
    pub fn decode(buf: &[u8]) -> Result<Self, NotifyError> {
        let kind = *buf.first().ok_or(NotifyError::InvalidOperation)?;
        match kind {
            0x00 => Ok(Self::UpdateRequested),
            0x01 => {
                let target = buf.get(1..5).ok_or(NotifyError::InvalidOperation)?;
                let total = buf.get(5..9).ok_or(NotifyError::InvalidOperation)?;
                Ok(Self::Offer {
                    target: u32::from_le_bytes(
                        target
                            .try_into()
                            .map_err(|_| NotifyError::InvalidOperation)?,
                    ),
                    total: u32::from_le_bytes(
                        total
                            .try_into()
                            .map_err(|_| NotifyError::InvalidOperation)?,
                    ),
                })
            }
            0x02 => {
                let written = buf.get(1..5).ok_or(NotifyError::InvalidOperation)?;
                Ok(Self::Complete {
                    written: u32::from_le_bytes(
                        written
                            .try_into()
                            .map_err(|_| NotifyError::InvalidOperation)?,
                    ),
                })
            }
            0x03 => Ok(Self::Activate),
            0x04 => Ok(Self::Abort),
            _ => Err(NotifyError::InvalidOperation),
        }
    }
}

/// Status / error code carried in `NotifyResponseHeader`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyError {
    InvalidOperation = 0x01,
    BufferTooSmall = 0x02,
    /// `Poll` found nothing latched.
    NoPending = 0x03,
    InternalError = 0xFF,
}

impl From<u8> for NotifyError {
    fn from(value: u8) -> Self {
        match value {
            0x01 => Self::InvalidOperation,
            0x02 => Self::BufferTooSmall,
            0x03 => Self::NoPending,
            _ => Self::InternalError,
        }
    }
}

impl core::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidOperation => f.write_str("invalid notify operation"),
            Self::BufferTooSmall => f.write_str("buffer too small"),
            Self::NoPending => f.write_str("no pending event"),
            Self::InternalError => f.write_str("internal notify server error"),
        }
    }
}

impl core::error::Error for NotifyError {}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct NotifyRequestHeader {
    op_code: u8,
    flags: u8,
    payload_len: u16,
}

impl NotifyRequestHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();

    pub fn new(op: NotifyOp, payload_len: u16) -> Self {
        Self {
            op_code: op as u8,
            flags: 0,
            payload_len: payload_len.to_le(),
        }
    }

    pub fn operation(&self) -> Result<NotifyOp, NotifyError> {
        NotifyOp::try_from(self.op_code)
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct NotifyResponseHeader {
    status: u8,
    reserved: u8,
    payload_len: u16,
}

impl NotifyResponseHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();

    pub fn success(payload_len: u16) -> Self {
        Self {
            status: 0,
            reserved: 0,
            payload_len: payload_len.to_le(),
        }
    }

    pub fn error(error: NotifyError) -> Self {
        Self {
            status: error as u8,
            reserved: 0,
            payload_len: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == 0
    }

    pub fn error_code(&self) -> Option<NotifyError> {
        if self.is_success() {
            None
        } else {
            Some(NotifyError::from(self.status))
        }
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::{FromBytes, IntoBytes};

    #[test]
    fn request_header_roundtrips_through_bytes() {
        let h = NotifyRequestHeader::new(NotifyOp::PushStatus, 1);
        let bytes = h.as_bytes();
        assert_eq!(bytes.len(), NotifyRequestHeader::SIZE);
        let decoded = NotifyRequestHeader::ref_from_bytes(bytes).unwrap();
        assert_eq!(decoded.operation(), Ok(NotifyOp::PushStatus));
        assert_eq!(decoded.payload_length(), 1);
    }

    #[test]
    fn response_header_success_and_error() {
        let ok = NotifyResponseHeader::success(4);
        let ok = NotifyResponseHeader::ref_from_bytes(ok.as_bytes()).unwrap();
        assert!(ok.is_success());
        assert_eq!(ok.payload_length(), 4);

        let err = NotifyResponseHeader::error(NotifyError::NoPending);
        let err = NotifyResponseHeader::ref_from_bytes(err.as_bytes()).unwrap();
        assert!(!err.is_success());
        assert_eq!(err.error_code(), Some(NotifyError::NoPending));
        assert_eq!(err.payload_length(), 0);
    }

    #[test]
    fn op_and_error_byte_mapping_is_stable() {
        for (raw, op) in [
            (0x01u8, NotifyOp::Subscribe),
            (0x02, NotifyOp::Poll),
            (0x03, NotifyOp::Decision),
            (0x04, NotifyOp::PushStatus),
        ] {
            assert_eq!(NotifyOp::try_from(raw), Ok(op));
            assert_eq!(op as u8, raw);
        }
        assert_eq!(NotifyOp::try_from(0x99), Err(NotifyError::InvalidOperation));

        for raw in 0x01u8..=0x03 {
            assert_eq!(NotifyError::from(raw) as u8, raw);
        }
        assert_eq!(NotifyError::from(0xFF), NotifyError::InternalError);
        assert_eq!(NotifyError::from(0x42), NotifyError::InternalError);
    }

    #[test]
    fn decision_and_phase_roundtrip() {
        for (raw, d) in [(0x00u8, Decision::Accepted), (0x01, Decision::Rejected)] {
            assert_eq!(Decision::try_from(raw), Ok(d));
            assert_eq!(d as u8, raw);
        }
        assert_eq!(Decision::try_from(0xFF), Err(NotifyError::InvalidOperation));

        for (raw, p) in [
            (0x00u8, Phase::Verifying),
            (0x01, Phase::Staging),
            (0x02, Phase::Staged),
            (0x03, Phase::Failed),
            (0x04, Phase::Activating),
            (0x05, Phase::Idle),
        ] {
            assert_eq!(Phase::try_from(raw), Ok(p));
            assert_eq!(p as u8, raw);
        }
        assert_eq!(Phase::try_from(0xFF), Err(NotifyError::InvalidOperation));
    }

    #[test]
    fn pending_roundtrips_through_bytes() {
        let mut buf = [0u8; Pending::MAX_ENCODED_LEN];

        for p in [
            Pending::UpdateRequested,
            Pending::Offer {
                target: 0x1000_0000,
                total: 4096,
            },
            Pending::Complete { written: 4096 },
            Pending::Activate,
            Pending::Abort,
        ] {
            let n = p.encode(&mut buf).unwrap();
            assert_eq!(Pending::decode(&buf[..n]), Ok(p));
        }
    }

    #[test]
    fn pending_encode_rejects_undersized_buffer() {
        let mut buf = [0u8; 4];
        assert_eq!(
            Pending::Offer {
                target: 1,
                total: 2
            }
            .encode(&mut buf),
            Err(NotifyError::BufferTooSmall)
        );
    }

    #[test]
    fn pending_decode_rejects_short_or_unknown_input() {
        assert_eq!(Pending::decode(&[]), Err(NotifyError::InvalidOperation));
        assert_eq!(
            Pending::decode(&[0x01, 0, 0]),
            Err(NotifyError::InvalidOperation)
        );
        assert_eq!(Pending::decode(&[0xFF]), Err(NotifyError::InvalidOperation));
    }
}
