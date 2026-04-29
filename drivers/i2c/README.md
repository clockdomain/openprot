# I2C Driver Model

This document describes the architecture of the layered I2C userspace driver
under `drivers/i2c/` and how it integrates with platform bindings in a
target-agnostic way.

For migration history, design tradeoffs, and the phased plan, see
[`MIGRATION_PLAN.md`](MIGRATION_PLAN.md).

## 1. Layer Overview

```
┌──────────────────────────────────────────────────────────┐
│  Application / Client                                    │
│  IpcI2cClient  (drivers/i2c/client)                      │
│  channel_transact(request) → response                    │
└────────────────────────┬─────────────────────────────────┘
                         │  Pigweed IPC channel
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Server Binary                                           │
│    (target/<plat>/tests/i2c:i2c_server_bin)              │
│  rust_app — wires codegen handles + backend + runtime    │
│  wait_group_add ×N  →  runtime::run                      │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Server Library  (drivers/i2c/server:i2c_server)         │
│  runtime::run — object_wait → channel_read               │
│               → dispatch_request → channel_respond       │
│               IRQ branch: drain_slave_rx + USER notify   │
│  dispatch_request — pure protocol→backend translator     │
└────────────────────────┬─────────────────────────────────┘
                         │  I2cBackend trait
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Platform Backend  (target/<plat>/backend/i2c)           │
│  PlatformI2cBackend : I2cBackend                         │
│  pub type Backend = PlatformI2cBackend                   │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│  PAC-level I2C driver  (platform peripherals crate)      │
│  Raw MMIO handle over vendor PAC RegisterBlock           │
└──────────────────────────────────────────────────────────┘
```

## 2. Crate Map

| Bazel target | Crate | Role |
|---|---|---|
| `//drivers/i2c/api` | `i2c_api` | Wire protocol + `I2cBackend` trait + client/target trait surface |
| `//drivers/i2c/server:i2c_server` | `i2c_server` | Dispatch + runtime loop library (platform-binding-agnostic within Pigweed kernel targets) |
| `//drivers/i2c/client:i2c_client` | `i2c_client` | Client facade library (`IpcI2cClient`) |
| `//drivers/i2c/backend-mock:i2c_backend_mock` | `i2c_backend` | Host-side loopback backend for dispatcher tests |

Concrete platform binding lives under `target/<plat>/`. A reference
implementation for the AST10x0 ships at
[`target/ast10x0/tests/i2c/`](../../target/ast10x0/tests/i2c/) — see that
directory's README for hardware specifics.

## 3. Wire Protocol  (`i2c_api::wire`)

Operations are identified by a 1-byte opcode in `I2cRequestHeader` (8 bytes):

```text
+--------+--------+--------+----------+-----------+----------+
| op (1) | flags  | bus(1) | addr (1) | write_len | read_len |
+--------+--------+--------+----------+-----------+----------+
                                       u16 LE      u16 LE
```

Followed by payload: `write_data (write_len bytes)`.

| Op | Code | Notes |
|---|---|---|
| `Write` | 0x00 | controller write |
| `Read` | 0x01 | controller read |
| `WriteRead` | 0x02 | combined transaction with repeated start |
| `Transaction` | 0x03 | reserved (multi-op transaction) |
| `Probe` | 0x04 | controller probe |
| `ConfigureSpeed` | 0x05 | reconfigure clock |
| `RecoverBus` | 0x06 | clock pulses + STOP to unstick the bus |
| `ConfigureSlave` | 0x07 | set slave address |
| `EnableSlave` / `DisableSlave` | 0x08 / 0x09 | slave receive arm/disarm |
| `SlaveReceive` | 0x0A | poll buffered RX |
| `SlaveWaitEvent` | 0x0B | poll-blocking event wait |
| `SlaveSetResponse` | 0x0C | pre-load TX for next master read |
| `EnableSlaveNotification` / `DisableSlaveNotification` | 0x0D / 0x0E | gate IRQ-driven wakeups |

`I2cResponseHeader` (4 bytes) carries a `ResponseCode` plus a payload length.

## 4. Backend Trait  (`i2c_api::backend`)

```rust
pub trait I2cBackend {
    fn write(&mut self, bus: u8, addr: u8, data: &[u8]) -> Result<(), ResponseCode>;
    fn read(&mut self, bus: u8, addr: u8, buf: &mut [u8]) -> Result<(), ResponseCode>;
    fn write_read(
        &mut self,
        bus: u8,
        addr: u8,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> Result<(), ResponseCode>;
    fn probe(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode>;
    fn configure_speed(&mut self, bus: u8, speed_hz: u32) -> Result<(), ResponseCode>;
    fn recover_bus(&mut self, bus: u8) -> Result<(), ResponseCode>;

    fn configure_slave(&mut self, bus: u8, addr: u8) -> Result<(), ResponseCode>;
    fn enable_slave(&mut self, bus: u8) -> Result<(), ResponseCode>;
    fn disable_slave(&mut self, bus: u8) -> Result<(), ResponseCode>;
    fn slave_receive(&mut self, bus: u8, buf: &mut [u8]) -> Result<usize, ResponseCode>;
    fn slave_wait_event(
        &mut self,
        bus: u8,
        rx_buf: &mut [u8],
    ) -> Result<(SlaveEventKind, usize), ResponseCode>;
    fn slave_set_response(&mut self, bus: u8, data: &[u8]) -> Result<(), ResponseCode>;
    fn enable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode>;
    fn disable_slave_notification(&mut self, bus: u8) -> Result<(), ResponseCode>;

    fn drain_slave_rx(&mut self, bus: u8) -> Result<usize, ResponseCode>;
}
```

The trait returns `ResponseCode` directly — there's no separate
`BackendError` layer. The wire-level error vocabulary already has the
right shape (`NoDevice`, `ArbitrationLost`, `Timeout`, …).

`bus: u8` is the wire-level identifier on every method. Each backend
chooses how to validate it (single-bus, multi-bus, etc.) — that's a
backend-implementation concern.

## 5. Per-Target Binding

`drivers/i2c/` ships only platform-agnostic libraries. Every binary that
names a specific `system.json5` lives next to that config under the
platform's tree.

```python
# target/<plat>/.../BUILD.bazel  (skeleton)
rust_app(
    name = "i2c_server_bin",
    srcs = ["server_main.rs"],
    codegen_crate_name = "app_i2c_server",
    system_config = ":system_config",
    deps = [
        "//drivers/i2c/server:i2c_server",
        "//target/<plat>/backend/i2c:i2c_backend_<plat>",
        "@pigweed//pw_kernel/userspace",
    ],
)

rust_app(
    name = "i2c_client_app",
    srcs = ["client_main.rs"],
    codegen_crate_name = "app_i2c_client",
    system_config = ":system_config",
    deps = [
        "//drivers/i2c/client:i2c_client",
        "@pigweed//pw_kernel/userspace",
        "@pigweed//pw_log/rust:pw_log",
    ],
)
```

Each backend target uses `crate_name = "i2c_backend"` and exports
`pub type Backend: I2cBackend`, so `server_main.rs` can stay generic
across backends:

```rust
use i2c_backend::Backend;
let mut backend = unsafe { Backend::new(/* platform-specific args */)? };
```

## 6. Server Library (`i2c_server`)

Two pieces, both platform-binding-agnostic within Pigweed kernel targets:

- `dispatch_request<B: I2cBackend>(backend, request, response) -> usize` —
  pure protocol→backend translator. No IPC, no OS dependency. Host-
  testable against the mock backend.
- `runtime::run<B>(backend, wg, irq, irq_signals, bus_id, notify_channel) -> !`
  — the dispatch loop. The IRQ branch unconditionally drains slave RX
  and raises `Signals::USER` on `notify_channel`, so any client blocked
  on `object_wait(channel, USER, …)` wakes and can call `SlaveReceive`.
  Pass `notify_channel = 0` to skip the peer signal.

The binary owns every `wait_group_add` call because only the codegen-
aware binding knows which handles exist. The binary also supplies the
peripheral driver's yield closure (the platform's choice of "what to
do between status reads in the polling loop").

## 7. Extension Points

- **New platform**: create `target/<plat>/tests/i2c/` (or a non-test
  binding directory if it's a real deployment) with a `system.json5`,
  `server_main.rs`, and a `rust_app` that depends on
  `//drivers/i2c/server:i2c_server` plus the platform's backend.
  Nothing under `drivers/i2c/` changes.
- **New operation**: add opcode to `I2cOp`, extend `I2cBackend`, add
  arm to `dispatch_request`, update the protocol doc.
- **New backend**: implement `I2cBackend` in a `rust_library` with
  `crate_name = "i2c_backend"` exporting `pub type Backend`. The
  host-side `backend-mock` crate is the simplest reference.
