# PLDM/Orchestrator notify channel — prototype report

Status of the control-plane IPC channel between the PLDM firmware device and
the Boot Orchestrator, covering phases 2 and 3 of
[`pldm-notify-prototype.plan.md`](../../../../pldm-notify-prototype.plan.md).
The design it implements is
[supervisor-as-initiator](./pldm-orchestrator-ipc-alt.md).

## Where it stands

| | |
|---|---|
| Host tests | 31/31 pass (35 added) |
| QEMU seam test | passes — two processes on a real kernel |
| Kernel crates | build and pass clippy for the target |
| `supervisor::run` | **never executed** |

The channel works end to end on a kernel. The orchestrator run loop that is
meant to drive it does not yet run anywhere.

## What was built

Phase 2 delivered the six steps of the plan:

- **A poll cadence in `services/orchestrator/timer`.** A third deadline class
  beside the boot and commit watchdogs. Deadlines due at the same instant are
  reported boot, then commit, then poll: the watchdogs are safety obligations
  and the poll is only a cadence tick.
- **The run loop in `services/orchestrator/server/src/supervisor.rs`.** Two-part
  registration (`wait_group_add` for `Signals::USER`, then a protocol-level
  `Subscribe`), a single park on the nearest deadline, drain every expiry, then
  poll. A poll-due and a nudge arriving in the same tick coalesce into one
  round-trip rather than burning two deadlines.
- **The mapping in `services/orchestrator/adapters/pldm/src/notify.rs`.** Wire
  values to core events, effects to reported phases, and the peer-health policy.
  Host-testable, and deliberately out of the kernel-tagged loop that calls it.
- **`services/pldm/notify-client-ipc`** — the transport, bounded.
- **`services/pldm/notify-server-runtime`** — PLDM's channel front end, plugged
  into the terminus loop through the existing `FdEventSink` seam.

Phase 3 added a two-process system image and QEMU test under
`target/ast10x0/tests/orchestrator/notify/`.

### The seam is one process, not two

`NotifyChannel` implements `FdEventSink`, and `run_terminus` calls
`sink.service()` once per iteration. The notify server therefore lives *inside*
the PLDM firmware-device process, on the same thread as the terminus loop. It
is not a separate task. Everything about the timing contract below follows from
that.

## Decisions worth challenging

**No wire message can assert verification.** No value arriving from PLDM maps to
`Event::UpdateVerified`. That event asserts the staged image passed
authentication — a verdict only the orchestrator's own crypto path may produce.
The mapping is exhaustive and a test asserts the property directly, so a future
variant breaks the build rather than opening a hole.

**`Pending::Abort` reuses `Event::UpdateRejected`.** A cancel maps onto the event
that discards the staged image and returns the machine to `Ready`. The core has
no separate aborted event and adding one means touching every handler in a
heavily-tested reducer. Safe in the direction that matters — both paths discard
— but it is a conflation, and it is the call most worth arguing about.

**The poll is not a core event.** Draining a deadline yields either a
state-machine event or "time to poll". Folding the poll into the core's event
type would have been less code and would have taught a deliberately
transport-free reducer that PLDM exists.

**The unwind goes through the core, not around it.** On timeout the loop does not
execute `Effect::DiscardStaged` directly. It dispatches the event that makes the
core emit it, so staging is released *and* the machine leaves `Updating` rather
than waiting forever on a peer that is gone.

**Bad health is one-way, for now.** A condemned peer is never re-admitted.
`pw_kernel` supplies process supervision primitives (`Signals::JOINABLE`,
`task_join`, `process_start` — see its `examples/adventure`), so the honest
framing is that this is right *until* restart-and-resubscribe is wired, not
permanently. The QEMU image already declares the process object for that.

## The timing contract

PLDM answers the channel from its terminus loop, so its responsiveness is
bounded by how often that loop comes around — and when idle, the loop parks on
the caller's full MCTP responder timeout. A supervisor with a bounded deadline
would time out against a firmware device that is perfectly healthy and merely
waiting for an Update Agent command, and because the health verdict is one-way
that condemns it permanently, on a schedule rather than on a fault. The worst
case is at startup, where the first `subscribe()` could fail and the channel
would never carry a message.

The fix is a servicing interval the sink declares
(`FdEventSink::max_service_interval_millis`) which caps the loop's idle poll. It
is a cap, never an extension, and defaults to "no constraint" so a sink with no
peer does not pay for a faster idle poll it does not need.

Both halves live together in `notify-api` — `MAX_SERVICE_INTERVAL_MILLIS` and
`MIN_TRANSACT_TIMEOUT_MILLIS` — because the relationship between them is the
thing that matters, and two constants in separate crates would drift.

**The 50 ms figure is an estimate, not a measurement.** See below.

## What running it turned up

Three findings that no amount of host testing would have produced.

**Logging inside a bounded window.** A debug line added between the wake and the
read cost more than the entire 50 ms budget: a console write under QEMU is
slower than the round-trip it was instrumenting, so the peer timed out and the
kernel tore down the transaction before the response landed. The diagnostic
caused the failure it was added to explain. Nothing may log on that path, and
the same window exists in the real firmware device.

**`time::sleep_until` does not delay on this target.** It returns early — already
documented in a comment in the SGPIO IRQ test. A loop paced with it is a tight
spin, which starved the peer process and made the failure mode change when a
single log line was added. The test now paces on the channel itself, which
yields properly and models a device parked on its transport more faithfully.

**The 50 ms round-trip budget is unvalidated.** Emulated, the first round-trip
takes roughly 200 ms, dominated by console writes and scheduling rather than the
exchange, so the QEMU test uses ten times the contract value and proves
boundedness rather than the number. The number itself needs measuring on
silicon, where a 200 MHz part with no console traffic should behave very
differently.

## Known gaps

- **`supervisor::run` has never executed.** The QEMU test drives the client and
  policy pieces directly; the run loop needs a state machine, a platform driver
  and a chain in the image.
- **The pre-transfer veto does nothing.** `dispatch` parses and validates the
  accept/reject answer and drops it: it has no handle on the `CmdInterface` to
  park a response against.
- **Nothing arms the watchdogs.** The loop drains boot and commit deadlines, but
  the platform driver never arms them, so the commit window — the load-bearing
  example in the argument for this design — is never armed.
- **A latched event can be lost.** The latch is a single slot and a new event
  overwrites the old. Harmless while only one event is in flight, which is all
  the intake path does; not harmless once the full transfer sequence runs.
- **The transfer choreography** beyond intake, abort and status is not wired, and
  `Phase` cannot express the design's `Receiving`/`Authenticating` states.

## Next

1. Run `supervisor::run` itself under QEMU.
2. Measure the timing contract on hardware and replace the estimate.
3. Wire the veto through to the `CmdInterface`.
4. Arm the boot and commit watchdogs from the platform driver.
5. Make the latch lossless, or document the invariant that makes a single slot
   sufficient.
6. Supervise and restart PLDM instead of condemning it permanently.
7. Decide the long-term wait strategy: MCTP has no notification path today and
   its receive is a deferred-response long poll in which PLDM blocks as a
   client, so "one park over both sources" needs new work in the MCTP server.
   Splitting PLDM into two threads avoids that at the cost of concurrency in the
   component with the largest attack surface.
