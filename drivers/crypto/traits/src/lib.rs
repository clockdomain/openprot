// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! `crypto_traits` — the abstract substitution seam (`crypto-driver-sketch.md`
//! §3.3, ADR-C1).
//!
//! Consumers (SPDM / attestation / PFR / DICE / secure-boot) depend **only**
//! on this crate — the abstract `Digest`/`Mac`/`Cipher`/`Verify` traits plus
//! the generic [`Stack`] facade — and never name a backend. The binary later
//! wires either `crypto_soft` (in-process) or `crypto_client` (IPC → server);
//! that choice is invisible here (sketch §3.3, §3.6, §3.7).
//!
//! This mirrors `services/mctp` exactly: `mctp-api` ships the abstract
//! `MctpClient` trait *and* a generic `stack::Stack<C>` facade in one crate;
//! consumers depend on `mctp-api` only and never touch `IpcMctpClient`. Here
//! the trait bundle is [`CryptoBackend`] and the facade is [`Stack`].
//!
//! Design constraints baked into these signatures:
//!
//! * **Whole-object, run-to-completion only** — every operation takes the full
//!   input and produces the full output in one call. There is deliberately no
//!   `begin`/`update`/`finish`: a held-open session would recreate the
//!   unsupported held-across-yield engine lock (sketch §3.1).
//! * **Algorithm selection reuses `crypto_api::Algo`** — not redefined here.
//! * **Verify-failure is a *result*, not an error** — `Verify` returns
//!   `Result<bool, _>`; `Ok(false)` is a clean "did not verify", never
//!   conflated with a transport/engine fault (sketch §3.5).

#![no_std]

pub mod stack;

pub use crypto_api::backend::{AesParams, EcPubP384, EcSig, KeyRef, RsaKey};
pub use crypto_api::protocol::Algo;

pub use stack::Stack;

/// Error surfaced by the abstract traits.
///
/// This is the consumer-facing error; it is intentionally backend-agnostic.
/// Note `Verify` does **not** use this for a failed signature check — see
/// [`Verify`] (sketch §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// The requested operation/algorithm is not supported by this backend.
    Unsupported,
    /// A key handle or key material was invalid.
    InvalidKey,
    /// AES input was not a whole number of blocks (delta A4).
    InputNotBlockAligned,
    /// Caller-provided output buffer was too small.
    OutputTooSmall,
    /// Engine/computation fault.
    EngineFault,
}

/// One-shot cryptographic hash.
///
/// `digest` consumes the entire `input` and writes the full digest into `out`,
/// returning the number of bytes written (`algo.digest_len()`). No streaming
/// state crosses the call boundary (sketch §3.1).
pub trait Digest {
    fn digest(&mut self, algo: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, CryptoError>;
}

/// One-shot keyed MAC (HMAC).
///
/// Whole-object: `key` + full `input` in, full MAC out. `key` is a
/// [`KeyRef`]; over an IPC backend only `KeyRef::Sealed` is legal (sketch
/// §3.4), but that constraint is enforced by the wiring, not this trait.
pub trait Mac {
    fn mac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError>;
}

/// One-shot AES (ECB/CBC, 128/256), encrypt or decrypt.
///
/// `params` carries mode/direction/key-size/IV. `input` must already be a
/// whole number of 16-byte blocks (no padding is applied here); a non-aligned
/// input is rejected as [`CryptoError::InputNotBlockAligned`] (delta A4).
pub trait Cipher {
    fn crypt(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError>;
}

/// Signature verification (ECDSA-P384 / RSA).
///
/// **The boolean is the cryptographic verdict, not an error** (sketch §3.5):
/// `Ok(true)` = signature verified, `Ok(false)` = signature did *not* verify
/// (a normal outcome — e.g. a forged blob). `Err(..)` is reserved for a
/// genuine engine/transport fault and must never be returned for a bad
/// signature.
pub trait Verify {
    /// ECDSA over NIST P-384 against a pre-computed 48-byte digest.
    fn ecdsa_p384_verify(
        &mut self,
        pubkey: EcPubP384,
        sig: EcSig,
        digest48: &[u8; 48],
    ) -> Result<bool, CryptoError>;

    /// RSA signature verification. `key` is the public key (modulus +
    /// exponent); `sig` is the raw signature; `expected` is the message hash /
    /// padded block the consumer expects to recover.
    fn rsa_verify(
        &mut self,
        key: RsaKey<'_>,
        sig: &[u8],
        expected: &[u8],
    ) -> Result<bool, CryptoError>;
}

/// The full crypto capability bundle: a single supertrait that ties the four
/// abstract capabilities together.
///
/// **Design choice — supertrait bundle over individual generic bounds.**
/// `Stack` is generic over one parameter `B: CryptoBackend` rather than four
/// separate bounds (`B: Digest + Mac + Cipher + Verify`). Rationale:
///
/// * It mirrors `mctp-api`, where `Stack<C: MctpClient>` is parameterised by
///   exactly one trait — a single named seam, not an ad-hoc bound list. A
///   backend is, by definition, "the thing that provides all crypto", so one
///   name for that contract is the honest abstraction.
/// * Both real backends (`crypto_soft`'s `SoftCrypto`, `crypto_client`'s IPC
///   adapter) provide *every* capability, so there is no partial-backend case
///   that individual bounds would buy us.
/// * The [blanket impl](#impl-CryptoBackend-for-T) means any type implementing
///   all four sub-traits automatically *is* a `CryptoBackend`; backends never
///   write `impl CryptoBackend` by hand and a test mock costs nothing extra.
pub trait CryptoBackend: Digest + Mac + Cipher + Verify {}

/// Blanket impl: anything that provides all four capabilities is a
/// [`CryptoBackend`]. Backends implement only the individual sub-traits.
impl<T: Digest + Mac + Cipher + Verify> CryptoBackend for T {}
