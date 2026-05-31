# Lifecycle State Machine

A target-agnostic, event-driven state machine for the OpenPRoT platform
lifecycle: secure boot, firmware verification, recovery, update, and runtime.

This is a Rust port of the *architecture* of ASPEED's `AspeedStateMachine`
(`aspeed-zephyr-project/apps/aspeed-pfr`). The transition graph and the
"a handler does work, then emits the next event" drive model are carried over;
the Zephyr `smf.h` machinery, `union` event payloads, and manual allocation are
replaced with plain Rust enums, a pure transition function, and two injected
traits. It is `no_std` and `#![forbid(unsafe_code)]`.

## Crates

| Crate | Path | Role |
|-------|------|------|
| `openprot_lifecycle_api` | [`api/`](api/) | `State`, `Event`, the pure `transition` function, and the `wire` module (the stable byte codec for `Event`/`State`/`Request`/`Response`, shared by both channel ends). Data + logic only — no loop, queue, or I/O. |
| `openprot_lifecycle_sm`  | [`sm/`](sm/)   | The run-loop (`StateMachine`) plus the `EventQueue` and `Actions` traits the loop is generic over. |
| `openprot_lifecycle_server` | [`server/`](server/) | The lifecycle *service*: the server end of the channel. Decodes inbound `Request`s into `Event`s (via `api::wire`), drives the `StateMachine` to quiescence, and encodes `Response`s. Platform-independent and host-tested; mirrors `mctp/server`. |
| `openprot_lifecycle_ipc` | [`ipc/`](ipc/) | On-target `EventQueue` backed by a `pw_kernel` channel; decodes inbound events with `api::wire`. Target-only. |
| `openprot_ipc_event_queue` | [`//util/ipc_event_queue`](../../util/ipc_event_queue) | Generic, transport-free `PendingQueue`: the in-process buffer for handler follow-up events. No syscall dependency, so it builds and unit-tests on the host. |

## Design

The run-loop carries no OS, transport, or hardware dependency — matching how the
MCTP server keeps platform primitives out of its core. Two traits are injected:

- **`EventQueue`** — the blocking event source/sink. Replaces Zephyr's `k_fifo`.
  On-target it is backed by a `pw_kernel` IPC channel; in tests it is an
  in-memory `VecDeque`.
- **`Actions`** — the work run on entering a state (`verify`, `recover`,
  `update`, …). On-target each method calls into OpenPRoT services and HAL
  traits (`Digest`, `Ecdsa`, the fwupdate service); in tests it is a scripted
  double. Handlers return the follow-up `Event` to feed back into the loop — the
  contract that `GenerateStateMachineEvent` provided in the original.

```text
external producers (commands, watchdog, IRQ)
        │  push(Event)
        ▼
   ┌──────────┐  recv()   ┌──────────────┐  transition()  ┌───────────┐
   │EventQueue│──────────▶│ StateMachine │───────────────▶│  State    │
   └──────────┘           └──────────────┘                └───────────┘
        ▲                        │ run_state()
        │  push(follow-up)       ▼
        └──────────────── Actions (verify / recover / update / …)
```

## State graph

```text
Boot ──Start──▶ Init ──InitDone──▶ FirmwareVerify ──VerifyDone──▶ Runtime
                 │                       │
       InitRotSecondaryBooted     VerifyFailed │ UpdateRequested
                 ▼                       ▼            ▼
            RotRecovery          FirmwareRecovery  FirmwareUpdate
              │     │              │       │          │       │
       RecoveryDone RecoveryFailed │  RecoveryFailed  UpdateDone UpdateFailed
              ▼     ▼         RecoveryDone ▼          ▼          ▼
           Reboot  Lockdown   FirmwareVerify Lockdown FirmwareVerify FirmwareRecovery
```

`VerifyUnprovisioned` routes `FirmwareVerify → Unprovisioned`, and
`ProvisionCmd` returns `Unprovisioned → Init`. See `api/src/lib.rs::transition`
for the authoritative table.

## Testing

```console
bazel test //services/lifecycle/...
```

The `api` crate's unit tests assert individual transitions; the `sm` crate's
integration test drives the full boot/verify/recovery/update flows through the
real run-loop with an in-memory queue — no `pw_kernel`, no hardware.

## Wiring a target (e.g. ast1060)

A target provides the queue and the `Actions` implementation, then starts the
loop:

1. Use [`openprot_lifecycle_ipc::IpcEventQueue`](ipc/) for the `EventQueue`,
   constructed from the lifecycle task's codegen channel handle. `recv` blocks
   on the channel's `USER` signal (the `K_FOREVER` of the original
   `k_fifo_get`), reads one event, and decodes it; `push` buffers a handler's
   follow-up event in process and is drained ahead of the next channel read.
   Inbound transports, the watchdog, and reset-detect IRQs are separate
   producers that send encoded events into the same channel. (The crate is
   generic underneath — `IpcEventQueue` composes the host-tested `WireCodec`
   and `PendingQueue` from `//util/ipc_event_queue` with the `pw_kernel`
   syscalls.)
2. Implement `Actions`, where each handler calls the relevant OpenPRoT service /
   HAL trait and maps the result to a follow-up `Event`.
3. Choose a driver:
   - **Service / request-driven** — construct
     `LifecycleServer::new(actions)` and, in the task loop, read a request off
     the channel, call `server.handle_request(&req, &mut resp, &mut queue)`,
     and write `resp` back. The server decodes the request, runs the machine to
     quiescence (draining handler follow-ups via `EventQueue::try_recv`), and
     encodes the settled `State`. This is the `pw_kernel` server shape, the
     analogue of `StreamServer::handle_ipc`.
   - **Free-running** — `StateMachine::new().run(&mut queue, &mut actions)`.
     `run` returns `Result<Infallible, _>`: a healthy queue blocks forever in
     `recv`, returning only if the channel fails unrecoverably.

The ast1060 backends live under `target/ast10x0/`; the core `api`/`sm` crates
intentionally have no dependency on them. `IpcEventQueue` depends only on the
`pw_kernel` `userspace` crate, so it is target-only but not target-specific.

## Status

This introduces the **first** lifecycle/secure-boot component for an OpenPRoT
target. Per the project's development process, landing it (an architecture +
boot-process change) is a "large change" and should go through the RFC / TSC
review described in `docs/src/development-process.md`.
