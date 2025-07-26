// Licensed under the Apache-2.0 license

//! Blocking/synchronous HAL traits for OpenPRoT
//!
//! This crate re-exports embedded-hal 1.0 traits for blocking hardware abstraction
//! layer operations such as SPI, I2C, GPIO, and other hardware interfaces.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Reset and Clock Control
pub mod system_control;

// Re-export embedded-hal 1.0 traits
pub use embedded_hal::delay::DelayNs;
pub use embedded_hal::digital::{InputPin, OutputPin, StatefulOutputPin};
pub use embedded_hal::i2c::{I2c, SevenBitAddress, TenBitAddress};
pub use embedded_hal::spi::{SpiBus, SpiDevice};
