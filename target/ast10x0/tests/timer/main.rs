// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Timer QEMU test: TimerManager <-> object_wait seam. Scaffold stub.

#![no_main]
#![no_std]

use userspace::{entry, syscall};

#[entry]
fn entry() {
    pw_log::info!("timer test: scaffold up");
    let _ = syscall::debug_shutdown(Ok(()));
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
