// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! ECDSA NIST KAT — HARDWARE ONLY (goal.md §4.B / correctness authority
//! §2.3.2). Runs the pinned NIST CAVP P-384/SHA-384 SigVer vectors
//! (`vectors.rs`, see `nist-reference/PINNED.txt`) through the real ECC
//! engine and checks each accept/reject verdict.
//!
//! Cannot run on QEMU: the SBC model has no ECC engine (goal.md ADR-4) — it
//! would time out on every vector. Hence `hardware`-tagged + qemu-incompatible
//! in BUILD.bazel; run on an AST1060 EVB via `--config=k_ast1060_evb`.

#![no_std]
#![no_main]

mod vectors;

use ast10x0_peripherals::sbc::{SbcDevice, SbcError, SbcOp};
use codegen as _;
use console_backend::console_backend_write_all;
use entry as _;
use target_common::{TargetInterface, declare_target};
use vectors::NIST_P384_SHA384_SIGVER;

pub struct Target {}

fn run_kat() -> Result<(), &'static str> {
    pw_log::info!("=== AST10x0 ECDSA NIST P-384/SHA-384 SigVer KAT ===");
    pw_log::info!(
        "{} pinned NIST CAVP vectors",
        NIST_P384_SHA384_SIGVER.len() as u32
    );

    let mut failures: u32 = 0;
    for (i, v) in NIST_P384_SHA384_SIGVER.iter().enumerate() {
        // SAFETY: the test owns the SBC singleton for its whole lifetime and
        // is single-threaded — the non-reentrant `SbcDevice` contract holds.
        let mut dev =
            unsafe { SbcDevice::new(ast1060_pac::Secure::ptr(), |_| core::hint::spin_loop()) };
        // SAFETY: no concurrent/reentrant ECDSA access in this test.
        let mut op = unsafe { SbcOp::from_device(&mut dev) };

        let got = op.verify_raw(&v.qx, &v.qy, &v.r, &v.s, &v.m);
        let ok = matches!(
            (got, v.expect_valid),
            (Ok(()), true) | (Err(SbcError::VerificationFailed), false)
        );
        let kind: &str = if v.expect_valid { "valid" } else { "invalid" };
        if ok {
            pw_log::info!(
                "vec[{}] {} ({}): PASS",
                i as u32,
                kind as &str,
                v.note as &str
            );
        } else {
            failures += 1;
            let cause: &str = match got {
                Ok(()) => "accepted; expected reject",
                Err(SbcError::VerificationFailed) => "rejected; expected accept",
                Err(SbcError::Timeout) => "engine timeout (wedged / wrong SRAM base?)",
                Err(_) => "unexpected error",
            };
            pw_log::error!(
                "vec[{}] {} ({}): FAIL - {}",
                i as u32,
                kind as &str,
                v.note as &str,
                cause as &str
            );
        }
    }

    if failures == 0 {
        pw_log::info!(
            "=== all {} NIST vectors PASS ===",
            NIST_P384_SHA384_SIGVER.len() as u32
        );
        Ok(())
    } else {
        pw_log::error!("=== {} NIST vector(s) FAILED ===", failures as u32);
        Err("NIST P-384/SHA-384 SigVer KAT failed")
    }
}

impl TargetInterface for Target {
    const NAME: &'static str = "AST10x0 ECDSA NIST KAT";

    fn main() -> ! {
        let sentinel: &[u8] = match run_kat() {
            Ok(()) => b"TEST_RESULT:PASS\n",
            Err(error) => {
                pw_log::error!("ECDSA NIST KAT failed: {}", error as &str);
                b"TEST_RESULT:FAIL\n"
            }
        };
        let _ = console_backend_write_all(sentinel);
        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
