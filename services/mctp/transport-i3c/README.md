# openprot-mctp-transport-i3c

I3C transport binding for the MCTP server.

## Overview

This crate implements MCTP-over-I3C transport. It provides the `Sender`
implementation for outbound packets and a receiver/decoder for inbound frames,
driving the `services/i3c` target service over its IPC seam.

`mctp-estack` ships `i2c`, `serial` and `usb` bindings but no I3C one, so the
DSP0233 header handling lives here in `encap`. Fragmentation and reassembly
stay in the MCTP stack, which is transport-independent.

## Key Types

- `I3cSender<T>` — implements `mctp_lib::Sender`; handles fragmentation,
  encoding and optional PEC via `MctpI3cEncap`
- `MctpI3cReceiver` — decodes inbound I3C frames into MCTP packets
- `MctpI3cEncap` — adds and removes the four-byte transport header

## How it differs from the I2C binding

The two are deliberately shaped alike, but three things are forced by I3C:

- **This side is a target, not a bus master.** It cannot start a transfer, so
  sending stages a TX and raises an IBI for the controller to collect by
  private read — one `I3cOp::Send` carrying the whole framed packet. The I2C
  sender trims the leading destination byte because embedded-hal prepends it;
  nothing is trimmed here.
- **The source address is runtime state.** I3C dynamic addresses are assigned
  by ENTDAA, so `I3cSender` reads the address back per message rather than
  taking it at construction. Sending before enumeration is `Unreachable`.
- **PEC is optional.** I3C protects the transfer at the link layer, so both
  `encode` and `decode` take an explicit flag instead of assuming one.

## MTU

`get_mtu()` reports `MCTP_I3C_IPC_MAXMTU` (241), not the DSP0233 ceiling
`MCTP_I3C_MAXMTU` (254). The i3c service carries one framed packet per
`I3cOp::Send`, capped at `i3c_api::MAX_PAYLOAD` (250 — the caliptra i3c-core
private write limit), and a framed packet is:

```text
4 (I3C header) + 4 (MCTP header) + payload + 1 (PEC) <= 250  =>  payload <= 241
```

The MCTP header is inside what gets framed because `Fragmenter` emits
`mtu + MctpHeader::LEN` bytes per packet. Reporting 254 here would produce
frames the service rejects as `TooLong`, and only for large payloads.

## Testing

```bash
bazel test //services/mctp/transport-i3c/...
```

- `mctp_transport_i3c_test` — encapsulation unit tests (header round-trip,
  framing rejection, PEC, buffer limits)
- `mctp_transport_i3c_roundtrip_test` — `Server::send()` through `I3cSender`,
  the real `i3c_server::dispatch`, and back out through `MctpI3cReceiver`;
  no kernel, no emulator

## Dependencies

- `i3c_api`, `i3c_client` — the i3c target service seam and client
- `mctp-lib` — `Sender` trait, fragmentation
- `mctp` — core MCTP types
- `pw_log` — logging
