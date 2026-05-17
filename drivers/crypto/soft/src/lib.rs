// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! `crypto_soft` — pure-software, in-process implementations of the
//! `crypto_traits` seam (`crypto-driver-sketch.md` §3.3 / §3.6).
//!
//! This is the substitution target consumers link when there is no engine and
//! no IPC: boot-time / secret-bearing / interleaved workloads, and the
//! always-present correctness fallback (sketch §3.3 policy table, §3.6).
//! Everything here runs to completion synchronously in the caller's address
//! space.
//!
//! Backing libraries (all `default-features = false`, `no_std`):
//!
//! * `Digest` → RustCrypto `sha2` (real SHA-256/384/512).
//! * `Mac`    → RustCrypto `hmac` over `sha2` (real HMAC).
//! * `Cipher` → RustCrypto `aes` block primitive with ECB/CBC chaining
//!   implemented here (the `cbc`/`ecb` mode crates are not vendored in
//!   `//third_party/crates_io`, so the mode loop is open-coded over the real
//!   AES block function — this is real AES, not a stub).
//! * `Verify` → **skeleton stub**: no ECDSA-P384 / RSA implementation is
//!   available in `@rust_crates`. See [`SoftVerify`].

#![no_std]

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use crypto_traits::{
    AesParams, Algo, Cipher, CryptoError, Digest, EcPubP384, EcSig, KeyRef, Mac, RsaKey, Verify,
};
use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256, Sha384, Sha512};

use crypto_api::backend::{AesDir, AesKeyBits, AesMode};

/// In-process software crypto provider. Zero-sized: holds no engine, no IPC
/// handle — that is the whole point of the software seam (sketch §3.6).
#[derive(Clone, Copy, Debug, Default)]
pub struct SoftCrypto;

impl SoftCrypto {
    pub const fn new() -> Self {
        Self
    }
}

// --- Digest -----------------------------------------------------------------

/// Type alias documenting that `SoftCrypto` is the digest provider.
pub type SoftDigest = SoftCrypto;

impl Digest for SoftCrypto {
    fn digest(&mut self, algo: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, CryptoError> {
        let n = algo.digest_len();
        if out.len() < n {
            return Err(CryptoError::OutputTooSmall);
        }
        match algo {
            Algo::Sha256 => {
                let mut h = Sha256::new();
                h.update(input);
                out[..n].copy_from_slice(&h.finalize());
            }
            Algo::Sha384 => {
                let mut h = Sha384::new();
                h.update(input);
                out[..n].copy_from_slice(&h.finalize());
            }
            Algo::Sha512 => {
                let mut h = Sha512::new();
                h.update(input);
                out[..n].copy_from_slice(&h.finalize());
            }
        }
        Ok(n)
    }
}

// --- Mac (HMAC) -------------------------------------------------------------

/// Type alias documenting that `SoftCrypto` is the MAC provider.
pub type SoftMac = SoftCrypto;

impl SoftCrypto {
    /// Pull raw key bytes. In-process software is the one place `KeyRef::Raw`
    /// is the normal case (sketch §3.4); a `KeyRef::Sealed` handle refers to a
    /// hardware-sealed key slot and is meaningless without an engine, so it is
    /// rejected here (there is no engine in software).
    fn key_bytes<'a>(key: KeyRef<'a>) -> Result<&'a [u8], CryptoError> {
        match key {
            KeyRef::Raw(k) => Ok(k),
            KeyRef::Sealed(_) => Err(CryptoError::InvalidKey),
        }
    }
}

impl Mac for SoftCrypto {
    fn mac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        let k = Self::key_bytes(key)?;
        let n = algo.digest_len();
        if out.len() < n {
            return Err(CryptoError::OutputTooSmall);
        }
        match algo {
            Algo::Sha256 => {
                let mut m = <Hmac<Sha256> as hmac::Mac>::new_from_slice(k)
                    .map_err(|_| CryptoError::InvalidKey)?;
                m.update(input);
                out[..n].copy_from_slice(&m.finalize().into_bytes());
            }
            Algo::Sha384 => {
                let mut m = <Hmac<Sha384> as hmac::Mac>::new_from_slice(k)
                    .map_err(|_| CryptoError::InvalidKey)?;
                m.update(input);
                out[..n].copy_from_slice(&m.finalize().into_bytes());
            }
            Algo::Sha512 => {
                let mut m = <Hmac<Sha512> as hmac::Mac>::new_from_slice(k)
                    .map_err(|_| CryptoError::InvalidKey)?;
                m.update(input);
                out[..n].copy_from_slice(&m.finalize().into_bytes());
            }
        }
        Ok(n)
    }
}

// --- Cipher (AES-ECB / AES-CBC, 128 / 256) ----------------------------------

/// Type alias documenting that `SoftCrypto` is the cipher provider.
pub type SoftCipher = SoftCrypto;

const AES_BLOCK: usize = 16;

/// Run `f` over each 16-byte block in place. Real AES block math; the
/// ECB/CBC chaining is open-coded because no mode crate is vendored.
fn ecb_blocks(buf: &mut [u8], mut f: impl FnMut(&mut [u8; AES_BLOCK])) {
    for chunk in buf.chunks_exact_mut(AES_BLOCK) {
        let mut b = [0u8; AES_BLOCK];
        b.copy_from_slice(chunk);
        f(&mut b);
        chunk.copy_from_slice(&b);
    }
}

impl Cipher for SoftCrypto {
    fn crypt(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        if input.len() % AES_BLOCK != 0 {
            // delta A4: typed, rejected before any block work.
            return Err(CryptoError::InputNotBlockAligned);
        }
        if out.len() < input.len() {
            return Err(CryptoError::OutputTooSmall);
        }
        let k = Self::key_bytes(key)?;

        // Construct the real AES block cipher for the requested key size.
        enum Aes {
            K128(Aes128),
            K256(Aes256),
        }
        let cipher = match params.key_bits {
            AesKeyBits::Bits128 => {
                Aes::K128(Aes128::new_from_slice(k).map_err(|_| CryptoError::InvalidKey)?)
            }
            AesKeyBits::Bits256 => {
                Aes::K256(Aes256::new_from_slice(k).map_err(|_| CryptoError::InvalidKey)?)
            }
        };

        let n = input.len();
        out[..n].copy_from_slice(input);
        let buf = &mut out[..n];

        let enc = |b: &mut [u8; AES_BLOCK]| {
            let blk = aes::cipher::generic_array::GenericArray::from_mut_slice(b);
            match &cipher {
                Aes::K128(c) => c.encrypt_block(blk),
                Aes::K256(c) => c.encrypt_block(blk),
            }
        };
        let dec = |b: &mut [u8; AES_BLOCK]| {
            let blk = aes::cipher::generic_array::GenericArray::from_mut_slice(b);
            match &cipher {
                Aes::K128(c) => c.decrypt_block(blk),
                Aes::K256(c) => c.decrypt_block(blk),
            }
        };

        match (params.mode, params.dir) {
            (AesMode::Ecb, AesDir::Encrypt) => ecb_blocks(buf, enc),
            (AesMode::Ecb, AesDir::Decrypt) => ecb_blocks(buf, dec),
            (AesMode::Cbc, AesDir::Encrypt) => {
                let mut prev = params.iv;
                for chunk in buf.chunks_exact_mut(AES_BLOCK) {
                    let mut b = [0u8; AES_BLOCK];
                    for i in 0..AES_BLOCK {
                        b[i] = chunk[i] ^ prev[i];
                    }
                    {
                        let blk = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut b);
                        match &cipher {
                            Aes::K128(c) => c.encrypt_block(blk),
                            Aes::K256(c) => c.encrypt_block(blk),
                        }
                    }
                    chunk.copy_from_slice(&b);
                    prev = b;
                }
            }
            (AesMode::Cbc, AesDir::Decrypt) => {
                let mut prev = params.iv;
                for chunk in buf.chunks_exact_mut(AES_BLOCK) {
                    let ct: [u8; AES_BLOCK] = {
                        let mut t = [0u8; AES_BLOCK];
                        t.copy_from_slice(chunk);
                        t
                    };
                    let mut b = ct;
                    {
                        let blk = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut b);
                        match &cipher {
                            Aes::K128(c) => c.decrypt_block(blk),
                            Aes::K256(c) => c.decrypt_block(blk),
                        }
                    }
                    for i in 0..AES_BLOCK {
                        b[i] ^= prev[i];
                    }
                    chunk.copy_from_slice(&b);
                    prev = ct;
                }
            }
        }
        Ok(n)
    }
}

// --- Verify (ECDSA-P384 / RSA) — SKELETON STUB ------------------------------

/// Type alias documenting that `SoftCrypto` is the verify provider.
pub type SoftVerify = SoftCrypto;

impl Verify for SoftCrypto {
    // TODO(skeleton): real software impl. No ECDSA-P384 or RSA implementation
    // is vendored in `//third_party/crates_io` (`@rust_crates` exposes
    // `sha2`/`hmac`/`aes`/`cipher` only — no `p384`/`ecdsa`/`rsa`). The shape
    // is correct: per sketch §3.5 a failed verification is `Ok(false)`, a
    // *result*, never `Err`. This stub deliberately reports `Ok(false)`
    // (verification did not succeed) rather than inventing a fake "valid"
    // outcome — wiring real verification in must not change these signatures.
    fn ecdsa_p384_verify(
        &mut self,
        _pubkey: EcPubP384,
        _sig: EcSig,
        _digest48: &[u8; 48],
    ) -> Result<bool, CryptoError> {
        // TODO(skeleton): real software impl (P-384 ECDSA verify).
        Ok(false)
    }

    fn rsa_verify(
        &mut self,
        _key: RsaKey<'_>,
        _sig: &[u8],
        _expected: &[u8],
    ) -> Result<bool, CryptoError> {
        // TODO(skeleton): real software impl (RSA signature verify).
        Ok(false)
    }
}
