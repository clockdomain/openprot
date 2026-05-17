// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Crypto driver wire protocol.
//!
//! Platform-agnostic: this crate names crypto **capabilities**, never the
//! silicon that implements them. The `engine` field discriminates a
//! *capability class* — symmetric (hash/MAC/cipher) vs public-key — not a
//! vendor block. The class→silicon mapping lives only in a `target/<plat>`
//! backend. Whole-object, run-to-completion only: there is deliberately no
//! `begin/update/finish` opcode (sketch §3.1).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Max in-band request/response payload. Large inputs use `HashRegion` with a
/// `RegionDescriptor` instead of streaming bytes through IPC.
pub const MAX_PAYLOAD_SIZE: usize = 1024;

/// Largest digest/MAC this protocol carries (SHA-512 = 64 bytes).
pub const MAX_DIGEST_SIZE: usize = 64;

/// Capability class the request targets — *not* a silicon block. One server
/// may own one backend per class; the class→block mapping is platform-only.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Hash / MAC / symmetric cipher.
    Symmetric = 0x00,
    /// Public-key: signature verify, modular exponentiation.
    PublicKey = 0x01,
}

impl TryFrom<u8> for Engine {
    type Error = CryptoError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x00 => Ok(Self::Symmetric),
            0x01 => Ok(Self::PublicKey),
            _ => Err(CryptoError::InvalidOperation),
        }
    }
}

/// Symmetric-class operations (`engine == Symmetric`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymmetricOp {
    HashOneShot = 0x01,
    HashRegion = 0x02,
    HashSg = 0x03,
    Hmac = 0x04,
    AesCrypt = 0x05,
}

impl TryFrom<u8> for SymmetricOp {
    type Error = CryptoError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x01 => Ok(Self::HashOneShot),
            0x02 => Ok(Self::HashRegion),
            0x03 => Ok(Self::HashSg),
            0x04 => Ok(Self::Hmac),
            0x05 => Ok(Self::AesCrypt),
            _ => Err(CryptoError::InvalidOperation),
        }
    }
}

/// Public-key-class operations (`engine == PublicKey`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicKeyOp {
    EcdsaP384Verify = 0x01,
    RsaModexp = 0x02,
}

impl TryFrom<u8> for PublicKeyOp {
    type Error = CryptoError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x01 => Ok(Self::EcdsaP384Verify),
            0x02 => Ok(Self::RsaModexp),
            _ => Err(CryptoError::InvalidOperation),
        }
    }
}

/// Digest algorithm selector. `digest_len` is the canonical output size.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algo {
    Sha256 = 0x01,
    Sha384 = 0x02,
    Sha512 = 0x03,
}

impl Algo {
    pub const fn digest_len(self) -> usize {
        match self {
            Algo::Sha256 => 32,
            Algo::Sha384 => 48,
            Algo::Sha512 => 64,
        }
    }
}

impl TryFrom<u8> for Algo {
    type Error = CryptoError;
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0x01 => Ok(Self::Sha256),
            0x02 => Ok(Self::Sha384),
            0x03 => Ok(Self::Sha512),
            _ => Err(CryptoError::InvalidAlgo),
        }
    }
}

/// Wire status. `VerifyFailed` is a *cryptographic result*, never conflated
/// with a transport/engine fault (sketch §3.5).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    Success = 0x00,
    InvalidOperation = 0x01,
    InvalidAlgo = 0x02,
    InvalidKeyHandle = 0x03,
    /// AES input not a block multiple — typed, rejected pre-engine (delta A4).
    InputNotBlockAligned = 0x04,
    Busy = 0x05,
    Timeout = 0x06,
    EngineFault = 0x07,
    /// Signature/verification did not pass. A normal outcome, not an error.
    VerifyFailed = 0x08,
    InternalError = 0xFF,
}

impl From<u8> for CryptoError {
    fn from(v: u8) -> Self {
        match v {
            0x00 => Self::Success,
            0x01 => Self::InvalidOperation,
            0x02 => Self::InvalidAlgo,
            0x03 => Self::InvalidKeyHandle,
            0x04 => Self::InputNotBlockAligned,
            0x05 => Self::Busy,
            0x06 => Self::Timeout,
            0x07 => Self::EngineFault,
            0x08 => Self::VerifyFailed,
            _ => Self::InternalError,
        }
    }
}

/// 8-byte request header, `repr(C, packed)` + zerocopy (USART convention).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct CryptoRequestHeader {
    pub engine: u8,
    pub op: u8,
    pub algo: u8,
    pub key_handle: u8,
    pub arg0: u16,
    pub payload_len: u16,
}

impl CryptoRequestHeader {
    pub const SIZE: usize = 8;

    pub fn new(engine: Engine, op: u8, algo: u8, key_handle: u8, arg0: u16, payload_len: u16) -> Self {
        Self {
            engine: engine as u8,
            op,
            algo,
            key_handle,
            arg0: arg0.to_le(),
            payload_len: payload_len.to_le(),
        }
    }

    pub fn engine(&self) -> Result<Engine, CryptoError> {
        Engine::try_from(self.engine)
    }

    pub fn algorithm(&self) -> Result<Algo, CryptoError> {
        Algo::try_from(self.algo)
    }

    pub fn arg0_value(&self) -> u16 {
        u16::from_le(self.arg0)
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}

/// 4-byte response header, mirrors `UsartResponseHeader`.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct CryptoResponseHeader {
    pub status: u8,
    pub reserved: u8,
    pub payload_len: u16,
}

impl CryptoResponseHeader {
    pub const SIZE: usize = 4;

    pub fn success(payload_len: u16) -> Self {
        Self {
            status: CryptoError::Success as u8,
            reserved: 0,
            payload_len: payload_len.to_le(),
        }
    }

    pub fn error(error: CryptoError) -> Self {
        Self {
            status: error as u8,
            reserved: 0,
            payload_len: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == CryptoError::Success as u8
    }

    pub fn error_code(&self) -> CryptoError {
        CryptoError::from(self.status)
    }

    pub fn payload_length(&self) -> usize {
        u16::from_le(self.payload_len) as usize
    }
}
