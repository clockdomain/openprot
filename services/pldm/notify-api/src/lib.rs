// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]

pub mod protocol;
pub mod transport;

#[doc(inline)]
pub use protocol::{
    Decision, NotifyError, NotifyOp, NotifyRequestHeader, NotifyResponseHeader, Pending, Phase,
    MAX_BUF_SIZE, MAX_PAYLOAD_SIZE, MAX_SERVICE_INTERVAL_MILLIS, MIN_TRANSACT_TIMEOUT_MILLIS,
};
pub use transport::{Transport, TransportError};
