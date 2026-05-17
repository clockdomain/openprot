// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Crypto server — protocol→backend translator.
//!
//! ONE server process, TWO capability classes, routed by `header.engine`
//! (`crypto-driver-sketch.md` §1, §4). Every request is a whole-object,
//! run-to-completion operation: there is deliberately no `PendingRead`, no
//! `DispatchOutcome::Queued`, and no IRQ-park branch (sketch §3.1, §3.2).
//! `dispatch_request` therefore always produces a response and just returns
//! its length — the runtime always calls `channel_respond`.

#![no_std]

pub mod runtime;

use crypto_api::backend::{
    AesDir, AesKeyBits, AesMode, AesParams, EcPubP384, EcSig, ExpSel, KeyRef, PublicKeyBackend,
    RsaKey, SymmetricBackend,
};
use crypto_api::protocol::{
    Algo, CryptoError, CryptoRequestHeader, CryptoResponseHeader, Engine, PublicKeyOp, SymmetricOp,
};

pub const MAX_REQUEST_SIZE: usize = 2048;
pub const MAX_RESPONSE_SIZE: usize = 2048;

/// Translate one whole-object request into a response, calling the matching
/// capability backend. Returns the number of bytes written into `response`.
///
/// Routing is by `header.engine`: symmetric-class ops
/// (`HashOneShot`/`HashRegion`/`HashSg`/`Hmac`/`AesCrypt`) go to `sym`,
/// public-key-class ops (`EcdsaP384Verify`/`RsaModexp`) go to `pk`. The
/// per-class op enum is decoded only after the class is known (the op space
/// is namespaced per class, sketch §4). No IPC, no parking — pure protocol
/// translation.
pub fn dispatch_request<Y: SymmetricBackend, P: PublicKeyBackend>(
    sym: &mut Y,
    pk: &mut P,
    request: &[u8],
    response: &mut [u8],
) -> usize {
    if request.len() < CryptoRequestHeader::SIZE {
        return encode_error(response, CryptoError::InvalidOperation);
    }

    let hdr_bytes = &request[..CryptoRequestHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, CryptoRequestHeader>::from_bytes(hdr_bytes).ok() else {
        return encode_error(response, CryptoError::InvalidOperation);
    };

    let engine = match hdr.engine() {
        Ok(e) => e,
        Err(e) => return encode_error(response, e),
    };

    let payload_len = hdr.payload_length();
    if request.len() < CryptoRequestHeader::SIZE + payload_len {
        return encode_error(response, CryptoError::InvalidOperation);
    }
    let payload = &request[CryptoRequestHeader::SIZE..CryptoRequestHeader::SIZE + payload_len];

    match engine {
        Engine::Symmetric => dispatch_symmetric(sym, &hdr, payload, response),
        Engine::PublicKey => dispatch_public_key(pk, &hdr, payload, response),
    }
}

/// Symmetric-class arm: digest / HMAC / AES, all run-to-completion.
/// `HashRegion`'s page loop lives inside the backend (sketch §3.1) — the
/// server just hands it the descriptor.
fn dispatch_symmetric<Y: SymmetricBackend>(
    sym: &mut Y,
    hdr: &CryptoRequestHeader,
    payload: &[u8],
    response: &mut [u8],
) -> usize {
    let op = match SymmetricOp::try_from(hdr.op) {
        Ok(op) => op,
        Err(e) => return encode_error(response, e),
    };

    // Digest output lands directly in the response payload region.
    let out_off = CryptoResponseHeader::SIZE;
    let out_cap = response.len().saturating_sub(out_off);

    match op {
        SymmetricOp::HashOneShot => {
            let algo = match hdr.algorithm() {
                Ok(a) => a,
                Err(e) => return encode_error(response, e),
            };
            run_hash(response, out_off, out_cap, |out| sym.hash(algo, payload, out))
        }
        SymmetricOp::HashRegion => {
            let algo = match hdr.algorithm() {
                Ok(a) => a,
                Err(e) => return encode_error(response, e),
            };
            let region = match decode_region(payload) {
                Ok(r) => r,
                Err(e) => return encode_error(response, e),
            };
            run_hash(response, out_off, out_cap, |out| {
                sym.hash_region(algo, region, out)
            })
        }
        SymmetricOp::HashSg => {
            let algo = match hdr.algorithm() {
                Ok(a) => a,
                Err(e) => return encode_error(response, e),
            };
            // Single in-band segment: the wire carries one complete payload;
            // multi-segment gather is a region/SG-list extension point (§8).
            let segments = [crypto_api::backend::Segment { data: payload }];
            run_hash(response, out_off, out_cap, |out| {
                sym.hash_sg(algo, &segments, out)
            })
        }
        SymmetricOp::Hmac => {
            let algo = match hdr.algorithm() {
                Ok(a) => a,
                Err(e) => return encode_error(response, e),
            };
            let key = KeyRef::Sealed(hdr.key_handle);
            run_hash(response, out_off, out_cap, |out| {
                sym.hmac(algo, key, payload, out)
            })
        }
        SymmetricOp::AesCrypt => {
            let params = match decode_aes_params(hdr, payload) {
                Ok(p) => p,
                Err(e) => return encode_error(response, e),
            };
            let (aes, data) = params;
            let key = KeyRef::Sealed(hdr.key_handle);
            match sym.aes(aes, key, data, &mut response[out_off..out_off + out_cap]) {
                Ok(n) => finish_success(response, n),
                Err(e) => encode_error(response, e.into()),
            }
        }
    }
}

/// Public-key-class arm. A failed signature/verify is a *cryptographic
/// result* (`Ok(false)` → `CryptoError::VerifyFailed`), never
/// `InternalError` (sketch §3.5).
fn dispatch_public_key<P: PublicKeyBackend>(
    pk: &mut P,
    hdr: &CryptoRequestHeader,
    payload: &[u8],
    response: &mut [u8],
) -> usize {
    let op = match PublicKeyOp::try_from(hdr.op) {
        Ok(op) => op,
        Err(e) => return encode_error(response, e),
    };

    match op {
        PublicKeyOp::EcdsaP384Verify => {
            // payload = qx[48] || qy[48] || r[48] || s[48] || digest[48] = 240
            const N: usize = 48;
            if payload.len() < N * 5 {
                return encode_error(response, CryptoError::InvalidOperation);
            }
            let mut pubkey = EcPubP384 {
                qx: [0u8; N],
                qy: [0u8; N],
            };
            let mut sig = EcSig {
                r: [0u8; N],
                s: [0u8; N],
            };
            let mut digest48 = [0u8; N];
            pubkey.qx.copy_from_slice(&payload[0..N]);
            pubkey.qy.copy_from_slice(&payload[N..2 * N]);
            sig.r.copy_from_slice(&payload[2 * N..3 * N]);
            sig.s.copy_from_slice(&payload[3 * N..4 * N]);
            digest48.copy_from_slice(&payload[4 * N..5 * N]);

            match pk.ecdsa_p384_verify(pubkey, sig, &digest48) {
                Ok(true) => finish_success(response, 0),
                // §3.5: a clean "did not verify" is a result, not a fault.
                Ok(false) => encode_error(response, CryptoError::VerifyFailed),
                Err(e) => encode_error(response, e.into()),
            }
        }
        PublicKeyOp::RsaModexp => {
            // arg0 = modulus length in bytes; payload = modulus || exponent || input
            let exp = if hdr.key_handle == 0 {
                ExpSel::Public
            } else {
                ExpSel::Private
            };
            let mod_len = hdr.arg0_value() as usize;
            if payload.len() < mod_len {
                return encode_error(response, CryptoError::InvalidOperation);
            }
            // Layout: modulus[mod_len] || exponent[mod_len] || input[rest].
            if payload.len() < 2 * mod_len {
                return encode_error(response, CryptoError::InvalidOperation);
            }
            let modulus = &payload[..mod_len];
            let exponent = &payload[mod_len..2 * mod_len];
            let input = &payload[2 * mod_len..];
            let key = RsaKey { modulus, exponent };

            let out_off = CryptoResponseHeader::SIZE;
            let out_cap = response.len().saturating_sub(out_off);
            match pk.rsa_modexp(exp, key, input, &mut response[out_off..out_off + out_cap]) {
                Ok(n) => finish_success(response, n),
                Err(e) => encode_error(response, e.into()),
            }
        }
    }
}

/// Run a digest-producing backend call whose output is written straight into
/// the response payload region, then stamp the success header.
fn run_hash<F>(response: &mut [u8], out_off: usize, out_cap: usize, f: F) -> usize
where
    F: FnOnce(&mut [u8]) -> Result<usize, crypto_api::backend::BackendError>,
{
    match f(&mut response[out_off..out_off + out_cap]) {
        Ok(n) => finish_success(response, n),
        Err(e) => encode_error(response, e.into()),
    }
}

/// `RegionDescriptor` wire form: `base:u64 || len:u32`, little-endian.
fn decode_region(payload: &[u8]) -> Result<crypto_api::backend::RegionDescriptor, CryptoError> {
    if payload.len() < 12 {
        return Err(CryptoError::InvalidOperation);
    }
    let base = u64::from_le_bytes(payload[0..8].try_into().unwrap());
    let len = u32::from_le_bytes(payload[8..12].try_into().unwrap());
    Ok(crypto_api::backend::RegionDescriptor { base, len })
}

/// Decode AES params from the header (`arg0` packs mode/dir/key-bits) and
/// split the IV (first 16 bytes) from the data. Returns `(AesParams, data)`
/// where `params.iv` is populated; the slice returned is the plaintext/
/// ciphertext only.
///
/// `arg0` bit layout: bit0 = mode (0 ECB / 1 CBC), bit1 = dir (0 enc / 1
/// dec), bit2 = key bits (0 = 128 / 1 = 256).
fn decode_aes_params<'a>(
    hdr: &CryptoRequestHeader,
    payload: &'a [u8],
) -> Result<(AesParams, &'a [u8]), CryptoError> {
    if payload.len() < 16 {
        return Err(CryptoError::InvalidOperation);
    }
    let arg0 = hdr.arg0_value();
    let mode = match arg0 & 0x1 {
        0 => AesMode::Ecb,
        _ => AesMode::Cbc,
    };
    let dir = match (arg0 >> 1) & 0x1 {
        0 => AesDir::Encrypt,
        _ => AesDir::Decrypt,
    };
    let key_bits = match (arg0 >> 2) & 0x1 {
        0 => AesKeyBits::Bits128,
        _ => AesKeyBits::Bits256,
    };
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&payload[..16]);
    let data = &payload[16..];
    // AES delta A4: input must be a 16-byte block multiple — typed, rejected
    // pre-engine.
    if !data.len().is_multiple_of(16) {
        return Err(CryptoError::InputNotBlockAligned);
    }
    Ok((
        AesParams {
            mode,
            dir,
            key_bits,
            iv,
        },
        data,
    ))
}

/// Stamp a success response header in front of `payload_len` bytes already
/// written at `CryptoResponseHeader::SIZE`.
fn finish_success(response: &mut [u8], payload_len: usize) -> usize {
    let hdr = CryptoResponseHeader::success(payload_len as u16);
    response[..CryptoResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    CryptoResponseHeader::SIZE + payload_len
}

/// Encode a header-only error response (sketch §3.5: `VerifyFailed` is one of
/// these, a distinct status — never silently turned into `InternalError`).
pub fn encode_error(response: &mut [u8], error: CryptoError) -> usize {
    let hdr = CryptoResponseHeader::error(error);
    response[..CryptoResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    CryptoResponseHeader::SIZE
}

/// Encode a success response by copying `payload` after the header. Used when
/// the payload is not produced in place.
pub fn encode_success(response: &mut [u8], payload: &[u8]) -> usize {
    let hdr = CryptoResponseHeader::success(payload.len() as u16);
    response[..CryptoResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    response[CryptoResponseHeader::SIZE..CryptoResponseHeader::SIZE + payload.len()]
        .copy_from_slice(payload);
    CryptoResponseHeader::SIZE + payload.len()
}

// Keep the `Algo` import meaningful even though it is only used via the header
// helper — silences an unused-import on toolchains that don't see through it.
const _: fn(Algo) = |_a: Algo| {};
