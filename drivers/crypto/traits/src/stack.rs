// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! High-level crypto stack facade.
//!
//! Bridges any [`CryptoBackend`] implementation to ergonomic
//! `digest()/mac()/cipher()/verify()` entry points, **hiding which backend or
//! transport is underneath**. Consumers depend on `crypto_traits` *only*; the
//! binary wires `Stack::new(CryptoClient::new(h))` (IPC → server) or
//! `Stack::new(SoftCrypto::new())` (in-process) per the sketch §3.3 / goal
//! §5.3 policy table, and that choice is invisible to the consumer.
//!
//! This is the direct analog of `services/mctp`'s `mctp-api::stack::Stack<C>`:
//! the abstract traits and the generic facade ship in the same crate, and the
//! concrete `IpcMctpClient`/`SoftCrypto` is never named in consumer code
//! (sketch §3.7, ADR-C1).
//!
//! ## Usage
//!
//! ```rust,ignore
//! use crypto_traits::{Algo, KeyRef, Stack};
//!
//! // The binary picks the backend; the consumer below does not care which.
//! let mut stack = Stack::new(SoftCrypto::new());
//!
//! let mut out = [0u8; 32];
//! let n = stack.digest(Algo::Sha256, b"hello", &mut out)?;
//!
//! let verified = stack.verify().ecdsa_p384_verify(pubkey, sig, &digest48)?;
//! ```

use crate::{
    Algo, AesParams, Cipher, CryptoBackend, CryptoError, Digest, EcPubP384, EcSig, KeyRef, Mac,
    RsaKey, Verify,
};

// ============================================================================
// Stack
// ============================================================================

/// A crypto stack facade backed by any [`CryptoBackend`] implementation.
///
/// `Stack` is the entry point for consumer code. It owns a concrete backend
/// (`SoftCrypto`, the IPC `CryptoClient`, or a test mock) and exposes the
/// crypto capabilities through delegating methods. Consumers only depend on
/// this crate; the underlying backend implementation and OS transport are
/// invisible (sketch §3.3, §3.6, §3.7).
pub struct Stack<B: CryptoBackend> {
    backend: B,
}

impl<B: CryptoBackend> Stack<B> {
    /// Create a new stack backed by the given crypto backend.
    ///
    /// `B` is bound by the single supertrait [`CryptoBackend`] (= `Digest +
    /// Mac + Cipher + Verify`); see its docs for why one named bundle is used
    /// instead of four independent generic bounds.
    pub fn new(backend: B) -> Self {
        Stack { backend }
    }

    /// One-shot hash. Delegates to [`Digest::digest`].
    pub fn digest(
        &mut self,
        algo: Algo,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        Digest::digest(&mut self.backend, algo, input, out)
    }

    /// One-shot keyed MAC (HMAC). Delegates to [`Mac::mac`].
    pub fn mac(
        &mut self,
        algo: Algo,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        Mac::mac(&mut self.backend, algo, key, input, out)
    }

    /// One-shot AES (ECB/CBC, 128/256). Delegates to [`Cipher::crypt`].
    pub fn cipher(
        &mut self,
        params: AesParams,
        key: KeyRef<'_>,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CryptoError> {
        Cipher::crypt(&mut self.backend, params, key, input, out)
    }

    /// ECDSA-P384 signature verification. Delegates to
    /// [`Verify::ecdsa_p384_verify`]; `Ok(false)` is a clean non-verify, not
    /// an error (sketch §3.5).
    pub fn verify_ecdsa_p384(
        &mut self,
        pubkey: EcPubP384,
        sig: EcSig,
        digest48: &[u8; 48],
    ) -> Result<bool, CryptoError> {
        Verify::ecdsa_p384_verify(&mut self.backend, pubkey, sig, digest48)
    }

    /// RSA signature verification. Delegates to [`Verify::rsa_verify`];
    /// `Ok(false)` is a clean non-verify, not an error (sketch §3.5).
    pub fn verify_rsa(
        &mut self,
        key: RsaKey<'_>,
        sig: &[u8],
        expected: &[u8],
    ) -> Result<bool, CryptoError> {
        Verify::rsa_verify(&mut self.backend, key, sig, expected)
    }

    /// Borrow the backend as a [`Verify`] for callers that prefer the trait's
    /// own method names, mirroring the `digest()/mac()/cipher()` ergonomics:
    /// `stack.verify().ecdsa_p384_verify(..)`.
    pub fn verify(&mut self) -> &mut impl Verify {
        &mut self.backend
    }

    /// Consume the stack and return the wrapped backend.
    pub fn into_inner(self) -> B {
        self.backend
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal mock backend: digest fills `0xAB`, verify returns a scripted
    /// verdict. Exercises the facade without pulling a real crypto crate.
    #[derive(Default)]
    struct MockBackend {
        verdict: bool,
    }

    impl Digest for MockBackend {
        fn digest(
            &mut self,
            algo: Algo,
            _input: &[u8],
            out: &mut [u8],
        ) -> Result<usize, CryptoError> {
            let n = algo.digest_len();
            if out.len() < n {
                return Err(CryptoError::OutputTooSmall);
            }
            for b in &mut out[..n] {
                *b = 0xAB;
            }
            Ok(n)
        }
    }

    impl Mac for MockBackend {
        fn mac(
            &mut self,
            algo: Algo,
            _key: KeyRef<'_>,
            _input: &[u8],
            out: &mut [u8],
        ) -> Result<usize, CryptoError> {
            let n = algo.digest_len();
            if out.len() < n {
                return Err(CryptoError::OutputTooSmall);
            }
            Ok(n)
        }
    }

    impl Cipher for MockBackend {
        fn crypt(
            &mut self,
            _params: AesParams,
            _key: KeyRef<'_>,
            input: &[u8],
            out: &mut [u8],
        ) -> Result<usize, CryptoError> {
            out[..input.len()].copy_from_slice(input);
            Ok(input.len())
        }
    }

    impl Verify for MockBackend {
        fn ecdsa_p384_verify(
            &mut self,
            _pubkey: EcPubP384,
            _sig: EcSig,
            _digest48: &[u8; 48],
        ) -> Result<bool, CryptoError> {
            Ok(self.verdict)
        }

        fn rsa_verify(
            &mut self,
            _key: RsaKey<'_>,
            _sig: &[u8],
            _expected: &[u8],
        ) -> Result<bool, CryptoError> {
            Ok(self.verdict)
        }
    }

    #[test]
    fn stack_digest_delegates() {
        let mut stack = Stack::new(MockBackend::default());
        let mut out = [0u8; 32];
        let n = stack.digest(Algo::Sha256, b"x", &mut out).unwrap();
        assert_eq!(n, 32);
        assert!(out.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn stack_verify_is_a_result_not_an_error() {
        // Ok(false) must be a clean non-verify, never an Err (sketch §3.5).
        let mut stack = Stack::new(MockBackend { verdict: false });
        let r = stack
            .verify_ecdsa_p384(
                EcPubP384 {
                    qx: [0; 48],
                    qy: [0; 48],
                },
                EcSig {
                    r: [0; 48],
                    s: [0; 48],
                },
                &[0u8; 48],
            )
            .unwrap();
        assert!(!r);
    }

    #[test]
    fn stack_verify_accessor_delegates() {
        let mut stack = Stack::new(MockBackend { verdict: true });
        let ok = stack
            .verify()
            .rsa_verify(
                RsaKey {
                    modulus: &[1, 2, 3],
                    exponent: &[1],
                },
                &[0u8; 4],
                &[0u8; 4],
            )
            .unwrap();
        assert!(ok);
    }
}
