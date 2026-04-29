// Licensed under the Apache-2.0 license

#![no_std]

//! I2C server — pure dispatch + reusable runtime.
//!
//! - [`I2cBackend`] is the trait every backend implements (one per platform
//!   plus the host-side mock — see the migration plan §4).
//! - [`dispatch_request`] is a pure function: parses an
//!   [`i2c_api::wire::I2cRequestHeader`], routes to the right backend
//!   method, encodes the response. No IPC, no syscalls — host-testable
//!   against the mock backend.
//! - [`runtime::run`] is the IPC dispatch loop the binary glue calls.

pub mod runtime;

use i2c_api::wire::{I2cOp, I2cRequestHeader, I2cResponseHeader};
use i2c_api::ResponseCode;

// Re-export so binaries can `use i2c_server::I2cBackend` if convenient.
pub use i2c_api::backend::I2cBackend;

/// Decode request header, dispatch to backend, encode response.
///
/// Read operations write their data directly into `response` after the
/// response header (offset [`I2cResponseHeader::SIZE`]), avoiding an
/// extra copy.
pub fn dispatch_request<B: I2cBackend>(
    backend: &mut B,
    request: &[u8],
    response: &mut [u8],
) -> usize {
    let Some(header) = I2cRequestHeader::from_bytes(request) else {
        return encode_error(response, ResponseCode::ServerError);
    };

    let Some(op) = header.operation() else {
        return encode_error(response, ResponseCode::ServerError);
    };

    let payload = &request[I2cRequestHeader::SIZE..];

    match op {
        I2cOp::Write => {
            let wlen = header.write_len as usize;
            if payload.len() < wlen {
                return encode_error(response, ResponseCode::BufferTooSmall);
            }
            match backend.write(header.bus, header.address, &payload[..wlen]) {
                Ok(()) => encode_success(response, 0),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::Read => {
            let rlen = header.read_len as usize;
            let avail = response.len().saturating_sub(I2cResponseHeader::SIZE);
            if rlen > avail {
                return encode_error(response, ResponseCode::BufferTooLarge);
            }
            let read_buf =
                &mut response[I2cResponseHeader::SIZE..I2cResponseHeader::SIZE + rlen];
            match backend.read(header.bus, header.address, read_buf) {
                Ok(()) => encode_success(response, rlen),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::WriteRead => {
            let wlen = header.write_len as usize;
            let rlen = header.read_len as usize;
            if payload.len() < wlen {
                return encode_error(response, ResponseCode::BufferTooSmall);
            }
            let avail = response.len().saturating_sub(I2cResponseHeader::SIZE);
            if rlen > avail {
                return encode_error(response, ResponseCode::BufferTooLarge);
            }
            let write_data = &payload[..wlen];
            let read_buf =
                &mut response[I2cResponseHeader::SIZE..I2cResponseHeader::SIZE + rlen];
            match backend.write_read(header.bus, header.address, write_data, read_buf) {
                Ok(()) => encode_success(response, rlen),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::Probe => match backend.probe(header.bus, header.address) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::ConfigureSpeed => {
            // Speed is encoded as `(write_len << 16) | read_len` per the
            // existing wire convention (no dedicated field).
            let speed_hz = ((header.write_len as u32) << 16) | header.read_len as u32;
            match backend.configure_speed(header.bus, speed_hz) {
                Ok(()) => encode_success(response, 0),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::RecoverBus => match backend.recover_bus(header.bus) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::Transaction => encode_error(response, ResponseCode::ServerError),

        I2cOp::ConfigureSlave => match backend.configure_slave(header.bus, header.address) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::EnableSlave => match backend.enable_slave(header.bus) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::DisableSlave => match backend.disable_slave(header.bus) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::SlaveReceive => {
            let rlen = header.read_len as usize;
            let avail = response.len().saturating_sub(I2cResponseHeader::SIZE);
            if rlen > avail {
                return encode_error(response, ResponseCode::BufferTooLarge);
            }
            let buf = &mut response[I2cResponseHeader::SIZE..I2cResponseHeader::SIZE + rlen];
            match backend.slave_receive(header.bus, buf) {
                Ok(n) => encode_success(response, n),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::SlaveWaitEvent => {
            let max_rx = header.read_len as usize;
            // Reserve space for event-kind byte + rx data.
            let avail = response.len().saturating_sub(I2cResponseHeader::SIZE + 1);
            let rx_cap = max_rx.min(avail);
            let rx_buf = &mut response
                [I2cResponseHeader::SIZE + 1..I2cResponseHeader::SIZE + 1 + rx_cap];
            match backend.slave_wait_event(header.bus, rx_buf) {
                Ok((kind, rx_len)) => {
                    let total = 1 + rx_len;
                    response[I2cResponseHeader::SIZE] = kind as u8;
                    encode_success(response, total)
                }
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::SlaveSetResponse => {
            let wlen = header.write_len as usize;
            if payload.len() < wlen {
                return encode_error(response, ResponseCode::BufferTooSmall);
            }
            match backend.slave_set_response(header.bus, &payload[..wlen]) {
                Ok(()) => encode_success(response, 0),
                Err(code) => encode_error(response, code),
            }
        }

        I2cOp::EnableSlaveNotification => match backend.enable_slave_notification(header.bus) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },

        I2cOp::DisableSlaveNotification => match backend.disable_slave_notification(header.bus) {
            Ok(()) => encode_success(response, 0),
            Err(code) => encode_error(response, code),
        },
    }
}

fn encode_error(response: &mut [u8], code: ResponseCode) -> usize {
    let header = I2cResponseHeader::error(code);
    response[..I2cResponseHeader::SIZE].copy_from_slice(&header.to_bytes());
    I2cResponseHeader::SIZE
}

fn encode_success(response: &mut [u8], data_len: usize) -> usize {
    let header = I2cResponseHeader::success(data_len as u16);
    response[..I2cResponseHeader::SIZE].copy_from_slice(&header.to_bytes());
    I2cResponseHeader::SIZE + data_len
}
