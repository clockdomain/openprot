// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AES over the HACE crypto sub-engine (AES-128/256, ECB + CBC, raw key).
//!
//! Behavioral authority: the pinned Zephyr `aspeed_hace` unified driver —
//! `zephyr-reference/{hace_aspeed.c,hace_aspeed.h,crypto_aspeed_priv.h}` @
//! `cfe94dc`; see `plans/goal.md` §1.9 (reference behavior), §2.3 (deltas),
//! §2.4 (NIST correctness authority). Correctness is gated by NIST
//! AESAVS/CAVP KATs, not by byte-parity with the driver's `IV ‖ CT` framing
//! (delta A2).
//!
//! API shape: ADR-A1 (goal.md §3 item 6) — this is the **trait-free,
//! slice-based driver core** on the borrow-arbitrated op, exactly as
//! `SbcOp::{verify_raw,modexp}`. `AesCipher` is obtained *only* by an
//! exclusive `&mut HaceDevice` borrow-split, so AES (the engine's third
//! operation) cannot overlap a digest/HMAC op — a borrow-check error, not a
//! runtime `in_use` flag and not a HW busy-bit read (delta A1 /
//! `design-patterns :: borrow-arbitrated-engine-exclusivity`).
//!
//! Intentional deltas vs. the driver (goal.md §2.3): **A2** the ciphertext
//! buffer is plain `CT` (no in-band IV prefix/strip — the IV is a separate
//! argument, matching the openprot cipher trait shape); **A3** the key/IV
//! region of the `.ram_nc` context is zeroized after every op and on drop;
//! **A4** a non-block-multiple input is rejected with a typed `InvalidInput`
//! before the engine is programmed (a bound the C omits). CFB/OFB/CTR,
//! AES-192, and DES/TDES are out of scope by decision (delta A5). The
//! OTP/secret-vault sideload-key path is **split (delta A6)**: the
//! handle→slot-select/command-word logic is in scope here (bit-exact vs. the
//! frozen source by compile-time `const` assertions in `constants.rs`); the
//! vault crypto end-to-end is separated and hardware-gated (goal.md §2.6).

use super::constants::{
    AES_CMD_BASE, HACE_CMD_AES128, HACE_CMD_AES256, HACE_CMD_CBC, HACE_CMD_ECB, HACE_CMD_ENCRYPT,
    HACE_SG_LAST, POLL_YIELD_NS,
};
use super::context::CryptoContext;
use super::device::HaceDevice;
use super::error::HaceError;
use super::helpers::ptr_to_u32;
use super::registers::HaceRegisters;
use core::marker::PhantomData;
use openprot_hal_blocking::cipher::{
    BlockCipherMode, CipherInit, CipherMode, CipherOp, ErrorType, SymmetricCipher,
};

/// The AES block size, in bytes. ECB/CBC inputs must be a multiple of this
/// (delta A4).
pub const AES_BLOCK: usize = 16;

/// Borrow-arbitrated AES operation over the HACE crypto sub-engine.
///
/// Built only via [`AesCipher::from_device`]; the retained `&'a mut` borrow of
/// the device's `yield_fn` pins `&'a mut HaceDevice` for the op's life, so no
/// second HACE operation (AES, digest, or HMAC) can run concurrently.
pub struct AesCipher<'a> {
    regs: HaceRegisters,
    ctx: &'a mut CryptoContext,
    poll_budget: u32,
    /// Cooperative yield hook, borrowed from the originating `HaceDevice` and
    /// invoked once between every completion poll. Type-erased so the adapter
    /// need not be generic over the strategy (same shape as `HaceDigest`).
    yield_fn: &'a mut dyn FnMut(u32),
}

impl<'a> AesCipher<'a> {
    pub(crate) fn new(
        regs: HaceRegisters,
        ctx: &'a mut CryptoContext,
        poll_budget: u32,
        yield_fn: &'a mut dyn FnMut(u32),
    ) -> Self {
        Self {
            regs,
            ctx,
            poll_budget,
            yield_fn,
        }
    }

    /// Construct an AES adapter from a [`HaceDevice`](super::device::HaceDevice).
    ///
    /// # Safety
    /// Caller must ensure no concurrent or reentrant HACE access for the
    /// lifetime of the returned [`AesCipher`] (the same gate-delegated
    /// single-instance contract as `HaceDigest::from_device`).
    pub unsafe fn from_device<Y: FnMut(u32)>(
        device: &'a mut super::device::HaceDevice<Y>,
    ) -> Self {
        // Borrow split. `regs`/`poll_budget`/`crypto_ctx` are `Copy`d out; the
        // retained `&'a mut device.yield_fn` reborrow pins `&'a mut HaceDevice`
        // for the whole life of the returned op — the arbiter (delta A1).
        let regs = device.regs;
        let poll_budget = device.poll_budget;
        // SAFETY: the device holds the sole pointer to this `.ram_nc` crypto
        // context (acquired once at its `unsafe fn new*` single-instance gate);
        // the caller upholds non-reentrancy and the live `&'a mut device`
        // (pinned by `yield_fn` below) gates it, so no other `&mut` is live.
        let ctx: &'a mut CryptoContext = unsafe { &mut *device.crypto_ctx };
        let yield_fn: &'a mut dyn FnMut(u32) = &mut device.yield_fn;
        Self::new(regs, ctx, poll_budget, yield_fn)
    }

    /// AES key-length → command bits. AES-192 (24 B) is out of scope by
    /// decision (goal.md §2.3 delta A5) and is rejected here.
    fn keylen_bits(key: &[u8]) -> Result<u32, HaceError> {
        match key.len() {
            16 => Ok(HACE_CMD_AES128),
            32 => Ok(HACE_CMD_AES256),
            _ => Err(HaceError::InvalidInput),
        }
    }

    /// One-shot AES transform. `mode_bits` ∈ {`HACE_CMD_ECB`,`HACE_CMD_CBC`};
    /// `iv` is `Some` only for CBC. `input`/`output` are plain data buffers —
    /// **no in-band IV** (delta A2). On return (success *or* failure) the
    /// key/IV region of the context is zeroized (delta A3).
    fn crypt(
        &mut self,
        mode_bits: u32,
        encrypt: bool,
        key: &[u8],
        iv: Option<&[u8; AES_BLOCK]>,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), HaceError> {
        // Delta A4: the block-multiple / sizing bound the authority's
        // `aspeed_aes_crypt` omits — reject before programming the engine.
        if input.is_empty()
            || input.len() % AES_BLOCK != 0
            || output.len() < input.len()
        {
            return Err(HaceError::InvalidInput);
        }
        let kbits = Self::keylen_bits(key)?;
        let len = u32::try_from(input.len()).map_err(|_| HaceError::InvalidInput)?;

        // Engine context: IV at ctx[0..16) (CBC only), raw key at
        // ctx[16..16+keylen) — `hace_aspeed.c:114,186,200` (goal.md §1.9.3).
        self.ctx.ctx = [0u8; 64];
        if let Some(iv) = iv {
            self.ctx.ctx[..AES_BLOCK].copy_from_slice(iv);
        }
        self.ctx.ctx[AES_BLOCK..AES_BLOCK + key.len()].copy_from_slice(key);

        // SG descriptors: addr = data phys, len = bytes | HACE_SG_LAST
        // (`hace_aspeed.c:132-133`; single/last entry).
        let in_ptr = ptr_to_u32(input.as_ptr())?;
        let out_ptr = ptr_to_u32(output.as_ptr())?;
        self.ctx.src.addr = in_ptr;
        self.ctx.src.len = len | HACE_SG_LAST;
        self.ctx.dst.addr = out_ptr;
        self.ctx.dst.len = len | HACE_SG_LAST;

        let cmd = AES_CMD_BASE
            | kbits
            | mode_bits
            | if encrypt { HACE_CMD_ENCRYPT } else { 0 };
        self.ctx.cmd = cmd;

        // The engine takes the *addresses of the SG descriptors* and the ctx
        // buffer, and `crypto_data_len = src.len` (with the terminator bit) —
        // `hace_aspeed.c:90-94` (goal.md §1.9.1).
        let src_desc = ptr_to_u32(core::ptr::addr_of!(self.ctx.src))?;
        let dst_desc = ptr_to_u32(core::ptr::addr_of!(self.ctx.dst))?;
        let ctx_base = ptr_to_u32(self.ctx.ctx.as_ptr())?;
        let data_len = self.ctx.src.len;

        self.regs.clear_crypto_intflag();
        self.regs
            .program_crypto_operation(src_desc, dst_desc, ctx_base, data_len, cmd);

        let mut done = false;
        for _ in 0..self.poll_budget {
            if self.regs.crypto_intflag_is_set() {
                done = true;
                break;
            }
            (self.yield_fn)(POLL_YIELD_NS);
        }

        // Delta A3: never leave key/IV resident in the `.ram_nc` DMA buffer,
        // on success or on timeout.
        self.ctx.ctx = [0u8; 64];

        if done {
            Ok(())
        } else {
            Err(HaceError::Timeout)
        }
    }

    /// AES-ECB encrypt. `key` is 16 or 32 bytes; `pt.len()` must be a non-zero
    /// multiple of 16; `ct.len() >= pt.len()`. `ct` receives exactly the
    /// ciphertext (delta A2).
    pub fn ecb_encrypt(&mut self, key: &[u8], pt: &[u8], ct: &mut [u8]) -> Result<(), HaceError> {
        self.crypt(HACE_CMD_ECB, true, key, None, pt, ct)
    }

    /// AES-ECB decrypt (inverse of [`ecb_encrypt`](Self::ecb_encrypt)).
    pub fn ecb_decrypt(&mut self, key: &[u8], ct: &[u8], pt: &mut [u8]) -> Result<(), HaceError> {
        self.crypt(HACE_CMD_ECB, false, key, None, ct, pt)
    }

    /// AES-CBC encrypt with the given 16-byte `iv`. `ct` receives exactly the
    /// ciphertext — the IV is **not** prepended (delta A2).
    pub fn cbc_encrypt(
        &mut self,
        key: &[u8],
        iv: &[u8; AES_BLOCK],
        pt: &[u8],
        ct: &mut [u8],
    ) -> Result<(), HaceError> {
        self.crypt(HACE_CMD_CBC, true, key, Some(iv), pt, ct)
    }

    /// AES-CBC decrypt with the given 16-byte `iv`. `ct` is the bare
    /// ciphertext (no leading IV — delta A2).
    pub fn cbc_decrypt(
        &mut self,
        key: &[u8],
        iv: &[u8; AES_BLOCK],
        ct: &[u8],
        pt: &mut [u8],
    ) -> Result<(), HaceError> {
        self.crypt(HACE_CMD_CBC, false, key, Some(iv), ct, pt)
    }

    /// Select an OTP/secret-vault sideload key and return the OTP-augmented
    /// command word — the verbatim port of the `aspeed_aes_crypt`
    /// non-`CAP_RAW_KEY` branch (`zephyr-reference/hace_aspeed.c:113-128`;
    /// delta A6, goal.md §2.3 / §2.6).
    ///
    /// Mirrors exactly:
    /// ```c
    /// } else { /*use secret vault key*/
    ///     uint8_t key_id = *((uint8_t *)ctx->key.handle);
    ///     if (key_id == 1)      SELECT_VAL_KEY_1(config->sbase);
    ///     else if (key_id == 2) SELECT_VAL_KEY_2(config->sbase);
    ///     else                  return -EINVAL;
    ///     data->cmd |= HACE_CMD_AES_KEY_FROM_OTP;
    /// }
    /// ```
    ///
    /// No raw key bytes are touched (the key is resident in OTP). `base_cmd`
    /// is the session command word (`AES_CMD_BASE | keylen | mode | dir`); the
    /// returned cmd has `HACE_CMD_AES_KEY_FROM_OTP` OR'd in. An invalid handle
    /// returns [`HaceError::InvalidInput`] (the C `-EINVAL`) **before** any
    /// register write.
    ///
    /// In scope (goal.md §2.6): the handle decode, the `sbase + 0xc` select
    /// RMW, and the cmd modification — all compile-time bit-exact vs. the
    /// frozen source (see `constants.rs`). **Out of scope here:** issuing the
    /// engine pass with a vault key (the crypto end-to-end) — it needs real
    /// silicon and an out-of-band-provisioned OTP slot, separated and
    /// hardware-gated (goal.md §2.6). `sbase` (the crypto-secure base, exactly
    /// the C `config->sbase`) is therefore the documented hardware seam.
    ///
    /// # Safety
    /// `sbase` must be the valid, exclusively-accessed crypto-secure base such
    /// that `sbase + 0xc` is the vault-select register (the same
    /// single-instance contract as the rest of this op).
    pub unsafe fn select_vault_key(
        &mut self,
        sbase: usize,
        key_id: u8,
        base_cmd: u32,
    ) -> Result<u32, HaceError> {
        // Decode first; on an invalid handle return the typed error with no
        // register write (verbatim: the C returns -EINVAL before SELECT_*).
        let slot = super::constants::decode_vault_key_id(key_id)
            .ok_or(HaceError::InvalidInput)?;
        // SAFETY: caller upholds the `sbase` MMIO contract above.
        unsafe { self.regs.select_vault_key(sbase, slot) };
        Ok(super::constants::aes_key_from_otp(base_cmd))
    }
}

impl Drop for AesCipher<'_> {
    fn drop(&mut self) {
        // Delta A3 (defensive): no key/IV residue past the op's scope.
        self.ctx.ctx = [0u8; 64];
    }
}

// ===== Optional openprot cipher-trait skin (ADR-A1) =====================
//
// A *thin, optional, fixed-`N`-buffer* wrapper over the raw core above — the
// `hal_impl`-style skin ADR-A1 (goal.md §3 item 6) keeps separate from the
// driver core, because the openprot `SymmetricCipher` trait has single fixed
// `PlainText`/`CipherText` associated types (it cannot express the ≥ 4 KB
// streaming-DMA path; use the raw `AesCipher` methods for that). `N` must be a
// non-zero multiple of `AES_BLOCK`; a bad length surfaces as the raw core's
// typed `HaceError::InvalidInput` (delta A4). NIST-unverified until run on
// real silicon (goal.md §2.5) — same caveat as the core.

/// AES-ECB mode marker (port-defined; the hal declares no concrete modes).
#[derive(Debug, Clone, Copy)]
pub struct Ecb;
/// AES-CBC mode marker.
#[derive(Debug, Clone, Copy)]
pub struct Cbc;

impl CipherMode for Ecb {}
impl BlockCipherMode for Ecb {}
impl CipherMode for Cbc {}
impl BlockCipherMode for Cbc {}

/// Owned AES key for the trait skin (raw-key path only — the OTP/secret-vault
/// sideload-key path is delta A6: its select *logic* is ported on the raw
/// driver core, but this trait skin carries raw key bytes only; vault crypto
/// E2E is hardware-gated, goal.md §2.3/§2.6). The size *is* the AES variant
/// (16 → AES-128, 32 → AES-256).
#[derive(Clone)]
pub enum AesKey {
    Aes128([u8; 16]),
    Aes256([u8; 32]),
}

impl AesKey {
    #[inline]
    fn as_slice(&self) -> &[u8] {
        match self {
            AesKey::Aes128(k) => k,
            AesKey::Aes256(k) => k,
        }
    }
}

/// Fixed-`N`-byte-buffer openprot cipher-trait skin bound to the one owned
/// [`HaceDevice`]. Every operation context it hands out is a borrow-arbitrated
/// [`AesCipher`] (delta A1 still holds — overlap is a borrow-check error).
pub struct AesSkin<'d, Y: FnMut(u32), const N: usize> {
    dev: &'d mut HaceDevice<Y>,
}

impl<'d, Y: FnMut(u32), const N: usize> AesSkin<'d, Y, N> {
    /// Bind the cipher skin to the device.
    ///
    /// # Safety
    /// Same gate-delegated non-reentrancy contract as
    /// [`AesCipher::from_device`]: no concurrent/reentrant HACE access for the
    /// lifetime of this skin or any context it produces.
    pub unsafe fn new(dev: &'d mut HaceDevice<Y>) -> Self {
        Self { dev }
    }
}

/// In-flight trait-skin operation: the borrow-arbitrated core plus the
/// session key/IV, parameterized by the mode marker `M`.
pub struct AesOp<'a, const N: usize, M> {
    core: AesCipher<'a>,
    key: AesKey,
    iv: [u8; AES_BLOCK],
    _m: PhantomData<M>,
}

impl<'d, Y: FnMut(u32), const N: usize> ErrorType for AesSkin<'d, Y, N> {
    type Error = HaceError;
}

impl<'d, Y: FnMut(u32), const N: usize> SymmetricCipher for AesSkin<'d, Y, N> {
    type Key = AesKey;
    type Nonce = [u8; AES_BLOCK];
    type PlainText = [u8; N];
    type CipherText = [u8; N];
}

impl<'a, const N: usize, M> ErrorType for AesOp<'a, N, M> {
    type Error = HaceError;
}

impl<'a, const N: usize, M> SymmetricCipher for AesOp<'a, N, M> {
    type Key = AesKey;
    type Nonce = [u8; AES_BLOCK];
    type PlainText = [u8; N];
    type CipherText = [u8; N];
}

macro_rules! cipher_init {
    ($mode:ty) => {
        impl<'d, Y: FnMut(u32), const N: usize> CipherInit<$mode> for AesSkin<'d, Y, N> {
            type CipherContext<'a>
                = AesOp<'a, N, $mode>
            where
                Self: 'a;

            fn init<'a>(
                &'a mut self,
                key: &Self::Key,
                nonce: &Self::Nonce,
                _mode: $mode,
            ) -> Result<Self::CipherContext<'a>, Self::Error> {
                // SAFETY: `AesSkin::new`'s non-reentrancy contract covers every
                // context derived from it; the reborrow of `*self.dev` is the
                // exclusive `&mut` borrow-split (delta A1).
                let core = unsafe { AesCipher::from_device(&mut *self.dev) };
                Ok(AesOp {
                    core,
                    key: key.clone(),
                    iv: *nonce,
                    _m: PhantomData,
                })
            }
        }
    };
}
cipher_init!(Ecb);
cipher_init!(Cbc);

impl<'a, const N: usize> CipherOp<Ecb> for AesOp<'a, N, Ecb> {
    fn encrypt(&mut self, plaintext: [u8; N]) -> Result<[u8; N], HaceError> {
        let mut ct = [0u8; N];
        self.core
            .ecb_encrypt(self.key.as_slice(), &plaintext, &mut ct)?;
        Ok(ct)
    }

    fn decrypt(&mut self, ciphertext: [u8; N]) -> Result<[u8; N], HaceError> {
        let mut pt = [0u8; N];
        self.core
            .ecb_decrypt(self.key.as_slice(), &ciphertext, &mut pt)?;
        Ok(pt)
    }
}

impl<'a, const N: usize> CipherOp<Cbc> for AesOp<'a, N, Cbc> {
    fn encrypt(&mut self, plaintext: [u8; N]) -> Result<[u8; N], HaceError> {
        let mut ct = [0u8; N];
        self.core
            .cbc_encrypt(self.key.as_slice(), &self.iv, &plaintext, &mut ct)?;
        Ok(ct)
    }

    fn decrypt(&mut self, ciphertext: [u8; N]) -> Result<[u8; N], HaceError> {
        let mut pt = [0u8; N];
        self.core
            .cbc_decrypt(self.key.as_slice(), &self.iv, &ciphertext, &mut pt)?;
        Ok(pt)
    }
}

#[cfg(test)]
mod vault_select_tests {
    //! Delta A6 select-logic acceptance (goal.md §3 item 7 / §2.6).
    //!
    //! The bit-exact parity vs. the frozen source is *already* enforced at
    //! compile time by the `const _: () = { … }` block in `constants.rs`
    //! (checked on every build, firmware included). These cases restate it in
    //! named, readable form and additionally cover the handle → typed-error
    //! mapping (`HaceError`, which the const block cannot compare).

    use super::super::constants::{
        aes_key_from_otp, decode_vault_key_id, vault_select_rmw, VaultKeySlot,
        HACE_CMD_AES_KEY_FROM_OTP,
    };
    use super::super::error::HaceError;

    /// Verbatim `if (key_id==1) … else if (==2) … else -EINVAL`
    /// (`hace_aspeed.c:116-126`), including the call-site error mapping.
    fn decode_or_einval(key_id: u8) -> Result<VaultKeySlot, HaceError> {
        decode_vault_key_id(key_id).ok_or(HaceError::InvalidInput)
    }

    #[test]
    fn handle_decode_matches_authority_ladder() {
        assert_eq!(decode_or_einval(1), Ok(VaultKeySlot::Slot1));
        assert_eq!(decode_or_einval(2), Ok(VaultKeySlot::Slot2));
        for bad in [0u8, 3, 4, 16, 0x80, 0xFF] {
            assert_eq!(decode_or_einval(bad), Err(HaceError::InvalidInput));
        }
    }

    #[test]
    fn select_val_key_1_clears_bit0_preserving_rest() {
        // hace_aspeed.h:194 — reg &= ~BIT(0)
        for cur in [0xFFFF_FFFFu32, 0x0000_0001, 0xA5A5_A5A5, 0x1234_5678] {
            assert_eq!(vault_select_rmw(VaultKeySlot::Slot1, cur), cur & !1);
        }
    }

    #[test]
    fn select_val_key_2_keeps_only_bit0() {
        // hace_aspeed.h:198 — reg &= BIT(0) (asymmetric vs. KEY_1, verbatim).
        for cur in [0xFFFF_FFFFu32, 0x0000_0001, 0xA5A5_A5A4, 0x1234_5679] {
            assert_eq!(vault_select_rmw(VaultKeySlot::Slot2, cur), cur & 1);
        }
    }

    #[test]
    fn otp_cmd_sets_exactly_bit24() {
        assert_eq!(aes_key_from_otp(0), HACE_CMD_AES_KEY_FROM_OTP);
        assert_eq!(HACE_CMD_AES_KEY_FROM_OTP, 1 << 24);
        let base = 0x1234_5678u32;
        assert_eq!(aes_key_from_otp(base) ^ base, HACE_CMD_AES_KEY_FROM_OTP);
    }
}
