// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Example out-of-tree board on the AST10x0 SoC.
//!
//! Boots openprot's AST10x0 entry code and console backend, announces
//! itself, and reports success so the QEMU harness can treat the image as a
//! passing test.

#![no_std]
#![no_main]

use console_backend::console_backend_write_all;
use entry as _;
use target_common::{declare_target, TargetInterface};

pub struct Target {}

impl TargetInterface for Target {
    const NAME: &'static str = "OpenPRoT example out-of-tree board (AST10x0)";

    fn main() -> ! {
        pw_log::info!("=== OpenPRoT example out-of-tree board ===");
        let _ = console_backend_write_all(b"example board: kernel up\nTEST_RESULT:PASS\n");

        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
