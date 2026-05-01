// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]

pub mod runtime;

use flash_api::backend::FlashBackend;
use flash_api::{FlashError, FlashOp, FlashRequestHeader, FlashResponseHeader};

/// Per-request scratch buffers sized for `MAX_PAYLOAD_SIZE` plus headers
/// and a little slack.  Concrete platform binaries pick their own sizes
/// when they call `runtime::run`; these are the defaults exposed for
/// convenience.
pub const MAX_REQUEST_SIZE: usize = 512;
pub const MAX_RESPONSE_SIZE: usize = 512;

/// Outcome of a single dispatch call. Flash v1 is fully synchronous so
/// the only outcome is `Respond`; the variant is kept to mirror the usart
/// runtime and leave room for a future parked-IRQ path.
pub enum DispatchOutcome {
    Respond(usize),
}

pub fn dispatch_request<B: FlashBackend>(
    backend: &mut B,
    request: &[u8],
    response: &mut [u8],
) -> DispatchOutcome {
    if request.len() < FlashRequestHeader::SIZE {
        return DispatchOutcome::Respond(encode_error(response, FlashError::InvalidOperation));
    }

    let hdr_bytes = &request[..FlashRequestHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, FlashRequestHeader>::from_bytes(hdr_bytes).ok() else {
        return DispatchOutcome::Respond(encode_error(response, FlashError::InvalidOperation));
    };

    let op = match hdr.operation() {
        Ok(op) => op,
        Err(e) => return DispatchOutcome::Respond(encode_error(response, e)),
    };

    let payload_len = hdr.payload_length();
    if request.len() < FlashRequestHeader::SIZE + payload_len {
        return DispatchOutcome::Respond(encode_error(response, FlashError::InvalidOperation));
    }
    let payload = &request[FlashRequestHeader::SIZE..FlashRequestHeader::SIZE + payload_len];

    let address = hdr.address_value();
    let length = hdr.length_value();

    match op {
        FlashOp::Exists => DispatchOutcome::Respond(encode_value(response, 0, 0)),

        FlashOp::GetCapacity => {
            let info = backend.info();
            DispatchOutcome::Respond(encode_value(response, info.capacity, 0))
        }

        FlashOp::GetChunkSize => {
            let info = backend.info();
            DispatchOutcome::Respond(encode_value(response, info.chunk_size, 0))
        }

        FlashOp::Read => {
            let payload_offset = FlashResponseHeader::SIZE;
            let payload_capacity = response.len().saturating_sub(payload_offset);
            let read_buf_len =
                core::cmp::min(length as usize, payload_capacity).min(u16::MAX as usize);

            match backend.read(
                address,
                &mut response[payload_offset..payload_offset + read_buf_len],
            ) {
                Ok(n) => {
                    let hdr = FlashResponseHeader::success(n as u32, n as u16);
                    response[..FlashResponseHeader::SIZE]
                        .copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
                    DispatchOutcome::Respond(FlashResponseHeader::SIZE + n)
                }
                Err(e) => DispatchOutcome::Respond(encode_error(response, e.into())),
            }
        }

        FlashOp::Write => {
            if (length as usize) != payload_len {
                return DispatchOutcome::Respond(encode_error(
                    response,
                    FlashError::InvalidLength,
                ));
            }
            match backend.write(address, payload) {
                Ok(n) => DispatchOutcome::Respond(encode_value(response, n as u32, 0)),
                Err(e) => DispatchOutcome::Respond(encode_error(response, e.into())),
            }
        }

        FlashOp::Erase => match backend.erase(address, length) {
            Ok(()) => DispatchOutcome::Respond(encode_value(response, length, 0)),
            Err(e) => DispatchOutcome::Respond(encode_error(response, e.into())),
        },
    }
}

fn encode_error(response: &mut [u8], error: FlashError) -> usize {
    let hdr = FlashResponseHeader::error(error);
    response[..FlashResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    FlashResponseHeader::SIZE
}

fn encode_value(response: &mut [u8], value: u32, payload_len: u16) -> usize {
    let hdr = FlashResponseHeader::success(value, payload_len);
    response[..FlashResponseHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
    FlashResponseHeader::SIZE + payload_len as usize
}
