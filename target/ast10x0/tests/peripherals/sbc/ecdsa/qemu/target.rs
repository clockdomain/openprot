// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! ECDSA QEMU-amenable tests (goal.md §4.A).
//!
//! Exercises only what does not need an ECC-engine verdict. `qemu_only`: the
//! D3 assertion is true *because* the QEMU SBC has no ECC engine (ADR-4) — on
//! real silicon it would produce a verdict and (correctly) fail here, which is
//! why this must never run on the EVB. Verdict parity / NIST KAT live in
//! `../evb/` (hardware-only).

#![no_std]
#![no_main]

use ast10x0_peripherals::sbc::{SbcDevice, SbcError, SbcOp};
use codegen as _;
use console_backend::console_backend_write_all;
use entry as _;
use openprot_hal_blocking::ecdsa::{
    ErrorKind, P384PublicKey, P384Signature, PublicKey, Signature,
};
use target_common::{TargetInterface, declare_target};
use zerocopy::IntoBytes;

pub struct Target {}

/// 1. Operand-order pin (mandatory, goal.md §4.A). Locks the `#[repr(C)]`
/// field order and `coordinates()` order of the HAL key/sig types to
/// `x‖y` / `r‖s`, so a future HAL struct refactor cannot silently transpose
/// operands the façade writes to fixed SRAM slots (§1.2 step 5).
fn test_operand_order() -> Result<(), &'static str> {
    let x = [0xA1u8; 48];
    let y = [0xB2u8; 48];
    let pk = P384PublicKey::new(x, y);
    let (gx, gy) = pk.coordinates();
    if gx != x || gy != y {
        return Err("P384PublicKey::coordinates() order changed");
    }
    let pkb = pk.as_bytes();
    if pkb.len() != 96 || pkb[0] != 0xA1 || pkb[47] != 0xA1 || pkb[48] != 0xB2 || pkb[95] != 0xB2 {
        return Err("P384PublicKey #[repr(C)] layout changed (must be x||y)");
    }

    let r = [0xC3u8; 48];
    let s = [0xD4u8; 48];
    let sig = P384Signature::new(r, s);
    let (gr, gs) = sig.coordinates();
    if gr != r || gs != s {
        return Err("P384Signature::coordinates() order changed");
    }
    let sb = sig.as_bytes();
    if sb.len() != 96 || sb[0] != 0xC3 || sb[47] != 0xC3 || sb[48] != 0xD4 || sb[95] != 0xD4 {
        return Err("P384Signature #[repr(C)] layout changed (must be r||s)");
    }
    Ok(())
}

/// 2. Interface/structural reject (goal.md §2.3.3 / §4.A). The trait only
/// guarantees the non-zero invariant; the port must enforce it.
fn test_structural_reject() -> Result<(), &'static str> {
    let zero = [0u8; 48];
    let nz = [1u8; 48];
    if P384PublicKey::from_coordinates(zero, zero).is_ok() {
        return Err("all-zero point accepted");
    }
    if P384Signature::from_coordinates(zero, zero).is_ok() {
        return Err("all-zero signature accepted");
    }
    if P384PublicKey::from_coordinates(nz, nz).is_err() {
        return Err("non-zero point rejected");
    }
    if P384Signature::from_coordinates(nz, nz).is_err() {
        return Err("non-zero signature rejected");
    }
    // Compile-time pin: the kinds the SbcError mapping targets still exist.
    let _ = (ErrorKind::InvalidPoint, ErrorKind::InvalidSignature);
    Ok(())
}

/// 3. D3 bounded-timeout positive test (goal.md §2.1 / D3 / §4.A). QEMU has
/// no ECC engine, so `secure014` bit-20 never sets ⇒ `verify_raw` must
/// exhaust the (small) budget and return `SbcError::Timeout`. Asserting
/// that *is* the test of the lone intentional delta.
fn test_d3_timeout() -> Result<(), &'static str> {
    let b = [0x11u8; 48];
    // SAFETY: the test owns the SBC singleton for its whole lifetime and is
    // single-threaded — the non-reentrant `SbcDevice` contract holds.
    let mut dev = unsafe {
        SbcDevice::new(ast1060_pac::Secure::ptr(), |_| core::hint::spin_loop())
    }
    .with_timeout_polls(16);
    // SAFETY: no concurrent/reentrant ECDSA access in this test.
    let mut op = unsafe { SbcOp::from_device(&mut dev) };

    match op.verify_raw(&b, &b, &b, &b, &b) {
        Err(SbcError::Timeout) => Ok(()),
        Ok(()) => Err("unexpected PASS on QEMU — is the ECC engine modelled now?"),
        Err(_) => Err("unexpected non-timeout error on QEMU"),
    }
}

fn run() -> Result<(), &'static str> {
    pw_log::info!("=== AST10x0 ECDSA QEMU-amenable tests (goal.md §4.A) ===");
    test_operand_order()?;
    pw_log::info!("operand-order pin: PASS");
    test_structural_reject()?;
    pw_log::info!("structural reject: PASS");
    test_d3_timeout()?;
    pw_log::info!("D3 bounded-timeout: PASS");
    pw_log::info!("=== ECDSA QEMU-amenable tests complete ===");
    Ok(())
}

impl TargetInterface for Target {
    const NAME: &'static str = "AST10x0 ECDSA QEMU";

    fn main() -> ! {
        let sentinel: &[u8] = match run() {
            Ok(()) => b"TEST_RESULT:PASS\n",
            Err(error) => {
                pw_log::error!("ECDSA qemu test failed: {}", error as &str);
                b"TEST_RESULT:FAIL\n"
            }
        };
        let _ = console_backend_write_all(sentinel);
        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
