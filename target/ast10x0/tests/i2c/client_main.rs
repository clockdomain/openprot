// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_main]
#![no_std]

//! Smoke test: validates the kernel boots, both processes start, the
//! server's `Backend::new` runs (calls `init_i2c_global` + per-bus
//! `Ast1060I2c::new`), the IPC channel handle binds, and the client
//! can issue `debug_shutdown` for a clean QEMU exit.
//!
//! The notification path is wired end-to-end at the system level —
//! server's runtime IRQ branch drains slave RX and raises
//! `Signals::USER` on the channel via `object_set_peer_user_signal` —
//! but exercising it requires a real I2C event source (a slave
//! attached in QEMU). A future companion app can
//! `object_wait(handle::I2C, USER, …)` then issue `SlaveReceive` to
//! consume the drained bytes.

use app_i2c_client::handle;
use i2c_client::IpcI2cClient;
use userspace::entry;
use userspace::syscall;

#[entry]
fn entry() -> ! {
    let _client = IpcI2cClient::new(handle::I2C);
    pw_log::info!("i2c client up; shutting down");

    let _ = syscall::debug_shutdown(Ok(()));
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
