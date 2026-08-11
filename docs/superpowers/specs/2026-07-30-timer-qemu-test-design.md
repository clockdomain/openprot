# Design: TimerManager ↔ `object_wait` QEMU test (`target/ast10x0/tests/timer`)

Date: 2026-07-30
Status: approved (design review with user)

## Goal

Prove the production seam that `services/orchestrator/timer`'s crate docs
promise: `TimerManager<T, Id, N>` instantiated with the real runtime types —
`userspace::time::Instant` as `T` and orchestrator-sm's `ComponentId` as `Id` —
multiplexing boot/commit watchdog deadlines into `syscall::object_wait`, running
on the ast10x0 QEMU target.

Success criteria:

- `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer/...` passes
  (UART sentinel `TEST_RESULT:PASS`).
- `bazel test //target/ast10x0/tests/timer/...` without the config builds the
  image and skips execution, like every other suite under `target/ast10x0/tests`.
- The wait loop in the test has the same shape as the intended orchestrator
  runtime loop (block in `object_wait` on {event object, next timer deadline}),
  so the test doubles as a reference for the runtime that will consume the crate.

## Non-goals

- No orchestrator-sm state-machine wiring (no `Event::Timeout` /
  `Event::CommitTimeout` mapping) — the sm crate is a dependency only for its
  `ComponentId` type, proving the intended instantiation.
- No two-process IPC composition, no wait groups.
- No upper-bound timing assertions — QEMU scheduling jitter would make them
  flaky. Lower bounds only.
- The cancel/late-fire *race* is not asserted here; the crate's unit tests and
  the sm crate's stale-timeout handling own that. This test asserts the
  non-racy cancellation contract (`cancel_boot` → `next_deadline() == None`).

## Layout and composition

New directory `target/ast10x0/tests/timer/user/`, mirroring
`target/ast10x0/tests/interrupts/user`:

- `target.rs` — kernel target. Copies the established pattern:
  `codegen::start()` in `main`, and `shutdown(code)` writes
  `TEST_RESULT:PASS\n` (code 0) or `TEST_RESULT:FAIL\n` to the console backend.
- `main.rs` (userspace app, e.g. app name `test_timer`) — the test proper.
  Single process, single thread.
- `system.json5` — armv7m layout adapted from the closest single-app config;
  one app with one process containing:
  - one `interrupt` object bound to an otherwise-unused IRQ (e.g. 44, mirroring
    the interrupts test's use of 42/43), self-fired via
    `syscall::debug_trigger_interrupt` — the "event arrived" source;
  - one thread object.
  Exact flash/RAM addresses are worked out in the implementation plan by
  adapting the closest existing config (PMSAv7 power-of-2 alignment rules).
- `BUILD.bazel` — same rule set as the sibling suites: `rust_binary` for the
  kernel target, the app target (`rust_app`/`rust_binary` per the in-repo
  pattern, e.g. `tests/mctp/ipc_client`), `target_codegen`,
  `target_linker_script`, `system_image`, `system_image_test`,
  `rust_binary_no_panics_test`, `TARGET_COMPATIBLE_WITH`.

App dependencies: `@pigweed//pw_kernel/userspace`,
`//services/orchestrator/timer:orchestrator_timer`,
`//services/orchestrator/sm:orchestrator_sm` (for `ComponentId`),
`@pigweed//pw_log/rust:pw_log`, generated `app_test_timer` codegen (handles +
IRQ constants).

## The wait loop

The app's core loop is the intended runtime shape:

```rust
let deadline = tm.next_deadline().unwrap_or(Instant::MAX);
match syscall::object_wait(handle::TIMER_IRQ, signals::TEST_IRQ, deadline) {
    Err(Error::DeadlineExceeded) => {
        let now = SystemClock::now();
        while let Some(expired) = tm.poll(now) {
            // record expiry for assertions
        }
    }
    Ok(_) => {
        syscall::interrupt_ack(handle::TIMER_IRQ, signals::TEST_IRQ)?;
        // event path: the "component reported in" analogue
    }
    Err(e) => fail(e), // any other error is a test failure
}
```

`TimerManager` is instantiated as
`TimerManager<Instant, ComponentId, N>` with a small `N` (e.g. 4).

## Scenarios (run in sequence, each logged via `pw_log`)

1. **Ordered expiry, tie-break, one-shot.** Arm `Boot(C0)` at now+50 ms,
   `Boot(C1)` at now+100 ms, `Commit` at now+100 ms. Drive the wait loop; expect
   expiries in order `Boot(C0)`, then `Boot(C1)`, then `Commit`:
   - `Boot(C1)` before `Commit` exercises the boot-before-commit tie-break at
     the shared +100 ms deadline;
   - after each drain, `poll` returns `None` (one-shot);
   - at each fire, `SystemClock::now() - t0 >= armed offset` (lower bound only).
2. **Re-arm replaces, not stacks.** Arm `Boot(C0)` at now+30 ms, immediately
   re-arm at now+80 ms. Expect exactly one fire, with elapsed ≥ 80 ms, and
   `poll` → `None` afterwards.
3. **Cancel via event.** Arm `Boot(C0)` at now+500 ms. Fire the test IRQ via
   `debug_trigger_interrupt`; `object_wait` must return `Ok` (event before
   deadline). Ack the interrupt, call `cancel_boot(C0)`, assert
   `next_deadline() == None`. Nothing left armed → scenario complete without
   any timeout having fired.

Completion: all scenarios pass → `debug_shutdown(Ok(()))` → kernel `shutdown(0)`
→ `TEST_RESULT:PASS`. Any assertion failure or unexpected `object_wait` error →
log the detail, `debug_shutdown(Err(...))` → `TEST_RESULT:FAIL`.

## Error handling

- Every syscall result is checked; unexpected errors abort the test as FAIL
  with the error logged — no silent retries.
- Duration arithmetic uses the `userspace::time::Duration`/`Instant` API; the
  test derives millisecond durations from the clock's tick rate rather than
  hard-coding tick counts.

## Testing

This *is* a test; its own verification is:

- `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer/...` — passes.
- `bazel test //target/ast10x0/...` — still green (no regression to the tree;
  new image builds without a runner).
- `bazel test //services/orchestrator/timer:orchestrator_timer_test` — host
  unit tests unaffected.

## Risks / open items

- `time::Instant<SystemClock>` must satisfy the crate's `T: Copy + Ord` bound.
  Expected to hold (pigweed `time` crate); verified at first compile. If it
  does not, the fallback is wrapping ticks (`u64`) directly, at the cost of the
  typed-instant proof.
- IRQ number 44 assumed unused on the `ast1030-evb` QEMU machine; the
  interrupts test's 42/43 precedent suggests the range is safe. Adjust if the
  build's codegen or QEMU rejects it.
- QEMU virtual-clock granularity: 30/50/80/100 ms offsets are chosen well above
  tick granularity; if the 30 ms vs 80 ms re-arm distinction proves flaky, both
  offsets scale up together.
