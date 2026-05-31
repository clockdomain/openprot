// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SBC public-key engine error definitions (shared by the ECDSA verify and
//! RSA modexp operations).

use openprot_hal_blocking::ecdsa::{Error as HalEcdsaError, ErrorKind};

/// Errors surfaced by the SBC engine layer. `#[non_exhaustive]`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SbcError {
    /// Operation did not complete before the poll budget was exhausted —
    /// the bounded-timeout intentional delta (ECDSA D3 / RSA R1; the
    /// authority would hang here instead).
    Timeout,
    /// ECDSA only: engine completed and reported the signature **invalid**
    /// (`secure014` bit-20 set, bit-21 clear).
    VerificationFailed,
    /// Caller-provided operand sizes are out of range — RSA input data
    /// `> 512` bytes (authority `rsa_aspeed.c:54-57` `-EINVAL`), or an
    /// exponent/modulus/output buffer too small for the stated bit lengths.
    InvalidInput,
}

/// Map to the generic HAL kind so the `hal_impl` skin can satisfy `ErrorType`.
impl HalEcdsaError for SbcError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Wedged engine / budget exhausted — retryable, like the
            // authority's `-EBUSY` exhaustion (mirrors the HAL doc example).
            SbcError::Timeout => ErrorKind::Busy,
            SbcError::VerificationFailed => ErrorKind::InvalidSignature,
            SbcError::InvalidInput => ErrorKind::Other,
        }
    }
}
