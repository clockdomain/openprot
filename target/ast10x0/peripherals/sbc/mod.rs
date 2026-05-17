// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! AST10x0 Secure Boot Controller (SBC) public-key engine.
//!
//! One hardware block (`secure` register block `0x7e6f_2000` + SECSRAM
//! `0x7900_0000`) exposing multiple operations. ECDSA P-384 verify is
//! operation #1 (`hal_impl`, ported under `peripheral-parity-port`); RSA is
//! operation #2 (same block — shared trigger `0xbc` / status `0x14`, distinct
//! bits — its own parity-port, see `plans/`). `registers` is the single
//! Confined-`unsafe` MMIO façade; `device`/`op` are the shared
//! cooperative-yield bounded-poll layer over it.

mod constants;
mod device;
mod error;
mod hal_impl;
mod op;
mod registers;

pub use device::SbcDevice;
pub use error::SbcError;
pub use op::SbcOp;
pub use registers::SbcRegisters;
