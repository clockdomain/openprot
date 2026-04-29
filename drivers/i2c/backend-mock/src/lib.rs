// Licensed under the Apache-2.0 license

#![no_std]

//! Loopback / inspectable [`I2cBackend`] for dispatcher tests.
//!
//! Records the last controller-mode call, replays a programmable RX
//! sequence on `read`/`write_read`, and returns deterministic
//! responses for slave-mode ops. Lives in `drivers/i2c/` (not under
//! `target/<plat>/`) because it has no platform dependency — host
//! tests, dispatcher unit tests, and any other consumer that wants
//! a behavior-free I2C peer can link it.
//!
//! `crate_name = "i2c_backend"` so a host-side server binary can
//! `use i2c_backend::Backend;` exactly the way the AST10x0 binary
//! does — drop-in substitution.

use i2c_api::backend::I2cBackend;
use i2c_api::{ResponseCode, SlaveEventKind};

/// Stable type alias the consumer imports as `i2c_backend::Backend`.
pub type Backend = MockI2cBackend;

const RX_PROGRAM_LEN: usize = 64;
const SLAVE_TX_LEN: usize = 32;

/// Last controller-mode call captured for test assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastCall {
    None,
    Write { bus: u8, addr: u8, len: usize },
    Read { bus: u8, addr: u8, len: usize },
    WriteRead { bus: u8, addr: u8, write_len: usize, read_len: usize },
    Probe { bus: u8, addr: u8 },
    ConfigureSpeed { bus: u8, hz: u32 },
    Recover { bus: u8 },
}

pub struct MockI2cBackend {
    /// Most recent controller call — inspect from a test.
    pub last_call: LastCall,
    /// Bytes that the next `read`/`write_read` will hand back.
    pub rx_program: [u8; RX_PROGRAM_LEN],
    pub rx_program_len: usize,
    /// Last `slave_set_response` payload — replayed by `slave_receive`.
    pub slave_tx: [u8; SLAVE_TX_LEN],
    pub slave_tx_len: usize,
    /// Slave notification flag (mirrors the AST10x0 backend's field).
    pub notification_enabled: bool,
}

impl MockI2cBackend {
    pub const fn new() -> Self {
        Self {
            last_call: LastCall::None,
            rx_program: [0u8; RX_PROGRAM_LEN],
            rx_program_len: 0,
            slave_tx: [0u8; SLAVE_TX_LEN],
            slave_tx_len: 0,
            notification_enabled: false,
        }
    }

    /// Test helper: load bytes that the next read returns.
    pub fn program_rx(&mut self, data: &[u8]) {
        let n = data.len().min(RX_PROGRAM_LEN);
        self.rx_program[..n].copy_from_slice(&data[..n]);
        self.rx_program_len = n;
    }
}

impl Default for MockI2cBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cBackend for MockI2cBackend {
    fn write(&mut self, bus: u8, addr: u8, data: &[u8]) -> Result<(), ResponseCode> {
        self.last_call = LastCall::Write {
            bus,
            addr,
            len: data.len(),
        };
        Ok(())
    }

    fn read(&mut self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<(), ResponseCode> {
        self.last_call = LastCall::Read {
            bus,
            addr,
            len: buf.len(),
        };
        let n = buf.len().min(self.rx_program_len);
        buf[..n].copy_from_slice(&self.rx_program[..n]);
        for slot in &mut buf[n..] {
            *slot = 0;
        }
        Ok(())
    }

    fn write_read(
        &mut self,
        bus: u8,
        addr: u8,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> Result<(), ResponseCode> {
        self.last_call = LastCall::WriteRead {
            bus,
            addr,
            write_len: write_data.len(),
            read_len: read_buf.len(),
        };
        let n = read_buf.len().min(self.rx_program_len);
        read_buf[..n].copy_from_slice(&self.rx_program[..n]);
        for slot in &mut read_buf[n..] {
            *slot = 0;
        }
        Ok(())
    }

    fn probe(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode> {
        self.last_call = LastCall::Probe { bus, addr };
        Ok(())
    }

    fn configure_speed(&mut self, bus: u8, speed_hz: u32) -> Result<(), ResponseCode> {
        self.last_call = LastCall::ConfigureSpeed {
            bus,
            hz: speed_hz,
        };
        Ok(())
    }

    fn recover_bus(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.last_call = LastCall::Recover { bus };
        Ok(())
    }

    fn configure_slave(&mut self, _bus: u8, _addr: u8) -> Result<(), ResponseCode> {
        Ok(())
    }

    fn enable_slave(&mut self, _bus: u8) -> Result<(), ResponseCode> {
        Ok(())
    }

    fn disable_slave(&mut self, _bus: u8) -> Result<(), ResponseCode> {
        Ok(())
    }

    fn slave_receive(&mut self, _bus: u8, buf: &mut [u8]) -> Result<usize, ResponseCode> {
        let n = buf.len().min(self.slave_tx_len);
        buf[..n].copy_from_slice(&self.slave_tx[..n]);
        Ok(n)
    }

    fn slave_wait_event(
        &mut self,
        _bus: u8,
        rx_buf: &mut [u8],
    ) -> Result<(SlaveEventKind, usize), ResponseCode> {
        let n = rx_buf.len().min(self.slave_tx_len);
        rx_buf[..n].copy_from_slice(&self.slave_tx[..n]);
        Ok((SlaveEventKind::DataReceived, n))
    }

    fn slave_set_response(&mut self, _bus: u8, data: &[u8]) -> Result<(), ResponseCode> {
        let n = data.len().min(SLAVE_TX_LEN);
        self.slave_tx[..n].copy_from_slice(&data[..n]);
        self.slave_tx_len = n;
        Ok(())
    }

    fn enable_slave_notification(&mut self, _bus: u8) -> Result<(), ResponseCode> {
        self.notification_enabled = true;
        Ok(())
    }

    fn disable_slave_notification(&mut self, _bus: u8) -> Result<(), ResponseCode> {
        self.notification_enabled = false;
        Ok(())
    }

    fn drain_slave_rx(&mut self, _bus: u8) -> Result<usize, ResponseCode> {
        Ok(self.slave_tx_len)
    }
}
