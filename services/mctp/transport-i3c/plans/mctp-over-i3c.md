# MCTP over I3C — implementation plan

Completes the I3C transport binding for the MCTP server. The encapsulation
layer landed in `4706184`; this plan covers everything between that and a
working MCTP exchange on the VeeR emulator.

## Starting point

| Piece | State |
|---|---|
| `services/mctp/transport-i3c/src/encap.rs` | done — DSP0233 header, optional PEC, 12 host tests |
| `services/i3c/{api,server,server-runtime}` | done — `Send`/`Recv`/`DynamicAddress`, dispatch, single-frame latch |
| `target/veer/tests/i3c_service/client.rs` | prototype IPC client, self-described as standing in for the unwritten `services/i3c/client-ipc` |
| `transport-i3c/src/{sender,receiver}.rs` | missing |
| `services/i3c/{client,client-ipc}` | missing |

`services/i3c/` has three of the five crates the service shape prescribes
(`docs/src/architecture.md`, `services/i2c/README.md`). The missing client is
what blocks the binding: `transport-i3c` currently depends on nothing but
`@rust_crates//:mctp`, where `transport-i2c` depends on `//services/i2c/api`.

## Constraints that shape the design

### MTU: the protocol ceiling exceeds the IPC frame

`MCTP_I3C_MAXMTU` is 254 (`encap.rs:67`), so a maximum frame is
`4 + 254 + 1 = 259` bytes with PEC. But `i3c_api::MAX_PAYLOAD` is 250,
documented as matching the caliptra i3c-core private read/write limit — a
hardware bound that cannot move.

**Decision:** `I3cSender::get_mtu()` returns `MCTP_I3C_IPC_MAXMTU = 241`, a
new constant in `encap.rs` documented against the IPC frame limit.
`MCTP_I3C_MAXMTU` stays as the DSP0233 protocol ceiling.

The budget is not `250 - 4 - 1`. `Fragmenter::fragment_vectored` computes
`fragment_length = mtu + MctpHeader::LEN` (`fragment.rs:132`, `LEN == 4`), so
the packet it emits — and that `encode` then frames — already carries the
4-byte MCTP transport header. One whole I3C frame is therefore:

    4 (I3C hdr) + 4 (MCTP hdr) + payload + 1 (PEC)  <=  250   =>   payload <= 241

Defining the constant in terms of `i3c_api::MAX_PAYLOAD` rather than as a
literal keeps it correct if the hardware limit ever moves.
Fragmentation lives in the MCTP stack and honours `get_mtu()`, so the cap is
enough — but left unaddressed it fails only on large payloads, as a `TooLong`
from the server rather than anything diagnosable at the MCTP layer.

### The dynamic address is runtime state, not construction state

`I2cSender::new()` takes a fixed `own_addr`. I3C addresses are assigned by the
controller during ENTDAA, which is why `encap` takes `own_addr` per instance
and the service exposes `I3cOp::DynamicAddress`.

**Decision:** `I3cSender` reads the address through the client on every send
rather than caching it. A cache is never revisited after a successful read, so
a bus reset and re-ENTDAA would leave a stale source address in every outbound
header — frames that still encode and send cleanly, so the fault is invisible
locally. The cost is one extra IPC round-trip per message; a cache invalidated
on send-failure is a possible later optimization (see the code review), but the
address's runtime lifetime is why the fresh read is the default.

### Direction: this side is a target, not a bus master

`I2cSender` is a master writing over `embedded_hal::i2c::I2c`. The I3C side is
a target: "send" stages a TX and raises an IBI for the controller to collect by
private read, which is what `I3cOp::Send` already does. So `I3cSender` is
generic over the i3c client, not over an embedded-hal bus.

One consequence: `I2cSender` skips `packet[1..]` because embedded-hal prepends
the destination byte itself. That is an embedded-hal artifact and must **not**
be carried over — `I3cOp::Send` takes the whole framed payload.

## Phase 1 — promote the client out of the test

Gives `services/i3c/` the five-crate shape and unblocks everything downstream.

1. `services/i3c/api/src/transport.rs` — `Transport` trait and `TransportError`,
   ported from `services/i2c/api/src/transport.rs`: bytes in, bytes out, one
   round-trip.
2. `services/i3c/client` — host-buildable, generic over `Transport`, holding all
   marshalling. Wraps `encode_request`/`decode_response` in typed methods
   (`send`, `recv`, `dynamic_address`) returning `Result<_, ClientError>`.
3. `services/i3c/client-ipc` — `IpcTransport` over `channel_transact`, kernel
   tagged, `TARGET_COMPATIBLE_WITH` from `//target/veer:defs.bzl` (not ast10x0;
   the i3c stack is veer-only today).
4. `services/i3c/server` — add `LoopbackTransport` calling `dispatch()` directly,
   so host tests drive the real client against a mock `I3cTarget`.
5. Refactor `target/veer/tests/i3c_service/client.rs` onto the new crate.

**Done when:** `//target/veer/tests/i3c_service:i3c_service_test` still passes
unchanged. That test already walks the whole dispatch surface, so an unchanged
pass is what proves the extraction faithful.

## Phase 2 — sender and receiver

1. `transport-i3c/src/sender.rs` — `I3cSender<C>` implementing
   `mctp_lib::Sender` (`send_vectored`, `get_mtu`), structured like
   `transport-i2c/src/sender.rs`: fragmentation loop, `MctpI3cEncap::encode`,
   error mapping to `mctp::Error::TxFailure`.
2. `transport-i3c/src/receiver.rs` — `MctpI3cReceiver` wrapping
   `MctpI3cEncap::decode`, producing packets for `Server::inbound()`.
3. `transport-i3c/src/lib.rs` — export the sender, receiver, and the encap
   items a consumer needs (the encode/decode entry points, the header type, and
   the MTU/size constants).
4. `BUILD.bazel` — add `//services/i3c/api`, `//services/i3c/client`,
   `@rust_crates//:mctp-lib`, `@pigweed//pw_log/rust:pw_log`.

## Phase 3 — host tests and wiring

1. `sender_receiver_roundtrip` mirroring the I2C test: drive `Server::send()`
   through `I3cSender` into a capture transport, decode with `MctpI3cReceiver`,
   assert the payload survives.
2. `single_fragment_produces_one_send`, and a fragmentation test that crosses
   the 241-byte cap — the case the MTU decision above exists to handle. Assert
   every framed write is `<= i3c_api::MAX_PAYLOAD`; that is the bound the cap
   exists to hold, and the one a wrong MTU breaks.
3. `transport-i3c/README.md`, modelled on `transport-i2c/README.md`.
4. Register the test in `//services/mctp:mctp_host_tests`, which lists no
   transport tests at all today.

## Phase 4 — on-target

`target/veer/tests/mctp_i3c/`, modelled on `i3c_service`: a two-process image
(MCTP server + i3c server) with a host harness driving a real MCTP request over
the emulator's I3C socket via `//target/veer/tests/i3c_host`. Tagged
`emulator` + `exclusive` like its siblings — the runner hardcodes
`--i3c-port=65534`.

## Verification

Phases 1–3 are host-buildable; only Phase 4 needs the emulator.

```bash
bazel test //services/i3c/... //services/mctp/...          # phases 1-3
bazel test //target/veer/tests/i3c_service:i3c_service_test # phase 1 gate
bazel test //target/veer/tests/mctp_i3c:mctp_i3c_test       # phase 4
./pw presubmit                                              # before each commit
```

## Process note

`docs/src/development-process.md` asks for an `[RFC]` issue for new components
before PRs land. Skipped deliberately for this work at the owner's direction;
the design decisions above stand in for it and should be carried into the PR
description.
