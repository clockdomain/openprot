// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 platform crypto backend — the **only** crate that names silicon.
//!
//! One platform crate, **both** capability-class backend traits from the
//! agnostic contract (`crypto_api::backend`; sketch §1, §3.2, §3.4, §5,
//! ADR-C2). The class→block binding lives here and *only* here:
//!
//! * `Symmetric → HACE` ([`SymmetricBackend`]) — wired to the real
//!   borrow-arbitrated [`HaceDevice`](ast10x0_peripherals::hace::HaceDevice)
//!   port in this worktree. Hash / HMAC / AES go through the actual
//!   `from_device` `&mut` borrow-split, so HACE engine exclusivity stays
//!   *structural* (a borrow-check fact, not a runtime flag — sketch §3.2).
//! * `PublicKey → SBC` ([`PublicKeyBackend`]) — a compiling **stub**. The SBC
//!   (ECDSA/RSA) peripheral port lives on branch `ast10x0-ecdsa` and is not
//!   present on this branch; every op returns `Err(BackendError::InternalError)`
//!   until that port is merged (see the prominent `TODO(pk)` on the impl).
//!
//! The single crypto server process owns both engines; this crate exports
//! `pub type SymmetricBackend` and `pub type PublicKeyBackend` for the
//! server binary's compile-time backend selection, mirroring the usart
//! backend's `pub type Backend`.

#![no_std]

use ast10x0_peripherals::hace::{AesCipher, HaceDevice, HaceDigest, HaceError, HaceHmac, HmacKey};
use crypto_api::backend::{
    AesDir, AesKeyBits, AesMode, AesParams, BackendError, EcPubP384, EcSig, ExpSel, KeyRef,
    PublicKeyBackend as PublicKeyBackendTrait, RegionDescriptor, RsaKey, Segment,
    SymmetricBackend as SymmetricBackendTrait,
};
use crypto_api::protocol::Algo;
use openprot_hal_blocking::digest::scoped::{DigestInit, DigestOp};
use openprot_hal_blocking::digest::{Sha2_256, Sha2_384, Sha2_512};
use openprot_hal_blocking::mac::scoped::{MacInit, MacOp};
use openprot_hal_blocking::mac::{HmacSha2_256, HmacSha2_384, HmacSha2_512};
// `Digest`/`Mac` outputs expose `.as_bytes()` via the trait bounds already in
// scope through `openprot_hal_blocking`; no direct `zerocopy` dependency needed.

/// Cooperative yield used by the HACE completion polls. The crypto server runs
/// the dispatch loop to completion before replying (sketch §3.1); a spin hint
/// is the budget-bounded poll filler the borrow-arbitrated device expects.
fn hace_yield(_ns: u32) {
    core::hint::spin_loop();
}

/// Map the low-level HACE port error onto the protocol-facing
/// [`BackendError`] (which in turn maps 1-to-1 onto `CryptoError`).
fn map_hace_err(e: HaceError) -> BackendError {
    match e {
        HaceError::Timeout => BackendError::Timeout,
        HaceError::InvalidInput => BackendError::InvalidOperation,
        // Any remaining low-level fault is an engine fault from the
        // protocol's point of view.
        _ => BackendError::EngineFault,
    }
}

// ===== Symmetric capability → HACE (real, borrow-arbitrated) =============

/// Platform symmetric backend. Owns the one borrow-arbitrated [`HaceDevice`]
/// singleton (sketch §5): every op obtains its `HaceDigest`/`HaceHmac`/
/// `AesCipher` via the `&mut` borrow-split, so two concurrent HACE ops are a
/// borrow-check error, not a runtime race (delta A1 /
/// `design-patterns :: borrow-arbitrated-engine-exclusivity`).
///
/// `Y` is fixed to a bare `fn(u32)` so the device type is nameable for the
/// `pub type` export while still satisfying `HaceDevice<Y: FnMut(u32)>`.
pub struct HaceSymmetricBackend {
    dev: HaceDevice<fn(u32)>,
}

impl HaceSymmetricBackend {
    /// Bind the backend to the singleton HACE engine.
    ///
    /// Safe to call exactly once per process: the single crypto server loop is
    /// the sole owner of the HACE MMIO + DMA-context mapping (sketch §3.2).
    /// Calling it more than once violates the device's documented
    /// single-instance contract.
    pub fn new() -> Self {
        // SAFETY: the crypto server process is the sole owner of the HACE
        // singleton (one mapping per the platform `system.json5`); the
        // borrow-arbitrated device upholds engine exclusivity from here on.
        let dev = unsafe { HaceDevice::<fn(u32)>::new_global(hace_yield as fn(u32)) };
        Self { dev }
    }
}

impl Default for HaceSymmetricBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable alias the crypto server binary names for compile-time selection
/// (mirrors the usart backend's `pub type Backend`). `Symmetric → HACE`
/// (ADR-C2): this binding exists in this crate only.
pub type SymmetricBackend = HaceSymmetricBackend;

/// One-shot digest of `input` for the given algorithm via the real verified
/// [`HaceDigest`] `from_device` borrow-split path. Writes the canonical-length
/// digest into `out`.
macro_rules! digest_one_shot {
    ($self:expr, $inner:ty, $marker:expr, $algo:expr, $input:expr, $out:expr) => {{
        let need = $algo.digest_len();
        if $out.len() < need {
            return Err(BackendError::InternalError);
        }
        // SAFETY: this backend owns the singleton device and the server loop
        // is single-threaded / run-to-completion (sketch §3.1/§3.2); no
        // concurrent or reentrant HACE access exists for the op's lifetime.
        let mut dd = unsafe { HaceDigest::<$inner>::from_device(&mut $self.dev) };
        let mut op = dd.init($marker).map_err(map_hace_err)?;
        op.update($input).map_err(map_hace_err)?;
        let result = op.finalize().map_err(map_hace_err)?;
        let bytes = result.as_bytes();
        $out[..need].copy_from_slice(&bytes[..need]);
        Ok(need)
    }};
}

impl SymmetricBackendTrait for HaceSymmetricBackend {
    fn hash(&mut self, algo: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError> {
        // REAL: drives the actual borrow-arbitrated HaceDigest path.
        match algo {
            Algo::Sha256 => digest_one_shot!(self, Sha2_256, Sha2_256, algo, input, out),
            Algo::Sha384 => digest_one_shot!(self, Sha2_384, Sha2_384, algo, input, out),
            Algo::Sha512 => digest_one_shot!(self, Sha2_512, Sha2_512, algo, input, out),
        }
    }

    fn hash_region(
        &mut self,
        algo: Algo,
        region: RegionDescriptor,
        out: &mut [u8],
    ) -> Result<usize, BackendError> {
        // REAL (engine path) / skeleton (region resolution): the real server
        // resolves the region to a server-owned slice and runs the page loop
        // itself (sketch §3.1); the engine only ever sees a byte slice. Here we
        // reconstruct that slice from the descriptor and feed it through the
        // same real digest path as `hash`.
        if region.len == 0 {
            return Err(BackendError::InvalidOperation);
        }
        // TODO(skeleton): wire to real HaceDevice — in the full server the
        // RegionDescriptor is validated against the process's mapped, read-only
        // measured-image regions (DICE/PFR, sketch §3.1) before dispatch. The
        // skeleton trusts the descriptor it is handed and forms the slice
        // directly so the real engine path below is still exercised.
        // SAFETY(skeleton): the descriptor names a server-owned, mapped,
        // read-only region; the real server validates the mapping pre-dispatch.
        let bytes = unsafe {
            core::slice::from_raw_parts(region.base as usize as *const u8, region.len as usize)
        };
        self.hash(algo, bytes, out)
    }

    fn hash_sg(
        &mut self,
        algo: Algo,
        segments: &[Segment<'_>],
        out: &mut [u8],
    ) -> Result<usize, BackendError> {
        // REAL: the HaceDigest `update` loop natively handles multi-segment
        // input (SG table). Feed each complete segment in order through the
        // real borrow-split path, then finalize once.
        if segments.is_empty() {
            return Err(BackendError::InvalidOperation);
        }
        macro_rules! sg_run {
            ($inner:ty, $marker:expr) => {{
                let need = algo.digest_len();
                if out.len() < need {
                    return Err(BackendError::InternalError);
                }
                // SAFETY: see `digest_one_shot!`; single-threaded server loop.
                let mut dd = unsafe { HaceDigest::<$inner>::from_device(&mut self.dev) };
                let mut op = dd.init($marker).map_err(map_hace_err)?;
                for seg in segments {
                    op.update(seg.data).map_err(map_hace_err)?;
                }
                let result = op.finalize().map_err(map_hace_err)?;
                let b = result.as_bytes();
                out[..need].copy_from_slice(&b[..need]);
                Ok(need)
            }};
        }
        match algo {
            Algo::Sha256 => sg_run!(Sha2_256, Sha2_256),
            Algo::Sha384 => sg_run!(Sha2_384, Sha2_384),
            Algo::Sha512 => sg_run!(Sha2_512, Sha2_512),
        }
    }

    fn hmac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, BackendError> {
        // REAL: drives the actual software-over-HACE HaceHmac path.
        //
        // The HACE HMAC port is raw-key only; an opaque hardware-sealed handle
        // has no HMAC binding in this port (the OTP sideload is an AES-only
        // path). A `Sealed` handle is therefore a typed key-handle rejection,
        // not an engine fault (sketch §3.4).
        let key_bytes = match key {
            KeyRef::Raw(k) => k,
            KeyRef::Sealed(_) => return Err(BackendError::InvalidKeyHandle),
        };
        let need = algo.digest_len();
        if out.len() < need {
            return Err(BackendError::InternalError);
        }
        let hkey = HmacKey::from_slice(key_bytes).map_err(map_hace_err)?;
        // SAFETY: this backend owns the singleton device; single-threaded,
        // run-to-completion server loop — no reentrant HACE access.
        let mut hm = unsafe { HaceHmac::from_device(&mut self.dev) };
        macro_rules! hmac_run {
            ($mac:ty, $marker:expr) => {{
                let mut op =
                    MacInit::<$mac>::init(&mut hm, $marker, hkey).map_err(map_hace_err)?;
                op.update(input).map_err(map_hace_err)?;
                let tag = op.finalize().map_err(map_hace_err)?;
                let b = tag.as_bytes();
                out[..need].copy_from_slice(&b[..need]);
                Ok(need)
            }};
        }
        match algo {
            Algo::Sha256 => hmac_run!(HmacSha2_256, HmacSha2_256),
            Algo::Sha384 => hmac_run!(HmacSha2_384, HmacSha2_384),
            Algo::Sha512 => hmac_run!(HmacSha2_512, HmacSha2_512),
        }
    }

    fn aes(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, BackendError> {
        // REAL: drives the actual borrow-arbitrated AesCipher path.
        //
        // The vault/OTP sideload-key crypto end-to-end is hardware-gated and
        // out of scope for this port (goal.md §2.6 / aes.rs delta A6): only the
        // raw-key path is exercised here. A `Sealed` handle (the over-the-wire
        // sideload form, sketch §3.4) is a typed key-handle rejection until the
        // hardware-gated vault path is bound.
        let key_bytes = match key {
            KeyRef::Raw(k) => k,
            KeyRef::Sealed(_) => return Err(BackendError::InvalidKeyHandle),
        };
        // The port already rejects a non-block-multiple input with a typed
        // error (aes.rs delta A4); surface that pre-engine as the typed
        // protocol status rather than a generic fault.
        if input.is_empty() || input.len() % 16 != 0 {
            return Err(BackendError::InputNotBlockAligned);
        }
        match params.key_bits {
            AesKeyBits::Bits128 if key_bytes.len() != 16 => {
                return Err(BackendError::InvalidKeyHandle);
            }
            AesKeyBits::Bits256 if key_bytes.len() != 32 => {
                return Err(BackendError::InvalidKeyHandle);
            }
            _ => {}
        }
        if out.len() < input.len() {
            return Err(BackendError::InternalError);
        }
        // SAFETY: this backend owns the singleton device; single-threaded,
        // run-to-completion server loop — no reentrant HACE access.
        let mut aes = unsafe { AesCipher::from_device(&mut self.dev) };
        let encrypt = matches!(params.dir, AesDir::Encrypt);
        let res = match params.mode {
            AesMode::Ecb => {
                if encrypt {
                    aes.ecb_encrypt(key_bytes, input, out)
                } else {
                    aes.ecb_decrypt(key_bytes, input, out)
                }
            }
            AesMode::Cbc => {
                if encrypt {
                    aes.cbc_encrypt(key_bytes, &params.iv, input, out)
                } else {
                    aes.cbc_decrypt(key_bytes, &params.iv, input, out)
                }
            }
        };
        res.map_err(map_hace_err)?;
        Ok(input.len())
    }
}

// ===== PublicKey capability → SBC (compiling stub) ======================

/// Platform public-key backend — **compiling stub** (`PublicKey → SBC`,
/// ADR-C2).
///
/// TODO(pk): SBC port lives on branch ast10x0-ecdsa; bind when merged.
///
/// The ECDSA/RSA (SBC) peripheral port does not exist on this branch; it is
/// developed independently and merged later. Until then every public-key op is
/// a clean `Err(BackendError::InternalError)` so the single crypto server
/// (which owns both engines, sketch §1) still links and routes.
///
/// Note: once wired, a *verify* op that genuinely fails its signature check
/// must return `Ok(false)` — **not** an error — per sketch §3.5. The stub
/// cannot perform that check, so it reports the not-yet-wired condition as an
/// engine-side `InternalError` rather than masquerading as `Ok(false)`.
pub struct SbcPublicKeyBackend {
    _private: (),
}

impl SbcPublicKeyBackend {
    /// Construct the stub SBC public-key backend.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for SbcPublicKeyBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable alias the crypto server binary names for compile-time selection.
/// `PublicKey → SBC` (ADR-C2); this binding exists in this crate only.
pub type PublicKeyBackend = SbcPublicKeyBackend;

impl PublicKeyBackendTrait for SbcPublicKeyBackend {
    fn ecdsa_p384_verify(
        &mut self,
        _pubkey: EcPubP384,
        _sig: EcSig,
        _digest48: &[u8; 48],
    ) -> Result<bool, BackendError> {
        // TODO(pk): SBC port lives on branch ast10x0-ecdsa; bind when merged.
        Err(BackendError::InternalError)
    }

    fn rsa_modexp(
        &mut self,
        _exp: ExpSel,
        _key: RsaKey<'_>,
        _input: &[u8],
        _out: &mut [u8],
    ) -> Result<usize, BackendError> {
        // TODO(pk): SBC port lives on branch ast10x0-ecdsa; bind when merged.
        Err(BackendError::InternalError)
    }
}
