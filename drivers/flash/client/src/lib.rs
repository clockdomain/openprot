// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]

use flash_api::{
    FlashError, FlashOp, FlashRequestHeader, FlashResponseHeader, MAX_PAYLOAD_SIZE,
};
use userspace::syscall;
use userspace::time::Instant;

const MAX_BUF_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    IpcError(pw_status::Error),
    ServerError(FlashError),
    InvalidResponse,
    BufferTooSmall,
}

impl From<pw_status::Error> for ClientError {
    fn from(e: pw_status::Error) -> Self {
        Self::IpcError(e)
    }
}

pub struct FlashClient {
    handle: u32,
}

impl FlashClient {
    pub const fn new(handle: u32) -> Self {
        Self { handle }
    }

    /// Probe the server. Returns `Ok(())` if the channel is alive and
    /// the peer answers with `Success`.
    pub fn exists(&self) -> Result<(), ClientError> {
        self.call_value(FlashOp::Exists, 0, 0).map(|_| ())
    }

    /// Total bytes of flash exposed by the backend.
    pub fn capacity(&self) -> Result<u32, ClientError> {
        self.call_value(FlashOp::GetCapacity, 0, 0)
    }

    /// Largest single read or write the backend will accept. Larger
    /// requests must be issued by the caller as a sequence of
    /// chunk-sized operations.
    pub fn chunk_size(&self) -> Result<u32, ClientError> {
        self.call_value(FlashOp::GetChunkSize, 0, 0)
    }

    /// Read up to `out.len()` bytes starting at `address`. The caller
    /// is responsible for ensuring `out.len() <= chunk_size()`.
    pub fn read(&self, address: u32, out: &mut [u8]) -> Result<usize, ClientError> {
        self.read_with_timeout(address, out, Instant::MAX)
    }

    pub fn read_with_timeout(
        &self,
        address: u32,
        out: &mut [u8],
        deadline: Instant,
    ) -> Result<usize, ClientError> {
        if out.len() > MAX_PAYLOAD_SIZE {
            return Err(ClientError::BufferTooSmall);
        }

        let mut req = [0u8; MAX_BUF_SIZE];
        let mut resp = [0u8; MAX_BUF_SIZE];

        let hdr = FlashRequestHeader::new(FlashOp::Read, address, out.len() as u32, 0);
        req[..FlashRequestHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));

        let resp_len = syscall::channel_transact(
            self.handle,
            &req[..FlashRequestHeader::SIZE],
            &mut resp,
            deadline,
        )?;

        parse_payload_response(&resp[..resp_len], out)
    }

    /// Write `data` starting at `address`. The caller is responsible
    /// for ensuring `data.len() <= chunk_size()`.
    pub fn write(&self, address: u32, data: &[u8]) -> Result<usize, ClientError> {
        if data.len() > MAX_PAYLOAD_SIZE {
            return Err(ClientError::BufferTooSmall);
        }

        let mut req = [0u8; MAX_BUF_SIZE];
        let mut resp = [0u8; MAX_BUF_SIZE];

        let hdr = FlashRequestHeader::new(
            FlashOp::Write,
            address,
            data.len() as u32,
            data.len() as u16,
        );
        req[..FlashRequestHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));
        req[FlashRequestHeader::SIZE..FlashRequestHeader::SIZE + data.len()].copy_from_slice(data);

        let resp_len = syscall::channel_transact(
            self.handle,
            &req[..FlashRequestHeader::SIZE + data.len()],
            &mut resp,
            Instant::MAX,
        )?;

        parse_value_response(&resp[..resp_len]).map(|n| n as usize)
    }

    /// Erase `length` bytes starting at `address`. Both must be aligned
    /// to and a multiple of the backend's erase granule.
    pub fn erase(&self, address: u32, length: u32) -> Result<(), ClientError> {
        let mut req = [0u8; MAX_BUF_SIZE];
        let mut resp = [0u8; MAX_BUF_SIZE];

        let hdr = FlashRequestHeader::new(FlashOp::Erase, address, length, 0);
        req[..FlashRequestHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));

        let resp_len = syscall::channel_transact(
            self.handle,
            &req[..FlashRequestHeader::SIZE],
            &mut resp,
            Instant::MAX,
        )?;

        parse_value_response(&resp[..resp_len]).map(|_| ())
    }

    fn call_value(&self, op: FlashOp, address: u32, length: u32) -> Result<u32, ClientError> {
        let mut req = [0u8; MAX_BUF_SIZE];
        let mut resp = [0u8; MAX_BUF_SIZE];

        let hdr = FlashRequestHeader::new(op, address, length, 0);
        req[..FlashRequestHeader::SIZE].copy_from_slice(zerocopy::IntoBytes::as_bytes(&hdr));

        let resp_len = syscall::channel_transact(
            self.handle,
            &req[..FlashRequestHeader::SIZE],
            &mut resp,
            Instant::MAX,
        )?;

        parse_value_response(&resp[..resp_len])
    }
}

fn parse_value_response(resp: &[u8]) -> Result<u32, ClientError> {
    if resp.len() < FlashResponseHeader::SIZE {
        return Err(ClientError::InvalidResponse);
    }

    let hdr_bytes = &resp[..FlashResponseHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, FlashResponseHeader>::from_bytes(hdr_bytes).ok() else {
        return Err(ClientError::InvalidResponse);
    };

    if hdr.is_success() {
        Ok(hdr.value_word())
    } else {
        Err(ClientError::ServerError(hdr.error_code()))
    }
}

fn parse_payload_response(resp: &[u8], out: &mut [u8]) -> Result<usize, ClientError> {
    if resp.len() < FlashResponseHeader::SIZE {
        return Err(ClientError::InvalidResponse);
    }

    let hdr_bytes = &resp[..FlashResponseHeader::SIZE];
    let Some(hdr) = zerocopy::Ref::<_, FlashResponseHeader>::from_bytes(hdr_bytes).ok() else {
        return Err(ClientError::InvalidResponse);
    };

    if !hdr.is_success() {
        return Err(ClientError::ServerError(hdr.error_code()));
    }

    let len = hdr.payload_length();
    if len > out.len() || resp.len() < FlashResponseHeader::SIZE + len {
        return Err(ClientError::InvalidResponse);
    }

    out[..len].copy_from_slice(&resp[FlashResponseHeader::SIZE..FlashResponseHeader::SIZE + len]);
    Ok(len)
}
