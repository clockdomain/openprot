// Licensed under the Apache-2.0 license

//! Server-side backend trait.
//!
//! [`I2cBackend`] is the contract every server-side I2C backend
//! implements. Lives in the api crate (not the server crate) so
//! host-side mock backends and dispatcher unit tests can depend on it
//! without pulling in the embedded `userspace` syscall surface.

use crate::{ResponseCode, SlaveEventKind};

/// Backend contract: one impl per platform plus the host-side mock.
///
/// All bus identifiers are wire-level `u8`; backends decide how to
/// validate them. See `drivers/i2c/MIGRATION_PLAN.md` §4 / §5.
pub trait I2cBackend {
    // -- controller mode ---------------------------------------------------

    /// Write `data` to `addr` on `bus`.
    fn write(&mut self, bus: u8, addr: u8, data: &[u8]) -> Result<(), ResponseCode>;

    /// Read into `buf` from `addr` on `bus`.
    fn read(&mut self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<(), ResponseCode>;

    /// Combined transaction: write `write_data`, repeated START, then
    /// read into `read_buf`.
    fn write_read(
        &mut self,
        bus: u8,
        addr: u8,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> Result<(), ResponseCode>;

    /// Probe for a device at `addr` on `bus`. `Ok(())` indicates ACK.
    fn probe(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode>;

    /// Reconfigure `bus`'s clock to `speed_hz`.
    fn configure_speed(&mut self, bus: u8, speed_hz: u32) -> Result<(), ResponseCode>;

    /// Attempt bus recovery (clock pulses + STOP) on `bus`.
    fn recover_bus(&mut self, bus: u8) -> Result<(), ResponseCode>;

    // -- slave (target) mode ----------------------------------------------

    /// Configure `bus` as a slave responding to `addr`.
    fn configure_slave(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode>;

    /// Enable slave receive on `bus`.
    fn enable_slave(&mut self, bus: u8) -> Result<(), ResponseCode>;

    /// Disable slave receive on `bus`.
    fn disable_slave(&mut self, bus: u8) -> Result<(), ResponseCode>;

    /// Pull buffered slave RX off `bus` into `buf`. Returns the byte
    /// count copied (may be 0 if nothing has arrived).
    fn slave_receive(&mut self, bus: u8, buf: &mut [u8]) -> Result<usize, ResponseCode>;

    /// Block until the next slave event on `bus`. On a data-receive
    /// event, copy received bytes into `rx_buf` and report the count.
    fn slave_wait_event(
        &mut self,
        bus: u8,
        rx_buf: &mut [u8],
    ) -> Result<(SlaveEventKind, usize), ResponseCode>;

    /// Pre-load `data` as the slave TX response for the next master
    /// read on `bus`.
    fn slave_set_response(&mut self, bus: u8, data: &[u8]) -> Result<(), ResponseCode>;

    /// Arm interrupt-driven slave-receive notifications on `bus`.
    fn enable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode>;

    /// Disarm interrupt-driven slave-receive notifications on `bus`.
    fn disable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode>;

    /// Drain hardware RX into an internal buffer for `bus`. Called by
    /// the runtime's IRQ branch to move bytes off the wire before the
    /// client wakes.
    fn drain_slave_rx(&mut self, bus: u8) -> Result<usize, ResponseCode>;
}
