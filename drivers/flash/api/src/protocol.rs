// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Wire protocol for the flash driver IPC channel.
//!
//! The operation set mirrors the flash storage HIL used in caliptra-mcu-sw
//! (`runtime/kernel/drivers/flash`), reframed as an opcode + packed-header
//! protocol matching the conventions of the other OpenPRoT userspace
//! drivers (see `drivers/usart/api`).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Maximum payload bytes carried in a single request or response.
///
/// Larger logical I/O is split into chunks by the client; the server's
/// per-call limit is reported by `GetChunkSize`.
pub const MAX_PAYLOAD_SIZE: usize = 256;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashOp {
    /// Probe the driver. Response carries no value.
    Exists = 0x01,
    /// Total bytes of flash exposed by this backend. Result in `value`.
    GetCapacity = 0x02,
    /// Maximum bytes per `Read` / `Write` request. Result in `value`.
    GetChunkSize = 0x03,
    /// Read `length` bytes starting at `address`. Response payload carries
    /// the bytes read; `value` is the byte count.
    Read = 0x04,
    /// Write the request payload (`payload_len` bytes) starting at
    /// `address`. `length` must equal `payload_len`. `value` returns the
    /// byte count actually written.
    Write = 0x05,
    /// Erase `length` bytes starting at `address`.
    Erase = 0x06,
}

impl TryFrom<u8> for FlashOp {
    type Error = FlashError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Exists),
            0x02 => Ok(Self::GetCapacity),
            0x03 => Ok(Self::GetChunkSize),
            0x04 => Ok(Self::Read),
            0x05 => Ok(Self::Write),
            0x06 => Ok(Self::Erase),
            _ => Err(FlashError::InvalidOperation),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashError {
    Success = 0x00,
    InvalidOperation = 0x01,
    InvalidAddress = 0x02,
    InvalidLength = 0x03,
    BufferTooSmall = 0x04,
    Busy = 0x05,
    Timeout = 0x06,
    /// Operation cannot complete right now; backend has queued it and will
    /// signal completion via interrupt.
    WouldBlock = 0x07,
    /// Underlying media reported an I/O error (e.g. flash program failure).
    IoError = 0x08,
    /// Address/length straddles a region the backend refuses to touch
    /// (e.g. write-protected partition).
    NotPermitted = 0x09,
    InternalError = 0xFF,
}

impl From<u8> for FlashError {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Self::Success,
            0x01 => Self::InvalidOperation,
            0x02 => Self::InvalidAddress,
            0x03 => Self::InvalidLength,
            0x04 => Self::BufferTooSmall,
            0x05 => Self::Busy,
            0x06 => Self::Timeout,
            0x07 => Self::WouldBlock,
            0x08 => Self::IoError,
            0x09 => Self::NotPermitted,
            _ => Self::InternalError,
        }
    }
}

/// Request header on the wire. 16 bytes, little-endian, packed.
///
/// `address` and `length` are interpreted per `op_code`; see `FlashOp`.
/// `payload_len` is the number of bytes that immediately follow this
/// header in the request frame (zero for read/erase/probe ops).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct FlashRequestHeader {
    pub op_code: u8,
    pub flags: u8,
    pub payload_len: u16,
    pub address: u32,
    pub length: u32,
    pub reserved: u32,
}

impl FlashRequestHeader {
    pub const SIZE: usize = 16;

    pub fn new(op: FlashOp, address: u32, length: u32, payload_len: u16) -> Self {
        Self {
            op_code: op as u8,
            flags: 0,
            payload_len: payload_len.to_le(),
            address: address.to_le(),
            length: length.to_le(),
            reserved: 0,
        }
    }

    pub fn operation(&self) -> Result<FlashOp, FlashError> {
        FlashOp::try_from(self.op_code)
    }

    pub fn address_value(&self) -> u32 {
        u32::from_le(self.address)
    }

    pub fn length_value(&self) -> u32 {
        u32::from_le(self.length)
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}

/// Response header on the wire. 8 bytes, little-endian, packed.
///
/// `value` is a per-op return word — capacity, chunk size, bytes
/// processed, etc. `payload_len` counts bytes that follow this header
/// (non-zero only for `Read`).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct FlashResponseHeader {
    pub status: u8,
    pub reserved: u8,
    pub payload_len: u16,
    pub value: u32,
}

impl FlashResponseHeader {
    pub const SIZE: usize = 8;

    pub fn success(value: u32, payload_len: u16) -> Self {
        Self {
            status: FlashError::Success as u8,
            reserved: 0,
            payload_len: payload_len.to_le(),
            value: value.to_le(),
        }
    }

    pub fn error(error: FlashError) -> Self {
        Self {
            status: error as u8,
            reserved: 0,
            payload_len: 0,
            value: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == FlashError::Success as u8
    }

    pub fn error_code(&self) -> FlashError {
        FlashError::from(self.status)
    }

    pub fn value_word(&self) -> u32 {
        u32::from_le(self.value)
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}
