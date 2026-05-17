// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! ECDSA error definitions.

use openprot_hal_blocking::ecdsa::{Error as HalEcdsaError, ErrorKind};

/// Errors surfaced by the ECDSA device layer.
///
/// Only the wait-policy failure (`Timeout`) is defined here: it is the typed,
/// bounded failure of the cooperative-yield poll seam. The verify-result and
/// input-validation variants land with the verify semantics under the
/// `peripheral-parity-port` workflow — hence `#[non_exhaustive]`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SbcError {
    /// Operation did not complete before the poll budget was exhausted
    /// (the D3 bounded-timeout path; goal.md §2.1).
    Timeout,
    /// Engine completed and reported the signature as **invalid**
    /// (`secure014` bit-20 set, bit-21 clear; goal.md §1.2 step 10).
    VerificationFailed,
}

/// Map to the generic HAL kind so the `hal_impl` skin can satisfy
/// `ErrorType` (goal.md §2.3.3: the trait wants shape; this is the mapping).
impl HalEcdsaError for SbcError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Wedged engine / budget exhausted — retryable, like the
            // authority's `-EBUSY` exhaustion (mirrors the HAL doc example).
            SbcError::Timeout => ErrorKind::Busy,
            SbcError::VerificationFailed => ErrorKind::InvalidSignature,
        }
    }
}
