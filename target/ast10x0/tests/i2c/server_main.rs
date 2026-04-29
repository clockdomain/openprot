// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_main]
#![no_std]

use app_i2c_server::{handle, signals};
use ast10x0_peripherals::i2c::I2cConfig;
use i2c_backend::Backend;
use i2c_server::runtime;
use userspace::entry;
use userspace::syscall::{self, Signals};
use userspace::time::Instant;

/// Bus this server task owns. Must match the MMIO mapping and IRQ
/// declarations in `system.json5` (currently bus 0 → I2C controller
/// at `0x7e7b_0080`, IRQ 110).
const BUS_ID: u8 = 0;

/// Yield closure threaded into the peripheral driver. Sleeps the task
/// on the wait-group's I2C signal until the controller raises its IRQ
/// (TX complete, RX, NACK, slave event, …), then acks. Replaces the
/// busy-loop polling — the task is fully passive between hardware
/// events.
///
/// `_ns` (the requested wait window) is intentionally ignored: the
/// underlying syscall blocks until the IRQ fires.
fn wait_for_i2c_irq(_ns: u32) {
    let _ = syscall::object_wait(handle::WG, signals::I2C, Instant::MAX);
    // Re-arm the kernel's IRQ subscription so the next edge wakes us.
    let _ = syscall::interrupt_ack(handle::I2C_IRQ, signals::I2C);
}

#[entry]
fn entry() -> ! {
    // Register both event sources up front so the wait group is fully
    // populated before the peripheral has a chance to issue a yield.
    let _ = syscall::wait_group_add(
        handle::WG,
        handle::I2C,
        Signals::READABLE,
        handle::I2C as usize,
    );
    let _ = syscall::wait_group_add(
        handle::WG,
        handle::I2C_IRQ,
        signals::I2C,
        handle::I2C_IRQ as usize,
    );
    // Prime the NVIC: the kernel only enables an IRQ in the NVIC the
    // first time userspace acks it. Without this initial ack, even a
    // software-triggered IRQ stays pending forever.
    let _ = syscall::interrupt_ack(handle::I2C_IRQ, signals::I2C);

    // SAFETY: this server task exclusively owns I2C bus `BUS_ID`'s
    // peripherals via the system.json5 device mapping; constructor
    // runs once at startup.
    let mut backend = unsafe {
        match Backend::new(BUS_ID, I2cConfig::default(), wait_for_i2c_irq) {
            Ok(b) => b,
            Err(_) => loop {},
        }
    };

    // Always-on notification: every I2C IRQ → drain slave RX → raise
    // USER on the IPC channel so any client blocked on
    // `object_wait(handle::I2C, USER, …)` wakes and can call
    // `SlaveReceive` to read what was drained.
    runtime::run(
        &mut backend,
        handle::WG,
        handle::I2C_IRQ,
        signals::I2C,
        BUS_ID,
        handle::I2C,
    );
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
