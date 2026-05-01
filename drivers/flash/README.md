# Flash Driver Model

This document describes the architecture of the layered flash userspace
driver under `drivers/flash/` and how it integrates with platform
bindings in a target-agnostic way.

The operation set is taken from caliptra-mcu-sw's
[`runtime/kernel/drivers/flash`](../../../caliptra-mcu-sw/runtime/kernel/drivers/flash)
HIL, repackaged onto Pigweed IPC channels.

## 1. Layer Overview

```
┌──────────────────────────────────────────────────────────┐
│  Application / Client                                    │
│  FlashClient  (drivers/flash/client)                     │
│  channel_transact(request) → response                    │
└────────────────────────┬─────────────────────────────────┘
                         │  Pigweed IPC channel
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Server Binary                                           │
│    (target/<plat>/tests/flash:flash_server_bin)          │
│  rust_app — wires codegen handles + backend + runtime    │
│  wait_group_add ×N  →  runtime::run                      │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Server Library  (drivers/flash/server:flash_server)     │
│  runtime::run — object_wait → channel_read               │
│               → dispatch_request → channel_respond       │
│  dispatch_request — pure protocol→backend translator     │
└────────────────────────┬─────────────────────────────────┘
                         │  FlashBackend trait
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Platform Backend  (target/<plat>/backend/flash)         │
│  PlatformFlashBackend : FlashBackend                     │
│  pub type Backend = PlatformFlashBackend                 │
└────────────────────────┬─────────────────────────────────┘
                         │  raw MMIO / SMC controller
                         ▼
┌──────────────────────────────────────────────────────────┐
│  Peripheral driver  (platform peripherals crate)         │
│  e.g. AST10x0 SMC controller (FMC / SPI1 / SPI2) over    │
│  vendor PAC RegisterBlock                                │
└──────────────────────────────────────────────────────────┘
```

## 2. Crate Map

| Bazel target | Crate | Role |
|---|---|---|
| `//drivers/flash/api` | `flash_api` | Wire protocol + backend trait contract |
| `//drivers/flash/server:flash_server` | `flash_server` | Dispatch + runtime loop library (platform-binding-agnostic within Pigweed kernel targets) |
| `//drivers/flash/client:flash_client` | `flash_client` | Client facade library (platform-binding-agnostic within Pigweed kernel targets) |
| `//target/<plat>/tests/flash:flash_server_bin` | binary | Per-platform smoke-test server binding (TODO) |
| `//target/<plat>/tests/flash:flash_client_app` | binary | Per-platform smoke-test client binding (TODO) |
| `//target/<plat>/backend/flash` | `flash_backend` | Per-platform backend implementation (TODO) |

## 3. Wire Protocol  (`flash_api::protocol`)

Operations are identified by a 1-byte opcode in `FlashRequestHeader`
(16 bytes, `repr(C, packed)`, little-endian):

| Op | Value | Inputs | Notes |
|---|---|---|---|
| `Exists` | 0x01 | — | Probe; response `value` = 0. |
| `GetCapacity` | 0x02 | — | Response `value` = total capacity in bytes. |
| `GetChunkSize` | 0x03 | — | Response `value` = max bytes per Read/Write. |
| `Read` | 0x04 | `address`, `length` | Response payload carries the bytes read; `value` = byte count. |
| `Write` | 0x05 | `address`, `length`, payload | `length` must equal `payload_len`. Response `value` = bytes written. |
| `Erase` | 0x06 | `address`, `length` | Both must be multiples of `FlashInfo::erase_size`. |

`FlashRequestHeader` layout (16 B):

```
op_code: u8   flags: u8   payload_len: u16
address: u32
length:  u32
reserved: u32
```

`FlashResponseHeader` (8 B) carries a `FlashError` status code, a
`payload_len` (non-zero only on `Read`), and a generic `value` word
used by op-specific returns:

```
status: u8   reserved: u8   payload_len: u16
value:  u32
```

All structures implement `zerocopy` traits for zero-copy serialization.

## 4. Backend Trait  (`flash_api::backend`)

```rust
pub struct FlashInfo {
    pub capacity: u32,
    pub chunk_size: u32,
    pub erase_size: u32,
}

pub trait FlashBackend {
    fn info(&self) -> FlashInfo;
    fn read(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BackendError>;
    fn write(&mut self, address: u32, data: &[u8]) -> Result<usize, BackendError>;
    fn erase(&mut self, address: u32, length: u32) -> Result<(), BackendError>;
    fn enable_interrupts(&mut self, _: IrqMask) -> Result<(), BackendError> { Ok(()) }
    fn disable_interrupts(&mut self, _: IrqMask) -> Result<(), BackendError> { Ok(()) }
}
```

`BackendError` maps 1-to-1 onto `FlashError` via `From<BackendError> for FlashError`.

The trait is synchronous and buffer-borrowing, in contrast to the
caliptra-mcu-sw `FlashStorage` HIL which is callback-based: the server
runtime drives concurrency rather than the backend.

### Async completion

`FlashBackend::enable_interrupts` / `disable_interrupts` and the
`IrqMask::OPERATION_COMPLETE` bit are placeholders — flash v1 has no
parked-IRQ retry path. Backends that cannot complete synchronously
return `BackendError::WouldBlock`; the response carries
`FlashError::WouldBlock` and the **client** retries. When a real async
backend lands we'll add a parked-request slot to `runtime::run` so the
server can defer `channel_respond` until the backend's done-IRQ fires,
without changing the wire protocol.

## 5. Per-Target Binding

`drivers/flash/` ships only platform-agnostic libraries (`flash_api`,
`flash_server`, `flash_client`). Every binary that names a specific
`system.json5` lives next to that config under the platform's tree.

The first concrete binding will be on AST10x0, where the flash hardware
is the SMC peripheral (FMC + SPI1 + SPI2). Planned layout:

```python
# target/ast10x0/tests/flash/BUILD.bazel  (planned)
rust_app(
    name = "flash_server_bin",
    srcs = ["server_main.rs"],
    codegen_crate_name = "app_flash_server",
    system_config = ":system_config",
    deps = [
        "//drivers/flash/server:flash_server",
        "//target/ast10x0/backend/flash:flash_backend_ast10x0",
        "@pigweed//pw_kernel/userspace",
    ],
)

rust_app(
    name = "flash_client_app",
    srcs = ["client_main.rs"],
    codegen_crate_name = "app_flash_client",
    system_config = ":system_config",
    deps = [
        "//drivers/flash/client:flash_client",
        "@pigweed//pw_kernel/userspace",
        "@pigweed//pw_log/rust:pw_log",
    ],
)
```

Each backend target uses `crate_name = "flash_backend"` and exports
`pub type Backend: FlashBackend`, so `server_main.rs` stays generic
across backends:

```rust
use flash_backend::Backend;
let mut backend = Backend::new();
flash_server::runtime::run(&mut backend, wg);
```

The AST10x0 backend will sit on top of the existing
[target/ast10x0/peripherals/smc](../../target/ast10x0/peripherals/smc/)
driver, which already provides a layered API over the FMC / SPI
controller registers (see
[mcu-sw-flash.md](../../target/ast10x0/peripherals/smc/mcu-sw-flash.md)
for the design discussion that motivated this driver layout).

## 6. Server Library (`flash_server`)

Two pieces, both platform-binding-agnostic within Pigweed kernel targets:

- `dispatch_request<B: FlashBackend>(backend, request, response) -> DispatchOutcome`
  — pure protocol→backend translator. No IPC, no OS dependency.
- `runtime::run<B>(backend, wg) -> !` — the dispatch loop.
  Topology-agnostic: the binary registers each channel with its handle
  as `user_data`, and the loop derives the channel handle from
  `wait_return.user_data` directly.

The binary owns every `wait_group_add` call because only the
codegen-aware binding knows which handles exist.

## 7. Extension Points

- **New platform**: create `target/<plat>/tests/flash/` (or a
  non-test binding directory if it's a real deployment) with a
  `system.json5`, `server_main.rs`, and a `rust_app` that depends on
  `//drivers/flash/server:flash_server` plus the platform's backend.
  Nothing under `drivers/flash/` changes.
- **New operation**: add opcode to `FlashOp`, extend `FlashBackend`,
  add arm to `dispatch_request`, update protocol section above.
- **New backend**: implement `FlashBackend` in a `rust_library` with
  `crate_name = "flash_backend"` exporting `pub type Backend`.

## 8. Relationship to caliptra-mcu-sw

The op set, error model, address+length read/write/erase shape, and
the `capacity` / `chunk_size` reporting are all lifted from
[caliptra-mcu-sw's `FlashStorage` HIL](../../../caliptra-mcu-sw/runtime/kernel/drivers/flash/src/hil.rs)
and [`flash_storage_cmd` syscall driver](../../../caliptra-mcu-sw/runtime/userspace/syscall/src/flash.rs).
What is *not* re-used is the implementation: caliptra runs on Tock with
a callback-based async HIL and a syscall ABI, whereas this driver runs
on Pigweed with synchronous backends and an IPC ABI. The patterns map
onto each other piece-for-piece — see
[mcu-sw-flash.md](../../target/ast10x0/peripherals/smc/mcu-sw-flash.md)
for the full mapping table.

## 9. Design Notes

- u32 address + length carried in the request header. Keeps the wire
  header at 16 B; matches caliptra's `flash_storage_cmd` u32 arguments.
- 8 B response header with a generic `value` word so that
  `GetCapacity` / `GetChunkSize` / `Write` can return a 32-bit result
  without payload.
- No `Configure` op — flash geometry is static per backend and reported
  via `GetCapacity` / `GetChunkSize`.
- No parked-IRQ path in v1 (see §4 above).

## 10. Open Questions

- **Protocol versioning.** v1 has no version field. The `flags` byte and
  `reserved: u32` in `FlashRequestHeader` are both unused, but `reserved`
  is neither zero-checked by the server nor repurposed by clients — it's
  forwards-compat-shaped without the discipline that would make it one.
  A v2 client that put data in `reserved` would be silently accepted by
  a v1 server, which would ignore the field and execute the request with
  v1 semantics: silent drift rather than a clean error. Two directions
  to pick from before any out-of-tree consumer depends on the wire:
  - **Keep `reserved`, enforce must-be-zero.** Server rejects non-zero
    `reserved` with `InvalidOperation`. Treat `flags` as
    feature-negotiation bits — a future extension (e.g. 64-bit addresses)
    sets a `flags` bit and repurposes `reserved` as the high u32. Costs
    4 bytes per request; gains an in-place extension slot.
  - **Drop `reserved`, version via new opcodes.** Header shrinks 16 → 12 B.
    When semantics need to change, add `ReadV2` etc. rather than
    extending an existing op. `flags` stays for per-op modifiers only.
