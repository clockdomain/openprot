// Licensed under the Apache-2.0 license

//! # MCTP over I2C Transport Binding
//!
//! This crate provides the I2C transport binding for the MCTP server,
//! ported from the Hubris `mctp-server/src/i2c.rs`.
//!
//! It implements [`mctp_stack::Sender`] for outbound MCTP-over-I2C packets
//! and provides [`MctpI2cReceiver`] for decoding inbound I2C target messages
//! into MCTP packets.
//!
//! ## Changes from Hubris
//!
//! - Hubris `drv_i2c_api::I2cDevice` → OpenPRoT `I2cClientBlocking` trait
//! - Hubris `TaskId` → generic `I2cClientBlocking` implementor
//! - All MCTP protocol logic (encoding, fragmentation, PEC) preserved as-is
//!   via `mctp_stack::i2c::MctpI2cEncap`

#![no_std]
#![warn(missing_docs)]

mod receiver;
mod sender;

pub use receiver::MctpI2cReceiver;
pub use sender::I2cSender;
