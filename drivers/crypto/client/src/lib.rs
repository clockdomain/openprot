// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! `crypto_client` — the IPC adapter that *implements* the `crypto_traits`
//! abstract seam (`crypto-driver-sketch.md` §3.7, ADR-C1).
//!
//! This crate is the crypto analog of `services/mctp`'s `IpcMctpClient`: it is
//! intentionally soaked in `userspace::syscall` / `pw_status`, and that IPC
//! coupling is *correct and expected here* — it is this crate's single
//! responsibility. The crate exposes itself **only** as a trait implementation:
//! [`CryptoClient`] `impl`s [`crypto_traits::Digest`], [`Mac`], [`Cipher`] and
//! [`Verify`]. Consumers depend on those traits (or on
//! `crypto_traits::Stack<B>`), never on this concrete type, and so cannot
//! become IPC-coupled even by accident — the property that makes the
//! wiring-time / boot-time backend substitution (sketch §3.3, §3.6) hold.
//!
//! All IPC request/response marshalling is **private** (private functions and
//! a private inherent `impl`). There are deliberately no public inherent
//! crypto-operation methods — that was the divergence the first scaffold
//! introduced and ADR-C1 forbids.
//!
//! Architectural constraints from `crypto-driver-sketch.md`:
//!
//! * **§3.1** — every operation is a *whole-object, run-to-completion* call.
//!   There is no `begin/update/finish`; the server completes the operation and
//!   releases the engine before replying.
//! * **§3.4** — a key only ever crosses the wire as an opaque sealed-key
//!   handle ([`KeyRef::Sealed`]), encoded into `header.key_handle`. Raw secret
//!   bytes are never sent; raw-key work belongs in in-process `crypto_soft`.
//! * **§3.5** — for public-key verify, a failed signature check is a *result*
//!   (server `CryptoError::VerifyFailed` → `Ok(false)`), never a transport
//!   error.

#![no_std]

use crypto_traits::{
    AesParams, Algo, Cipher, CryptoError, Digest, EcPubP384, EcSig, KeyRef, Mac, RsaKey, Verify,
};

use crypto_api::backend::{AesDir, AesKeyBits, AesMode};
use crypto_api::protocol::{
    CryptoError as WireError, CryptoRequestHeader, CryptoResponseHeader, Engine, PublicKeyOp,
    SymmetricOp, MAX_PAYLOAD_SIZE,
};

use userspace::syscall;
use userspace::time::Instant;

/// Request/response scratch size: header + a full in-band payload, plus slack
/// for fixed-size operand prefixes (IV, region descriptor, EC operands).
const MAX_BUF_SIZE: usize = CryptoRequestHeader::SIZE + MAX_PAYLOAD_SIZE + 256;

// ============================================================================
// Private internal error
// ============================================================================

/// Internal failure surface. This is **not** public: ADR-C1 requires the crate
/// present itself only through the `crypto_traits` traits, whose methods return
/// [`crypto_traits::CryptoError`]. This enum exists purely so the private
/// marshalling helpers can distinguish transport faults from server status, and
/// is collapsed into the trait error (or, for `Verify`, into `Ok(false)`)
/// before anything leaves this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpcError {
    /// Pigweed IPC transport fault (channel layer).
    Transport,
    /// Server replied with a non-`Success` crypto status.
    Server(WireError),
    /// Reply was malformed / truncated.
    InvalidResponse,
    /// A caller buffer (input or scratch) did not fit.
    BufferTooSmall,
}

impl From<pw_status::Error> for IpcError {
    fn from(_e: pw_status::Error) -> Self {
        Self::Transport
    }
}

/// Collapse an internal failure into the abstract trait error. `Verify`-style
/// `VerifyFailed` is handled by the caller *before* this (it is a result, not
/// an error — sketch §3.5); reaching here with it is a protocol misuse and is
/// reported as an engine fault.
fn to_trait_error(e: IpcError) -> CryptoError {
    match e {
        IpcError::Server(WireError::InvalidAlgo) => CryptoError::Unsupported,
        IpcError::Server(WireError::InvalidOperation) => CryptoError::Unsupported,
        IpcError::Server(WireError::InvalidKeyHandle) => CryptoError::InvalidKey,
        IpcError::Server(WireError::InputNotBlockAligned) => CryptoError::InputNotBlockAligned,
        IpcError::BufferTooSmall => CryptoError::OutputTooSmall,
        IpcError::Server(WireError::Busy)
        | IpcError::Server(WireError::Timeout)
        | IpcError::Server(WireError::EngineFault)
        | IpcError::Server(WireError::VerifyFailed)
        | IpcError::Server(WireError::Success)
        | IpcError::Server(WireError::InternalError)
        | IpcError::Transport
        | IpcError::InvalidResponse => CryptoError::EngineFault,
    }
}

// ============================================================================
// CryptoClient — the IPC adapter
// ============================================================================

/// IPC adapter to `crypto_server`. Holds only the IPC channel handle.
///
/// This type's *only* public surface is its `crypto_traits` trait
/// implementations ([`Digest`], [`Mac`], [`Cipher`], [`Verify`]); every method
/// below that does IPC is private. Construct one and hand it to
/// `crypto_traits::Stack::new(..)`, or use it behind a `&mut dyn Digest`
/// (etc.) — consumers never name this type.
pub struct CryptoClient {
    handle: u32,
}

impl CryptoClient {
    /// Wrap an IPC channel handle (typically a codegen `handle::CRYPTO`).
    pub const fn new(handle: u32) -> Self {
        Self { handle }
    }

    // ---- private IPC marshalling ------------------------------------------

    /// Encode `header || prefix || payload`, transact, copy the response
    /// payload into `out`, and return its length. Private: this is the
    /// marshalling ADR-C1 says must not be a public surface.
    fn transact(
        &self,
        hdr: &CryptoRequestHeader,
        prefix: &[u8],
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, IpcError> {
        let mut req = [0u8; MAX_BUF_SIZE];
        let mut n = 0;
        n += encode(&mut req, n, zerocopy::IntoBytes::as_bytes(hdr))?;
        n += encode(&mut req, n, prefix)?;
        n += encode(&mut req, n, payload)?;

        let mut resp = [0u8; MAX_BUF_SIZE];
        let resp_len =
            syscall::channel_transact(self.handle, &req[..n], &mut resp, Instant::MAX)?;
        parse_payload_response(&resp[..resp_len], out)
    }

    /// Like [`Self::transact`] but the reply carries no payload — only the
    /// status matters (public-key verify, sketch §3.5). Returns the raw server
    /// status so the caller can map `VerifyFailed → Ok(false)`.
    fn transact_status(
        &self,
        hdr: &CryptoRequestHeader,
        prefix: &[u8],
        payload: &[u8],
    ) -> Result<(), IpcError> {
        let mut req = [0u8; MAX_BUF_SIZE];
        let mut n = 0;
        n += encode(&mut req, n, zerocopy::IntoBytes::as_bytes(hdr))?;
        n += encode(&mut req, n, prefix)?;
        n += encode(&mut req, n, payload)?;

        let mut resp = [0u8; MAX_BUF_SIZE];
        let resp_len =
            syscall::channel_transact(self.handle, &req[..n], &mut resp, Instant::MAX)?;
        parse_no_payload_response(&resp[..resp_len])
    }
}

// ============================================================================
// crypto_traits::Digest
// ============================================================================

impl Digest for CryptoClient {
    fn digest(&mut self, algo: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, CryptoError> {
        if input.len() > MAX_PAYLOAD_SIZE {
            return Err(CryptoError::OutputTooSmall);
        }
        if out.len() < algo.digest_len() {
            return Err(CryptoError::OutputTooSmall);
        }
        let hdr = CryptoRequestHeader::new(
            Engine::Symmetric,
            SymmetricOp::HashOneShot as u8,
            algo as u8,
            0,
            0,
            input.len() as u16,
        );
        self.transact(&hdr, &[], input, out).map_err(to_trait_error)
    }
}

// ============================================================================
// crypto_traits::Mac
// ============================================================================

impl Mac for CryptoClient {
    fn mac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        // §3.4: only a sealed-key handle may cross the wire.
        let key_handle = sealed_handle(key)?;
        if input.len() > MAX_PAYLOAD_SIZE {
            return Err(CryptoError::OutputTooSmall);
        }
        if out.len() < algo.digest_len() {
            return Err(CryptoError::OutputTooSmall);
        }
        let hdr = CryptoRequestHeader::new(
            Engine::Symmetric,
            SymmetricOp::Hmac as u8,
            algo as u8,
            key_handle,
            0,
            input.len() as u16,
        );
        self.transact(&hdr, &[], input, out).map_err(to_trait_error)
    }
}

// ============================================================================
// crypto_traits::Cipher
// ============================================================================

impl Cipher for CryptoClient {
    fn crypt(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        if input.len() % 16 != 0 {
            // delta A4: typed, rejected before any IPC.
            return Err(CryptoError::InputNotBlockAligned);
        }
        // §3.4: only a sealed-key handle may cross the wire.
        let key_handle = sealed_handle(key)?;
        if input.len() > MAX_PAYLOAD_SIZE {
            return Err(CryptoError::OutputTooSmall);
        }
        if out.len() < input.len() {
            return Err(CryptoError::OutputTooSmall);
        }

        // `arg0` packs the AES selectors; the 16-byte IV is a payload prefix.
        let mode = match params.mode {
            AesMode::Ecb => 0u16,
            AesMode::Cbc => 1u16,
        };
        let dir = match params.dir {
            AesDir::Encrypt => 0u16,
            AesDir::Decrypt => 1u16,
        };
        let bits = match params.key_bits {
            AesKeyBits::Bits128 => 0u16,
            AesKeyBits::Bits256 => 1u16,
        };
        let arg0 = mode | (dir << 1) | (bits << 2);

        let hdr = CryptoRequestHeader::new(
            Engine::Symmetric,
            SymmetricOp::AesCrypt as u8,
            0,
            key_handle,
            arg0,
            input.len() as u16,
        );
        self.transact(&hdr, &params.iv, input, out)
            .map_err(to_trait_error)
    }
}

// ============================================================================
// crypto_traits::Verify
// ============================================================================

impl Verify for CryptoClient {
    fn ecdsa_p384_verify(
        &mut self,
        pubkey: EcPubP384,
        sig: EcSig,
        digest48: &[u8; 48],
    ) -> Result<bool, CryptoError> {
        // Payload: qx(48) || qy(48) || r(48) || s(48) || digest(48) = 240 B.
        let mut prefix = [0u8; 192];
        prefix[..48].copy_from_slice(&pubkey.qx);
        prefix[48..96].copy_from_slice(&pubkey.qy);
        prefix[96..144].copy_from_slice(&sig.r);
        prefix[144..192].copy_from_slice(&sig.s);

        let payload_len = (prefix.len() + digest48.len()) as u16;
        let hdr = CryptoRequestHeader::new(
            Engine::PublicKey,
            PublicKeyOp::EcdsaP384Verify as u8,
            0,
            0,
            0,
            payload_len,
        );

        match self.transact_status(&hdr, &prefix, digest48) {
            Ok(()) => Ok(true),
            // §3.5: a failed check is a normal cryptographic outcome.
            Err(IpcError::Server(WireError::VerifyFailed)) => Ok(false),
            Err(e) => Err(to_trait_error(e)),
        }
    }

    fn rsa_verify(
        &mut self,
        key: RsaKey<'_>,
        sig: &[u8],
        expected: &[u8],
    ) -> Result<bool, CryptoError> {
        // Payload: [mod_len:u16][sig_len:u16][exp_len:u16][expd_len:u16]
        //          || modulus || signature || exponent || expected
        let mut lens = [0u8; 8];
        lens[0..2].copy_from_slice(&(key.modulus.len() as u16).to_le_bytes());
        lens[2..4].copy_from_slice(&(sig.len() as u16).to_le_bytes());
        lens[4..6].copy_from_slice(&(key.exponent.len() as u16).to_le_bytes());
        lens[6..8].copy_from_slice(&(expected.len() as u16).to_le_bytes());

        let total =
            lens.len() + key.modulus.len() + sig.len() + key.exponent.len() + expected.len();
        if total > MAX_PAYLOAD_SIZE {
            return Err(CryptoError::OutputTooSmall);
        }

        let mut req = [0u8; MAX_BUF_SIZE];
        let hdr = CryptoRequestHeader::new(
            Engine::PublicKey,
            PublicKeyOp::RsaModexp as u8,
            0,
            0,
            // ExpSel::Public — verify uses the public exponent.
            0,
            total as u16,
        );

        let build = |req: &mut [u8]| -> Result<usize, IpcError> {
            let mut n = 0;
            n += encode(req, n, zerocopy::IntoBytes::as_bytes(&hdr))?;
            n += encode(req, n, &lens)?;
            n += encode(req, n, key.modulus)?;
            n += encode(req, n, sig)?;
            n += encode(req, n, key.exponent)?;
            n += encode(req, n, expected)?;
            Ok(n)
        };

        let n = match build(&mut req) {
            Ok(n) => n,
            Err(e) => return Err(to_trait_error(e)),
        };

        let mut resp = [0u8; MAX_BUF_SIZE];
        let resp_len =
            match syscall::channel_transact(self.handle, &req[..n], &mut resp, Instant::MAX) {
                Ok(l) => l,
                Err(_) => return Err(CryptoError::EngineFault),
            };

        match parse_no_payload_response(&resp[..resp_len]) {
            Ok(()) => Ok(true),
            // §3.5: a failed check is a normal cryptographic outcome.
            Err(IpcError::Server(WireError::VerifyFailed)) => Ok(false),
            Err(e) => Err(to_trait_error(e)),
        }
    }
}

// ============================================================================
// Private wire helpers
// ============================================================================

/// Enforce sketch §3.4: only an opaque sealed-key handle may cross the IPC
/// boundary. Raw secret bytes are rejected here — raw-key work belongs in
/// in-process `crypto_soft`.
fn sealed_handle(key: KeyRef<'_>) -> Result<u8, CryptoError> {
    match key {
        KeyRef::Sealed(h) => Ok(h),
        KeyRef::Raw(_) => Err(CryptoError::InvalidKey),
    }
}

/// Append `src` into `buf` at `off`, returning bytes written. Overflow of the
/// fixed scratch is reported as `BufferTooSmall` (never a panic).
fn encode(buf: &mut [u8], off: usize, src: &[u8]) -> Result<usize, IpcError> {
    let end = off.checked_add(src.len()).ok_or(IpcError::BufferTooSmall)?;
    if end > buf.len() {
        return Err(IpcError::BufferTooSmall);
    }
    buf[off..end].copy_from_slice(src);
    Ok(src.len())
}

fn parse_no_payload_response(resp: &[u8]) -> Result<(), IpcError> {
    if resp.len() < CryptoResponseHeader::SIZE {
        return Err(IpcError::InvalidResponse);
    }
    let hdr_bytes = &resp[..CryptoResponseHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, CryptoResponseHeader>::from_bytes(hdr_bytes).ok() else {
        return Err(IpcError::InvalidResponse);
    };
    if hdr.is_success() {
        Ok(())
    } else {
        Err(IpcError::Server(hdr.error_code()))
    }
}

fn parse_payload_response(resp: &[u8], out: &mut [u8]) -> Result<usize, IpcError> {
    if resp.len() < CryptoResponseHeader::SIZE {
        return Err(IpcError::InvalidResponse);
    }
    let hdr_bytes = &resp[..CryptoResponseHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, CryptoResponseHeader>::from_bytes(hdr_bytes).ok() else {
        return Err(IpcError::InvalidResponse);
    };
    if !hdr.is_success() {
        return Err(IpcError::Server(hdr.error_code()));
    }
    let len = hdr.payload_length();
    if len > out.len() || resp.len() < CryptoResponseHeader::SIZE + len {
        return Err(IpcError::InvalidResponse);
    }
    out[..len]
        .copy_from_slice(&resp[CryptoResponseHeader::SIZE..CryptoResponseHeader::SIZE + len]);
    Ok(len)
}
