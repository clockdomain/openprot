// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! HAL trait skin: `EcdsaVerify<P384>` over the internal ECDSA driver.
//!
//! ADR-1 (goal.md §5) — *skin, not skeleton*. This is the only place the
//! `openprot_hal_blocking::ecdsa` traits touch the port: pure boundary
//! translation (trait types → raw 48-byte operands), then a call into the
//! trait-agnostic [`SbcOp::verify_raw`]. The façade/device/op layers are
//! architected against the behavioral authority (§1), not this trait;
//! deleting this file leaves the driver fully compiling and usable.

use openprot_hal_blocking::digest::DigestAlgorithm;
use openprot_hal_blocking::ecdsa::{
    Curve, EcdsaVerify, ErrorType, P384, P384PublicKey, P384Signature, PublicKey, Signature,
};

use super::error::SbcError;
use super::op::SbcOp;

impl ErrorType for SbcOp<'_> {
    type Error = SbcError;
}

impl EcdsaVerify<P384> for SbcOp<'_> {
    type PublicKey = P384PublicKey;
    type Signature = P384Signature;

    fn verify(
        &mut self,
        public_key: &P384PublicKey,
        digest: <<P384 as Curve>::DigestType as DigestAlgorithm>::Digest,
        signature: &P384Signature,
    ) -> Result<(), SbcError> {
        let (qx, qy) = public_key.coordinates();
        let (r, s) = signature.coordinates();
        // `Sha2_384::Digest` is `Digest<12>` — invariantly 48 bytes
        // (12 × u32, `#[repr(C)]`); native-endian, matching the façade's
        // `from_ne_bytes` operand load (goal.md §1.1).
        let mut m = [0u8; 48];
        m.copy_from_slice(digest.as_bytes());
        self.verify_raw(&qx, &qy, &r, &s, &m)
    }
}
