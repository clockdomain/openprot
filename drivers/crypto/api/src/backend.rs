// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Backend trait contract — the seam between protocol dispatch and the
//! per-platform crypto engines.
//!
//! Platform-agnostic: traits are named by capability class. A
//! `target/<plat>/backend/crypto` crate implements both and is the *only*
//! place a concrete silicon block is named (sketch §1, §5). `BackendError`
//! maps 1-to-1 onto `CryptoError`.

use crate::protocol::{Algo, CryptoError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    InvalidOperation,
    InvalidAlgo,
    InvalidKeyHandle,
    InputNotBlockAligned,
    Busy,
    Timeout,
    EngineFault,
    InternalError,
}

impl From<BackendError> for CryptoError {
    fn from(value: BackendError) -> Self {
        match value {
            BackendError::InvalidOperation => CryptoError::InvalidOperation,
            BackendError::InvalidAlgo => CryptoError::InvalidAlgo,
            BackendError::InvalidKeyHandle => CryptoError::InvalidKeyHandle,
            BackendError::InputNotBlockAligned => CryptoError::InputNotBlockAligned,
            BackendError::Busy => CryptoError::Busy,
            BackendError::Timeout => CryptoError::Timeout,
            BackendError::EngineFault => CryptoError::EngineFault,
            BackendError::InternalError => CryptoError::InternalError,
        }
    }
}

/// Key reference. Over the IPC wire only `Sealed(handle)` is legal — raw
/// secret key bytes never cross the boundary (sketch §3.4). `Raw` exists for
/// the non-secret bulk-cipher case and for in-process `crypto_soft`.
#[derive(Clone, Copy, Debug)]
pub enum KeyRef<'a> {
    /// Opaque handle to a hardware-sealed key slot (never software-visible).
    Sealed(u8),
    /// Raw key material — non-secret bulk only / in-process.
    Raw(&'a [u8]),
}

/// Large-input descriptor: the server runs the page loop itself over this
/// region and holds the engine only for the duration (sketch §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionDescriptor {
    pub base: u64,
    pub len: u32,
}

/// One complete scatter/gather segment (no partial blocks across IPC).
#[derive(Clone, Copy, Debug)]
pub struct Segment<'a> {
    pub data: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AesMode {
    Ecb,
    Cbc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AesDir {
    Encrypt,
    Decrypt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AesKeyBits {
    Bits128,
    Bits256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AesParams {
    pub mode: AesMode,
    pub dir: AesDir,
    pub key_bits: AesKeyBits,
    /// 16-byte IV for CBC; ignored for ECB.
    pub iv: [u8; 16],
}

/// NIST P-384 public key (raw 48-byte big-endian-ordered scalars).
#[derive(Clone, Copy, Debug)]
pub struct EcPubP384 {
    pub qx: [u8; 48],
    pub qy: [u8; 48],
}

/// P-384 signature.
#[derive(Clone, Copy, Debug)]
pub struct EcSig {
    pub r: [u8; 48],
    pub s: [u8; 48],
}

/// Which RSA exponent the modexp uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpSel {
    /// Public exponent `e` — verify / encrypt.
    Public,
    /// Private exponent `d` — sign / decrypt.
    Private,
}

/// Caller RSA key (modexp only; padding/hashing stays consumer-side, sketch
/// §3.4 / R6).
#[derive(Clone, Copy, Debug)]
pub struct RsaKey<'a> {
    pub modulus: &'a [u8],
    pub exponent: &'a [u8],
}

/// Symmetric capability: digest / HMAC / AES. The platform impl owns the
/// underlying single engine and arbitrates exclusive access per call.
pub trait SymmetricBackend {
    fn hash(&mut self, algo: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError>;
    fn hash_region(
        &mut self,
        algo: Algo,
        region: RegionDescriptor,
        out: &mut [u8],
    ) -> Result<usize, BackendError>;
    fn hash_sg(
        &mut self,
        algo: Algo,
        segments: &[Segment<'_>],
        out: &mut [u8],
    ) -> Result<usize, BackendError>;
    fn hmac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, BackendError>;
    fn aes(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, BackendError>;
}

/// Public-key capability: a *separate* engine, but owned by the same server
/// process (sketch §1).
pub trait PublicKeyBackend {
    /// Returns the verify *result* — `Ok(false)` is a clean "did not verify",
    /// not an error (sketch §3.5).
    fn ecdsa_p384_verify(
        &mut self,
        pubkey: EcPubP384,
        sig: EcSig,
        digest48: &[u8; 48],
    ) -> Result<bool, BackendError>;
    fn rsa_modexp(
        &mut self,
        exp: ExpSel,
        key: RsaKey<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, BackendError>;
}
