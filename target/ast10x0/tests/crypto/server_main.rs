// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Crypto server binary for the AST10x0 smoke image.
//!
//! Constructs both capability-class backends from the platform `crypto_backend`
//! crate (the only crate naming silicon — `Symmetric → HACE`,
//! `PublicKey → SBC` stub), wires the single `crypto` IPC channel into the
//! server's wait group exactly as the usart `server_main` does, then hands
//! control to `crypto_server::runtime::run`.
//!
//! Unlike the usart server there is no IRQ object: every crypto op is
//! whole-object / run-to-completion, so the runtime has no IRQ-park branch
//! (sketch §3.1/§3.2) and `run` takes only `(sym, pk, wg)`.

#![no_main]
#![no_std]

use app_crypto_server::handle;
use crypto_backend::{PublicKeyBackend, SymmetricBackend};
use crypto_server::runtime;
use userspace::entry;
use userspace::syscall::{self, Signals};

#[entry]
fn entry() {
    // Symmetric → HACE (real, borrow-arbitrated); PublicKey → SBC (stub).
    let mut sym = SymmetricBackend::new();
    let mut pk = PublicKeyBackend::new();

    // The binary owns wait_group_add (same convention as usart's server_main):
    // register the one `crypto` channel with its own handle as user_data so
    // the runtime can route wake-ups by channel handle. One channel carries
    // both capability classes (sketch §6/§7).
    let _ = syscall::wait_group_add(
        handle::WG,
        handle::CRYPTO,
        Signals::READABLE,
        handle::CRYPTO as usize,
    );

    runtime::run(&mut sym, &mut pk, handle::WG);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
