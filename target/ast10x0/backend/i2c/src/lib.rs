// Licensed under the Apache-2.0 license

#![no_std]

//! AST10x0 I2C backend — single-bus per system image.
//!
//! Wraps one [`Ast1060I2c`] instance bound to the bus declared in the
//! system image's `system.json5`. Validates incoming wire-level
//! `bus: u8` against the configured bus and routes everything else to
//! the peripheral driver.
//!
//! # Architecture
//!
//! ```text
//! Platform entry.rs (boot, single-threaded, kernel context):
//!   init_i2c_global()                ← SCU reset + I2CG0C/I2CG10
//!   Pinctrl::apply_pinctrl_group()   ← SCU4xx pin mux (per bus used)
//!
//! Server task (one per system image, single-bus):
//!   Ast1060I2cBackend::new(bus_id, config, yield_ns)
//!     bus_ptrs(bus_id)               ← &PAC I2cN + I2cbuffN regs
//!     Ast1060I2c::new(...)           ← I2CC00 reset, timing, IER
//!
//!   per IPC op (controller mode):
//!     check_bus(bus)                 ← validate bus == bus_id
//!     Ast1060I2c::write/read/...     ← reuses the persistent instance
//! ```
//!
//! No per-op `from_initialized` reconstruction is needed: the backend
//! holds a single live `Ast1060I2c<…>` that handles every request.
//!
//! `yield_ns` is a non-capturing `fn(u32)` supplied by the binary.
//! For IRQ-driven servers it should `object_wait` on the bus's IRQ
//! signal so the task sleeps until the controller fires (see the
//! `wait_for_i2c_irq` example in `target/ast10x0/tests/i2c/server_main.rs`).
//!
//! See `drivers/i2c/MIGRATION_PLAN.md` §4-§5 for the multi-backend
//! and single-bus design rationale.

use ast10x0_peripherals::i2c::{
    Ast1060I2c, I2cConfig, I2cError, SlaveBuffer, SlaveConfig, SlaveEvent,
};
use ast1060_pac::{i2c, i2cbuff};
use i2c_api::backend::I2cBackend;
use i2c_api::{ResponseCode, SlaveEventKind};

/// Stable type alias the server binary imports as `i2c_backend::Backend`.
pub type Backend = Ast1060I2cBackend;

/// Single-bus AST10x0 I2C backend.
pub struct Ast1060I2cBackend {
    /// The wire-level bus index this backend was constructed for. Every
    /// IPC request validates `header.bus == bus_id`.
    bus_id: u8,
    /// Hardware handle for the configured bus.
    i2c: Ast1060I2c<'static, fn(u32)>,
    /// Server-side notification flag toggled by
    /// `enable_slave_notification` / `disable_slave_notification`.
    notification_enabled: bool,
    /// Last drained slave RX, available to subsequent SlaveReceive calls.
    slave_rx: SlaveBuffer,
}

impl Ast1060I2cBackend {
    /// Construct the backend bound to a single I2C bus.
    ///
    /// # Safety
    ///
    /// - The platform's pre-kernel `entry.rs` must have already run
    ///   `init_i2c_global()` and `Pinctrl::apply_pinctrl_group(...)`
    ///   for `bus_id`. SCU lives outside this process's memory
    ///   mappings; touching it from userspace is the platform's job.
    /// - `bus_id` must be the controller this server task exclusively
    ///   owns (declared in the system image's `system.json5`).
    /// - This must be called once per server task.
    pub unsafe fn new(
        bus_id: u8,
        config: I2cConfig,
        yield_ns: fn(u32),
    ) -> Result<Self, ResponseCode> {
        let (regs, buff) = bus_ptrs(bus_id).ok_or(ResponseCode::InvalidBus)?;
        // SAFETY: caller guarantees exclusive ownership of `bus_id`'s
        // peripherals; pointers come straight from the PAC and are
        // valid for 'static. `yield_ns` is supplied by the binary.
        let i2c = unsafe {
            Ast1060I2c::new(regs, buff, config, yield_ns).map_err(map_i2c_error)?
        };
        Ok(Self {
            bus_id,
            i2c,
            notification_enabled: false,
            slave_rx: SlaveBuffer::new(),
        })
    }

    /// The bus this backend serves. Used by the runtime to know which
    /// notification-enabled bus to drain on an IRQ wake-up.
    pub fn bus_id(&self) -> u8 {
        self.bus_id
    }

    /// Drain hardware RX into [`Self::slave_rx`].
    pub fn drain_to_internal_buffer(&mut self) -> Result<usize, ResponseCode> {
        let mut buf = [0u8; 32];
        let n = self.i2c.slave_read(&mut buf).map_err(map_i2c_error)?;
        self.slave_rx.set_len(n);
        self.slave_rx.data_mut()[..n].copy_from_slice(&buf[..n]);
        Ok(n)
    }

    fn check_bus(&self, bus: u8) -> Result<(), ResponseCode> {
        if bus == self.bus_id {
            Ok(())
        } else {
            Err(ResponseCode::InvalidBus)
        }
    }
}

impl I2cBackend for Ast1060I2cBackend {
    // -- controller mode ---------------------------------------------------

    fn write(&mut self, bus: u8, addr: u8, data: &[u8]) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c.write(addr, data).map_err(map_i2c_error)
    }

    fn read(&mut self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c.read(addr, buf).map_err(map_i2c_error)
    }

    fn write_read(
        &mut self,
        bus: u8,
        addr: u8,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c
            .write_read(addr, write_data, read_buf)
            .map_err(map_i2c_error)
    }

    fn probe(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        // The peripheral's `write` short-circuits empty payloads, so
        // probe issues a 1-byte write of 0x00 instead. Devices that
        // can't tolerate a write should be probed via SMBus quick
        // command at the peripheral layer once that's exposed.
        self.i2c.write(addr, &[0u8]).map_err(map_i2c_error)
    }

    fn configure_speed(&mut self, bus: u8, _speed_hz: u32) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        // Peripheral's I2cConfig is consumed at construction; runtime
        // re-configuration would require re-running `init_hardware`.
        // Stub for now; revisit if a consumer needs it.
        Ok(())
    }

    fn recover_bus(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c.recover_bus().map_err(map_i2c_error)
    }

    // -- slave (target) mode ----------------------------------------------

    fn configure_slave(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        let cfg = SlaveConfig::new(addr).map_err(map_i2c_error)?;
        self.i2c.configure_slave(&cfg).map_err(map_i2c_error)
    }

    fn enable_slave(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c.enable_slave();
        Ok(())
    }

    fn disable_slave(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.i2c.disable_slave();
        Ok(())
    }

    fn slave_receive(&mut self, bus: u8, buf: &mut [u8]) -> Result<usize, ResponseCode> {
        self.check_bus(bus)?;
        // If we drained on a prior IRQ, serve from the internal buffer
        // first. Otherwise fall through to a polling hardware read.
        let buffered = self.slave_rx.data().len();
        if buffered > 0 {
            let n = buffered.min(buf.len());
            buf[..n].copy_from_slice(&self.slave_rx.data()[..n]);
            self.slave_rx.set_len(0);
            return Ok(n);
        }
        self.i2c.slave_read(buf).map_err(map_i2c_error)
    }

    fn slave_wait_event(
        &mut self,
        bus: u8,
        rx_buf: &mut [u8],
    ) -> Result<(SlaveEventKind, usize), ResponseCode> {
        self.check_bus(bus)?;
        // The peripheral exposes `slave_has_data()` polling but no
        // event-typed wait; synthesize a DataReceived event when bytes
        // arrive. Other event kinds (Stop, ReadRequest, etc.) need an
        // IRQ-driven notification path.
        loop {
            if self.i2c.slave_has_data() {
                let n = self.i2c.slave_read(rx_buf).map_err(map_i2c_error)?;
                return Ok((kind_from_event(SlaveEvent::DataReceived { len: n }), n));
            }
            core::hint::spin_loop();
        }
    }

    fn slave_set_response(&mut self, bus: u8, data: &[u8]) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        // `slave_write` pre-loads the TX buffer for the next master
        // read. Returns the byte count actually queued; we treat
        // partial loads as success (caller can re-arm).
        let _ = self.i2c.slave_write(data).map_err(map_i2c_error)?;
        Ok(())
    }

    fn enable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.notification_enabled = true;
        Ok(())
    }

    fn disable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode> {
        self.check_bus(bus)?;
        self.notification_enabled = false;
        Ok(())
    }

    fn drain_slave_rx(&mut self, bus: u8) -> Result<usize, ResponseCode> {
        self.check_bus(bus)?;
        self.drain_to_internal_buffer()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a wire-level bus index (0–13) to PAC register block pointers.
fn bus_ptrs(bus: u8) -> Option<(*const i2c::RegisterBlock, *const i2cbuff::RegisterBlock)> {
    use ast1060_pac::*;
    match bus {
        0 => Some((I2c::ptr(), I2cbuff::ptr())),
        1 => Some((I2c1::ptr(), I2cbuff1::ptr())),
        2 => Some((I2c2::ptr(), I2cbuff2::ptr())),
        3 => Some((I2c3::ptr(), I2cbuff3::ptr())),
        4 => Some((I2c4::ptr(), I2cbuff4::ptr())),
        5 => Some((I2c5::ptr(), I2cbuff5::ptr())),
        6 => Some((I2c6::ptr(), I2cbuff6::ptr())),
        7 => Some((I2c7::ptr(), I2cbuff7::ptr())),
        8 => Some((I2c8::ptr(), I2cbuff8::ptr())),
        9 => Some((I2c9::ptr(), I2cbuff9::ptr())),
        10 => Some((I2c10::ptr(), I2cbuff10::ptr())),
        11 => Some((I2c11::ptr(), I2cbuff11::ptr())),
        12 => Some((I2c12::ptr(), I2cbuff12::ptr())),
        13 => Some((I2c13::ptr(), I2cbuff13::ptr())),
        _ => None,
    }
}

fn map_i2c_error(e: I2cError) -> ResponseCode {
    match e {
        I2cError::NoAcknowledge => ResponseCode::NoDevice,
        I2cError::Timeout => ResponseCode::Timeout,
        I2cError::ArbitrationLoss => ResponseCode::ArbitrationLost,
        I2cError::Busy => ResponseCode::Busy,
        I2cError::Invalid | I2cError::InvalidAddress => ResponseCode::InvalidAddress,
        I2cError::BusRecoveryFailed => ResponseCode::BusStuck,
        I2cError::Bus | I2cError::Abnormal | I2cError::Overrun | I2cError::SlaveError => {
            ResponseCode::IoError
        }
    }
}

fn kind_from_event(ev: SlaveEvent) -> SlaveEventKind {
    match ev {
        SlaveEvent::ReadRequest | SlaveEvent::DataSent { .. } => SlaveEventKind::ReadRequest,
        SlaveEvent::WriteRequest
        | SlaveEvent::DataReceived { .. }
        | SlaveEvent::DataReceivedAndSent { .. } => SlaveEventKind::DataReceived,
        SlaveEvent::Stop => SlaveEventKind::Stop,
    }
}
