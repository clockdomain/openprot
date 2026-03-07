# Port Hubris `ast1060-mctp-i2c-echo` to OpenPRoT (Pigweed)

## Context

**Source:** `hubris/app/ast1060-mctp-i2c-echo/`
A Hubris RTOS application running MCTP-over-I2C echo on an AST1060 RoT.

| Task | Crate | Role |
|------|-------|------|
| `jefe` | `task-jefe` | Supervisor / fault manager |
| `idle` | `task-idle` | Idle task |
| `mctp_echo` | `mctp-echo` | Listens for MCTP type-1 requests, echoes payload back |
| `mctp_server` | `mctp-server` | MCTP transport layer (features: `serial_log`, `transport_i2c`) |
| `uart_driver` | `drv-ast1060-uart` | UART peripheral driver |
| `i2c_driver` | `drv-mock-i2c` | Mock I2C driver (feature: `mock-only`) |

**Target:** `openprot/`
Cargo workspace + Bazel, targeting the **Pigweed kernel** (pw_kernel). Existing services follow `api/` + `server/` + `backend-*/` pattern (see `services/i2c/`). Platform integration is Pigweed-only — the MCTP server will run as a **userspace process** under pw_kernel, not a Hubris task or Linux process.

## Porting Principle

**Preserve as much of the original Hubris code as possible.** Only change what is OS-dependent. The MCTP protocol logic, packet handling, server state management, transport encoding/decoding, and application logic (echo) should be carried over as-is. The only parts that change are:

- **IPC mechanism**: Hubris `idol` / `sys_reply` / `Leased` → Pigweed `pw_kernel` IPC/channels
- **Task/process model**: Hubris `task_slot!` / `sys_recv_open` / notifications → Pigweed userspace process event loop
- **Driver APIs**: Hubris `drv-i2c-api` / `ast1060-uart-api` → OpenPRoT `services/i2c/` userspace driver
- **Build system**: Hubris `app.toml` + `build.rs` code generation → Bazel BUILD files + `system.json5`

Everything else — the `Server` struct, `Router` integration, `Sender` implementations, handle management, timeout logic, MCTP type definitions — should remain structurally identical to the Hubris originals.

---

## Phase 1: MCTP Service API (`services/mctp/api`) — COMPLETE

Create the platform-independent MCTP types and traits crate.

- [x] Create `services/mctp/api/` directory structure
- [x] `Cargo.toml` — `openprot-mctp-api` crate
- [x] `src/lib.rs` — `Handle`, `RecvMetadata` types
- [x] `src/error.rs` — `ResponseCode`, `MctpError` (ported from hubris `mctp-api` `ServerError`)
- [x] `src/traits.rs` — `MctpClient`, `MctpListener`, `MctpReqChannel`, `MctpRespChannel`
- [x] Add to workspace `Cargo.toml`
- [x] Verify `cargo check` passes

## Phase 2: MCTP Server Core (`services/mctp/server`) — COMPLETE

Create the platform-independent server logic crate.

- [x] Create `services/mctp/server/` directory structure
- [x] `Cargo.toml` — `openprot-mctp-server` crate
- [x] `src/lib.rs` + `src/server.rs` — `Server` struct with EID mgmt, pending recv tracking, timeouts
- [x] Add to workspace `Cargo.toml`
- [x] Verify `cargo check` passes
- [x] Integrate `mctp-stack` (`mctp-lib`) `Router` as the packet processing engine
- [x] Re-export `mctp_stack::Sender` trait for transport bindings
- [x] Wire up inbound packet → Router → pending recv fulfillment via `Server::inbound()` + `Server::update()`

## Phase 3: I2C Transport Binding (`services/mctp/transport-i2c`) — COMPLETE

Port the I2C transport from hubris `mctp-server/src/i2c.rs`, using the I2C userspace driver at `services/i2c/` as the underlying transport.

- [x] Create `services/mctp/transport-i2c/` crate
- [x] Implement `Sender` for I2C using the `services/i2c/` userspace driver (client API + IPC to I2C server)
- [x] Implement inbound I2C → MCTP packet decoding (using `mctp-stack::i2c::MctpI2cHandler`)
- [x] Use `I2cTargetClient` from `services/i2c/api` for slave/target mode receive
- [x] Echo integration test with client/server partition (`DirectClient` implementing `MctpClient`, 2 tests passing)

## Phase 4: Serial Transport Binding (`services/mctp/transport-serial`) — NOT STARTED

Port the serial transport from hubris `mctp-server/src/serial.rs`. (Lower priority than I2C.)

- [ ] Create `services/mctp/transport-serial/` crate
- [ ] Implement `Sender` for serial (using `embedded-io::Write`, not hubris `ast1060-uart-api`)
- [ ] Implement inbound serial → MCTP packet decoding (using `mctp-stack::serial::MctpSerialHandler`)

## Phase 5: MCTP Echo Application — IN PROGRESS

Port the echo task from hubris `task/mctp-echo/`.

- [x] IPC wire protocol (`services/mctp/api/src/wire.rs`) — request/response encoding for all MCTP operations
- [x] IPC client (`services/mctp/client/`) — `IpcMctpClient` implementing `MctpClient` via wire protocol + IPC
- [x] Server-side IPC dispatch (`services/mctp/server/src/dispatch.rs`) — decodes wire requests, calls `Server`
- [x] Wire-protocol dispatch integration test (`tests/dispatch.rs`) — full round-trip through wire encoding
- [x] Echo binary as Pigweed userspace process (`target/ast1060-evb/mctp/mctp_echo.rs`)
- [ ] Wire up with server + I2C transport for an end-to-end demo (needs real I2C sender in server `main.rs`)

## Phase 6: Pigweed Platform Integration — MOSTLY DONE

Wire up the MCTP server as a Pigweed userspace process on the AST1060-EVB (`target/ast1060-evb/`).

- [x] MCTP server `main.rs`: event loop driven by pw_kernel IPC/channels
  - Replaces hubris `sys_recv_open` / notifications / `idol` IPC dispatch
  - Uses `dispatch_mctp_op` for IPC request handling
  - Follows the pattern established by `services/i2c/server/`
  - Uses NoopSender for initial bring-up; real I2cSender wiring is TODO
- [x] Connect `IpcMctpClient::send_recv` to `syscall::channel_transact` (gated behind `pigweed` feature)
- [x] Bazel BUILD files for each new crate (following `services/i2c/` pattern)
- [x] `system.json5` entry for MCTP server + echo processes (`target/ast1060-evb/mctp/`)
- [x] Integration with `target/ast1060-evb/` platform definition (`target.rs`, `BUILD.bazel`)

## Phase 7: Testing & Documentation — PARTIALLY DONE

- [x] Wire protocol unit tests (7 tests in `api/src/wire.rs`)
- [x] Echo integration tests with client/server partition (2 tests in `server/tests/echo.rs`)
- [x] Wire-protocol dispatch integration tests (2 tests in `server/tests/dispatch.rs`)
- [x] README for each MCTP crate (`api/`, `server/`, `client/`, `transport-i2c/`)
- [ ] QEMU-based end-to-end test (following `services/i2c/` test pattern)
- [ ] Update `docs/src/specification/middleware/mctp.md` with implementation status

---

## Current Status

**Phases 1–3, 5, and 6 mostly complete.** All library code, IPC dispatch, Bazel BUILD files, system configuration, and echo binary are written. 11 tests pass. Remaining work: wire up real I2cSender in server main.rs (replace NoopSender), QEMU e2e test, and docs update.

## Key Dependencies

| Crate | Source | Role |
|-------|--------|------|
| `mctp` | workspace (types crate) | `Eid`, `MsgType`, `Tag`, `Error` etc. |
| `mctp-stack` / `mctp-lib` | `github.com/9elements/mctp-lib` branch `buildup` | `Router`, `Sender`, fragmentation, serial/I2C handlers |
| `services/i2c/` | I2C userspace driver | I2C client/target/server — MCTP transport-i2c uses this as its underlying I2C transport |
| `heapless` | workspace | `no_std` collections |
| `zerocopy` | workspace | Zero-copy serialization |
| Pigweed (`pw_kernel`) | Bazel via `MODULE.bazel` | Userspace processes, IPC channels, system image |
