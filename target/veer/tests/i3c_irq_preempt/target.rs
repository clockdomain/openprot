// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Kernel image for the VeeR I3C userspace-IRQ test. Boots and hands control
//! to the userspace app processes declared in system.json5.

#![no_std]
#![no_main]

use console_backend as _;
use entry::exit;
use target_common::{declare_target, TargetInterface};

pub struct Target {}

impl TargetInterface for Target {
    const NAME: &'static str = "Caliptra I3C Userspace IRQ Test";

    fn main() -> ! {
        codegen::start();
        #[expect(clippy::empty_loop)]
        loop {}
    }

    fn shutdown(code: u32) -> ! {
        match code {
            0 => pw_log::info!("PASS"),
            _ => pw_log::info!("FAIL: {}", code),
        }
        exit(code);
    }
}

declare_target!(Target);
