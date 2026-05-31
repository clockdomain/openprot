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
| `openprot_lifecycle_api` | [`api/`](api/) | `State`, `Event`, and the pure `transition` function. Data + logic only — no loop, queue, or I/O. |
| `openprot_lifecycle_sm`  | [`sm/`](sm/)   | The run-loop (`StateMachine`) plus the `EventQueue` and `Actions` traits the loop is generic over. |

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

A target provides the two trait implementations and starts the loop:

1. Implement `EventQueue` over a `pw_kernel` IPC channel (`recv` blocks on the
   channel; `push` enqueues). Inbound transports, the watchdog, and reset-detect
   IRQs all `push` into the same channel.
2. Implement `Actions`, where each handler calls the relevant OpenPRoT service /
   HAL trait and maps the result to a follow-up `Event`.
3. `StateMachine::new().run(&mut queue, &mut actions)` in the lifecycle task.

The ast1060 backends live under `target/ast10x0/`; this crate intentionally has
no dependency on them.

## Status

This introduces the **first** lifecycle/secure-boot component for an OpenPRoT
target. Per the project's development process, landing it (an architecture +
boot-process change) is a "large change" and should go through the RFC / TSC
review described in `docs/src/development-process.md`.
