# openprot-mctp-transport-i2c

I2C transport binding for the MCTP server, ported from Hubris `mctp-server/src/i2c.rs`.

## Overview

This crate implements MCTP-over-I2C transport. It provides the `Sender` implementation for outbound packets and a receiver/decoder for inbound I2C target messages. It uses the OpenPRoT `services/i2c/` userspace driver as the underlying I2C transport, replacing the Hubris `drv_i2c_api::I2cDevice`.

## Key Types

- `I2cSender<C>` — implements `mctp_stack::Sender` for I2C; handles fragmentation, encoding, and PEC (Packet Error Checking) via `mctp_stack::i2c::MctpI2cEncap`
- `MctpI2cReceiver` — decodes inbound I2C target-mode messages into MCTP packets

## Dependencies

- `openprot-mctp-api` — API traits
- `openprot-i2c-api` — `I2cClientBlocking` trait for I2C bus access
- `mctp-stack` — `Sender` trait, I2C encapsulation/decapsulation
- `mctp` — core MCTP types
- `embedded-hal` — hardware abstraction
- `heapless` — `no_std` collections
