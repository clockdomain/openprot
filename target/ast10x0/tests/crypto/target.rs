// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 Crypto Service Target
//!
//! This target runs the crypto server as a userspace process owning the HACE
//! (Symmetric) engine. A client communicates with it over one `crypto` IPC
//! channel.

#![no_std]
#![no_main]

use cortex_m_semihosting::debug::{EXIT_FAILURE, EXIT_SUCCESS, exit};
use target_common::{TargetInterface, declare_target};
use {console_backend as _, entry as _};

pub struct Target {}

impl TargetInterface for Target {
    const NAME: &'static str = "AST10x0 Crypto Service";

    fn main() -> ! {
        codegen::start();
        #[expect(clippy::empty_loop)]
        loop {}
    }

    fn shutdown(code: u32) -> ! {
        let status = if code == 0 { EXIT_SUCCESS } else { EXIT_FAILURE };
        exit(status);
        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
