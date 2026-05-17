// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Crypto smoke client for the AST10x0 image.
//!
//! Performs one whole-object SHA-256 `Digest::digest` round-trip over a fixed
//! input via the abstract `crypto_traits` seam (wrapped in
//! `crypto_traits::Stack`, the consumer pattern — sketch §3.7/ADR-C1: the
//! consumer never names `CryptoClient` or the transport directly past the
//! wiring line). It checks the response is `Ok` and the returned length equals
//! the algorithm digest size, then signals pass/fail through the same
//! semihosting exit path the usart client2 uses (`syscall::debug_shutdown`).

#![no_main]
#![no_std]

use app_crypto_client::handle;
use crypto_client::CryptoClient;
use crypto_traits::{Algo, Stack};
use userspace::entry;
use userspace::syscall;

// Fixed input; expected SHA-256 is checked only by length here (the digest
// value itself is exercised by the hardware-only KAT harness, not this smoke
// test — sketch §7).
const INPUT: &[u8] = b"openprot crypto smoke";

#[entry]
fn entry() {
    // Wiring line: the binary picks the backend (IPC CryptoClient → server);
    // everything below sees only the abstract `Stack` facade.
    let mut stack = Stack::new(CryptoClient::new(handle::CRYPTO));

    let mut out = [0u8; 64]; // large enough for any Algo digest
    let expected = Algo::Sha256.digest_len();

    let result = match stack.digest(Algo::Sha256, INPUT, &mut out) {
        Ok(n) if n == expected => {
            pw_log::info!("crypto SHA-256 digest ok ({} bytes)", n as u32);
            Ok(())
        }
        Ok(n) => {
            pw_log::error!(
                "crypto SHA-256 returned unexpected length {} (want {})",
                n as u32,
                expected as u32
            );
            Err(pw_status::Error::Internal)
        }
        Err(_) => {
            pw_log::error!("crypto SHA-256 digest failed");
            Err(pw_status::Error::Internal)
        }
    };

    let _ = syscall::debug_shutdown(result);
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
