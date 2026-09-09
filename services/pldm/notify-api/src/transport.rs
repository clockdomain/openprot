// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! The transport seam. Same shape as `i2c_api::transport`: one whole
//! serialized request goes out, one whole response comes back — bytes in,
//! bytes out, one shot.
//!
//! - `LoopbackTransport` (in `notify-server`) — calls `notify_server::dispatch`
//!   directly against a shared `NotifyState`. Host-buildable; this is what the
//!   Phase 1 integration tests exercise.
//! - `IpcTransport` (in `notify-client-ipc`, Phase 2) — the kernel-tagged
//!   production path over `channel_transact`, with a **bounded** deadline
//!   (never `Instant::MAX` — see the design doc).

/// Why a transport round-trip failed. Transport-neutral; notify-level status
/// travels inside the response payload, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The underlying channel/syscall/loopback call failed.
    Failed,
    /// The round-trip did not complete before its bounded deadline. Surfaced
    /// so the orchestrator runtime can treat a silent PLDM as unhealthy.
    Timeout,
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Failed => f.write_str("notify transport round-trip failed"),
            Self::Timeout => f.write_str("notify transport round-trip timed out"),
        }
    }
}

impl core::error::Error for TransportError {}

/// Bytes-in -> bytes-out, exactly one round-trip.
pub trait Transport {
    fn transact(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, TransportError>;
}
