# TimerManager ↔ `object_wait` QEMU Test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A QEMU system-image test at `target/ast10x0/tests/timer/` proving that `TimerManager<userspace::time::Instant, ComponentId, N>` correctly multiplexes boot/commit watchdog deadlines into `syscall::object_wait` — ordered expiry, tie-break, one-shot, re-arm-replaces, and cancel-on-event.

**Architecture:** One system image mirroring `target/ast10x0/tests/interrupts/user`: a kernel `target.rs` whose `shutdown(code)` writes the `TEST_RESULT:PASS/FAIL` UART sentinel, plus a single-process `rust_app` (`test_timer`) that runs three scenarios sequentially and reports via `syscall::debug_shutdown`. The app waits on an interrupt object (self-fired via `debug_trigger_interrupt`) with deadline = `TimerManager::next_deadline()` — the same loop shape the future orchestrator runtime will use.

**Tech Stack:** Bazel + pigweed pw_kernel tooling (`rust_app`, `system_image`, `system_image_test`), QEMU `ast1030-evb` via `--config=virt_ast10x0`, Rust 2024 no_std.

**Spec:** `docs/superpowers/specs/2026-07-30-timer-qemu-test-design.md`

## Global Constraints

- Branch: `timer-qemu-test` (already created, based on `upstream/main`).
- All new files carry the repo license header: `// Licensed under the Apache-2.0 license` + `// SPDX-License-Identifier: Apache-2.0` (`#`-style in BUILD/json5 files).
- Timing assertions are **lower-bound only** (`elapsed >= armed offset`). Never add an upper bound — QEMU jitter makes them flaky.
- Test IRQ number is **44** (interrupts test uses 42/43; must not collide).
- Dependency labels (both already `//visibility:public`):
  `//services/orchestrator/timer:orchestrator_timer` (crate `openprot_orchestrator_timer`),
  `//services/orchestrator/sm:orchestrator_sm` (crate `openprot_orchestrator_sm`).
- The QEMU run command (used by several steps):
  `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
- The build-only command: `bazel build //target/ast10x0/tests/timer/...`
- Commit messages follow repo convention: `ast10x0: <what>` prefix, ending with
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

### Verified API facts (do not re-derive)

- `syscall::object_wait(handle: u32, signals: Signals, deadline: Instant) -> Result<WaitReturn>`; on deadline it returns `Err(Error::DeadlineExceeded)`; `WaitReturn` has field `pending_signals: Signals`.
- `userspace::time::{Instant, Duration, SystemClock, Clock}`; `Instant = time::Instant<SystemClock>` implements `Copy + Ord` (manual impls in pigweed's time crate), satisfying `TimerManager`'s `T` bound. `SystemClock::now() + Duration::from_millis(n)` works.
- `syscall::debug_trigger_interrupt(irq: u32)`, `syscall::interrupt_ack(handle: u32, signals: Signals)`, `syscall::debug_shutdown(status: pw_status::Result<()>)`.
- Codegen from `system.json5` (via `rust_app`'s `codegen_crate_name = "app_test_timer"`): object `timer_irq` → `handle::TIMER_IRQ`; irq name `test_irq` → `signals::TEST_IRQ`; app constant `test_irq` → `constants::TEST_IRQ`.
- `openprot_orchestrator_timer::{TimerManager, Expired, Full}`; `Expired<Id>` derives `PartialEq`. `openprot_orchestrator_sm::ComponentId` has `const fn new(u8)` and derives `Copy + Eq`.
- App entry pattern: `use userspace::{entry, syscall};` then `#[entry] fn entry() { … }` plus a local `#[panic_handler]` (see `target/ast10x0/tests/mctp/ipc_client/client_main.rs`).

---

### Task 1: Suite scaffold — image builds, harness detects both PASS and FAIL

**Files:**
- Create: `target/ast10x0/tests/timer/user/system.json5`
- Create: `target/ast10x0/tests/timer/user/target.rs`
- Create: `target/ast10x0/tests/timer/user/main.rs` (stub)
- Create: `target/ast10x0/tests/timer/user/BUILD.bazel`

**Interfaces:**
- Consumes: existing pigweed tooling rules; `//target/ast10x0:defs.bzl`, `:entry`, platform, linker template.
- Produces: bazel targets `//target/ast10x0/tests/timer:timer` (image) and `:timer_test` (QEMU test); codegen crate `app_test_timer` with `handle::TIMER_IRQ`, `signals::TEST_IRQ`, `constants::TEST_IRQ`. Task 2–4 replace `main.rs` contents.

- [ ] **Step 1: Write `system.json5`**

```json5
// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

// AST10x0 timer QEMU test: TimerManager <-> object_wait seam.
//
// Single app, single process. The interrupt object (IRQ 44, self-fired via
// debug_trigger_interrupt) is the "event arrived" source for the
// cancel-via-event scenario; deadlines come from TimerManager::next_deadline.
//
// Memory map (AST10x0: 768 KB SRAM, no XIP), same shape as tests/mctp/ipc_client:
//   0x00000000 - 0x00000500  vector table (1280 B)
//   0x00000500 - 0x00020200  kernel flash (~127 KB)
//   then app flash (128 KB)
//   0x00060000 - 0x00080000  kernel RAM (128 KB)
//   then app RAM (32 KB)
{
    arch: {
        type: "armv7m",
        vector_table_start_address: 0x00000000,
        vector_table_size_bytes: 1280,
    },
    kernel: {
        flash_start_address: 0x00000500,
        flash_size_bytes: 129792,
        ram_start_address: 0x00060000,
        ram_size_bytes: 131072,
    },
    apps: [
        {
            name: "test_timer",
            flash_size_bytes: 131072,
            processes: [
                {
                    name: "test_timer_process",
                    ram_size_bytes: 32768,
                    objects: [
                        {
                            name: "timer_irq",
                            type: "interrupt",
                            irqs: [
                                {
                                    name: "test_irq",
                                    number: 44,
                                },
                            ],
                        },
                        {
                            type: "thread",
                            name: "test_timer_thread",
                            kernel_stack_size_bytes: 4096,
                        },],

                },
            ],
            constants: [
                {
                    name: "test_irq",
                    type: "u32",
                    value: 44,
                },
            ],
        },
    ],
}
```

- [ ] **Step 2: Write kernel `target.rs`** (the interrupts-test pattern verbatim, renamed)

```rust
// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]

use console_backend::console_backend_write_all;
use entry as _;
use target_common::{declare_target, TargetInterface};

pub struct Target {}

impl TargetInterface for Target {
    const NAME: &'static str = "AST10x0 Timer Test";

    fn main() -> ! {
        codegen::start();
        #[expect(clippy::empty_loop)]
        loop {}
    }

    fn shutdown(code: u32) -> ! {
        pw_log::info!("Shutting down with code {}", code as u32);
        let sentinel: &[u8] = if code == 0 {
            b"TEST_RESULT:PASS\n"
        } else {
            b"TEST_RESULT:FAIL\n"
        };
        let _ = console_backend_write_all(sentinel);
        #[expect(clippy::empty_loop)]
        loop {}
    }
}

declare_target!(Target);
```

- [ ] **Step 3: Write stub `main.rs` that deliberately FAILS** (proves the harness detects failure before we trust its passes)

```rust
// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Timer QEMU test: TimerManager <-> object_wait seam. Scaffold stub.

#![no_main]
#![no_std]

use pw_status::Error;
use userspace::{entry, syscall};

#[entry]
fn entry() {
    pw_log::info!("timer test: scaffold up");
    let _ = syscall::debug_shutdown(Err(Error::Internal));
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
```

- [ ] **Step 4: Write `BUILD.bazel`**

```python
# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0

load("@pigweed//pw_kernel/tooling:rust_app.bzl", "rust_app")
load("@pigweed//pw_kernel/tooling:system_image.bzl", "system_image", "system_image_test")
load("@pigweed//pw_kernel/tooling:target_codegen.bzl", "target_codegen")
load("@pigweed//pw_kernel/tooling:target_linker_script.bzl", "target_linker_script")
load("@pigweed//pw_kernel/tooling/panic_detector:rust_binary_no_panics_test.bzl", "rust_binary_no_panics_test")
load("@rules_rust//rust:defs.bzl", "rust_binary")
load("//target/ast10x0:defs.bzl", "TARGET_COMPATIBLE_WITH")

filegroup(
    name = "system_config",
    srcs = ["system.json5"],
)

target_codegen(
    name = "codegen",
    arch = "@pigweed//pw_kernel/arch/arm_cortex_m:arch_arm_cortex_m",
    system_config = ":system_config",
    target_compatible_with = TARGET_COMPATIBLE_WITH,
)

target_linker_script(
    name = "linker_script",
    system_config = ":system_config",
    tags = ["kernel"],
    target_compatible_with = TARGET_COMPATIBLE_WITH,
    template = "//target/ast10x0:linker_script_template",
)

rust_binary(
    name = "target",
    srcs = ["target.rs"],
    edition = "2024",
    tags = ["kernel"],
    target_compatible_with = TARGET_COMPATIBLE_WITH,
    deps = [
        ":codegen",
        ":linker_script",
        "//target/ast10x0:entry",
        "@pigweed//pw_kernel/arch/arm_cortex_m:arch_arm_cortex_m",
        "@pigweed//pw_kernel/kernel",
        "@pigweed//pw_kernel/subsys/console:console_backend",
        "@pigweed//pw_kernel/target:target_common",
        "@pigweed//pw_kernel/userspace",
        "@pigweed//pw_log/rust:pw_log",
    ],
)

# Test app: exercises TimerManager against object_wait; calls
# debug_shutdown(Ok|Err) to report the result.
rust_app(
    name = "test_timer",
    srcs = ["main.rs"],
    codegen_crate_name = "app_test_timer",
    edition = "2024",
    system_config = ":system_config",
    tags = ["kernel"],
    target_compatible_with = TARGET_COMPATIBLE_WITH,
    deps = [
        "//services/orchestrator/sm:orchestrator_sm",
        "//services/orchestrator/timer:orchestrator_timer",
        "@pigweed//pw_kernel/userspace",
        "@pigweed//pw_log/rust:pw_log",
        "@pigweed//pw_status/rust:pw_status",
    ],
)

system_image(
    name = "timer",
    apps = [":test_timer"],
    kernel = ":target",
    platform = "//target/ast10x0",
    system_config = ":system_config",
    tags = ["kernel"],
    target_compatible_with = TARGET_COMPATIBLE_WITH,
)

system_image_test(
    name = "timer_test",
    image = ":timer",
    target_compatible_with = TARGET_COMPATIBLE_WITH,
)

rust_binary_no_panics_test(
    name = "no_panics_test",
    binary = ":timer",
    tags = ["kernel"],
)
```

Note: the stub `main.rs` does not yet use the orchestrator deps or the codegen
crate; if the build errors on unused deps, keep them (bazel rust rules don't
error on unused deps by default) — do NOT trim them, tasks 2–4 need them.

- [ ] **Step 5: Build**

Run: `bazel build //target/ast10x0/tests/timer/...`
Expected: success. If codegen rejects the json5 (e.g. schema drift), diff
against `target/ast10x0/tests/mctp/ipc_client/system.json5` — that file is the
schema authority for single-thread apps; the interrupt object block's authority
is `target/ast10x0/tests/interrupts/user/system.json5`.

- [ ] **Step 6: Run under QEMU, expect the harness to catch the deliberate failure**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
Expected: test **FAILS**, log shows `TEST_RESULT:FAIL`. This proves the
sentinel plumbing detects failure — without this step a broken harness that
always passes would go unnoticed.

- [ ] **Step 7: Flip the stub to PASS**

In `main.rs`, replace the `debug_shutdown` line and drop the now-unused import:

```rust
    let _ = syscall::debug_shutdown(Ok(()));
```

and change `use pw_status::Error;` to nothing (delete the line).

- [ ] **Step 8: Run under QEMU, expect PASS**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
Expected: PASS, log shows `TEST_RESULT:PASS`.

- [ ] **Step 9: Verify the no-runner path skips execution**

Run: `bazel test //target/ast10x0/tests/timer/...`
Expected: builds; `timer_test` is skipped or trivially passes without QEMU
(same behavior as the sibling suites — compare with
`bazel test //target/ast10x0/tests/interrupts/...` if unsure).

- [ ] **Step 10: Commit**

```bash
git add target/ast10x0/tests/timer
git commit -m "ast10x0: scaffold timer qemu test suite

Kernel target + single-process test_timer app + interrupt object (IRQ 44).
Stub app passes trivially; harness verified to detect both PASS and FAIL.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Scenario 1 — ordered expiry, boot-before-commit tie-break, one-shot

**Files:**
- Modify: `target/ast10x0/tests/timer/user/main.rs` (full replacement, content below)

**Interfaces:**
- Consumes: `app_test_timer::{handle, signals}` codegen from Task 1;
  `TimerManager`, `Expired` from `openprot_orchestrator_timer`; `ComponentId`
  from `openprot_orchestrator_sm`.
- Produces: `run_test() -> pw_status::Result<()>` dispatcher plus
  `scenario_ordered_expiry() -> pw_status::Result<()>`; `type Tm`, `const C0/C1`,
  and the entry/panic boilerplate that Tasks 3–4 extend (they only ADD functions
  and one call line each).

- [ ] **Step 1: Replace `main.rs` with the scenario-1 implementation**

```rust
// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Timer QEMU test: proves the TimerManager <-> object_wait seam with the
//! production types (userspace Instant, orchestrator-sm ComponentId).
//!
//! The wait loop is the intended orchestrator runtime shape: block in
//! object_wait on {interrupt object, next timer deadline}; DeadlineExceeded
//! drains TimerManager::poll, Ok is the event path.

#![no_main]
#![no_std]

use app_test_timer::{handle, signals};
use openprot_orchestrator_sm::ComponentId;
use openprot_orchestrator_timer::{Expired, TimerManager};
use pw_status::{Error, Result};
use userspace::time::{Clock, Duration, Instant, SystemClock};
use userspace::{entry, syscall};

const C0: ComponentId = ComponentId::new(0);
const C1: ComponentId = ComponentId::new(1);

/// Chain capacity 4 — matches the crate's own unit-test sizing.
type Tm = TimerManager<Instant, ComponentId, 4>;

/// Scenario 1: arm Boot(C0)@+50ms, Boot(C1)@+100ms, Commit@+100ms. Expect
/// expiries in exactly that order (Boot(C1) before Commit exercises the
/// boot-before-commit tie-break at the shared deadline), each at or after its
/// armed offset (lower bound only — no upper bound under QEMU), one-shot.
fn scenario_ordered_expiry() -> Result<()> {
    pw_log::info!("scenario 1: ordered expiry + tie-break + one-shot");
    let mut tm = Tm::new();
    let t0 = SystemClock::now();
    tm.arm_boot(C0, t0 + Duration::from_millis(50))
        .map_err(|_| Error::ResourceExhausted)?;
    tm.arm_boot(C1, t0 + Duration::from_millis(100))
        .map_err(|_| Error::ResourceExhausted)?;
    tm.arm_commit(t0 + Duration::from_millis(100));

    let expected: [(Expired<ComponentId>, Duration); 3] = [
        (Expired::Boot(C0), Duration::from_millis(50)),
        (Expired::Boot(C1), Duration::from_millis(100)),
        (Expired::Commit, Duration::from_millis(100)),
    ];

    let mut idx = 0;
    while idx < expected.len() {
        let deadline = tm.next_deadline().ok_or(Error::Internal)?;
        match syscall::object_wait(handle::TIMER_IRQ, signals::TEST_IRQ, deadline) {
            Err(Error::DeadlineExceeded) => {
                let now = SystemClock::now();
                while let Some(fired) = tm.poll(now) {
                    let (want, offset) = expected[idx];
                    if fired != want {
                        pw_log::error!("scenario 1: wrong expiry at index {}", idx as u32);
                        return Err(Error::Internal);
                    }
                    if now < t0 + offset {
                        pw_log::error!("scenario 1: expiry {} fired early", idx as u32);
                        return Err(Error::Internal);
                    }
                    idx += 1;
                }
            }
            Ok(_) => {
                pw_log::error!("scenario 1: unexpected event wakeup");
                return Err(Error::Internal);
            }
            Err(e) => return Err(e),
        }
    }

    if tm.poll(SystemClock::now()).is_some() {
        pw_log::error!("scenario 1: watchdog fired twice (not one-shot)");
        return Err(Error::Internal);
    }
    if tm.next_deadline().is_some() {
        pw_log::error!("scenario 1: deadline outstanding after full drain");
        return Err(Error::Internal);
    }
    pw_log::info!("scenario 1: PASS");
    Ok(())
}

fn run_test() -> Result<()> {
    scenario_ordered_expiry()?;
    Ok(())
}

#[entry]
fn entry() {
    match run_test() {
        Ok(()) => {
            pw_log::info!("timer test: all scenarios PASSED");
            let _ = syscall::debug_shutdown(Ok(()));
        }
        Err(e) => {
            pw_log::error!("timer test FAILED: {}", e as u32);
            let _ = syscall::debug_shutdown(Err(e));
        }
    }
    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
```

Note on the drain loop: after the first wait (~50 ms) only `Boot(C0)` is due,
so the inner `while let` drains one entry; the second wait's deadline is the
shared +100 ms instant and its drain yields `Boot(C1)` then `Commit` in one
pass — the tie-break assertion rides on `expected`'s ordering.

- [ ] **Step 2: Build**

Run: `bazel build //target/ast10x0/tests/timer/...`
Expected: success. If `Clock` is an unused import warning, keep it only if
`SystemClock::now()` requires the trait in scope (it does — `now` is a trait
method of `time::Clock`).

- [ ] **Step 3: Run under QEMU**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
Expected: PASS; log contains `scenario 1: PASS` and `TEST_RESULT:PASS`.

- [ ] **Step 4: Commit**

```bash
git add target/ast10x0/tests/timer/user/main.rs
git commit -m "ast10x0: timer test scenario 1 - ordered expiry, tie-break, one-shot

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Scenario 2 — re-arm replaces the deadline, never stacks

**Files:**
- Modify: `target/ast10x0/tests/timer/user/main.rs` (add one function; add one line to `run_test`)

**Interfaces:**
- Consumes: `Tm`, `C0`, imports and boilerplate from Task 2 (unchanged).
- Produces: `scenario_rearm_replaces() -> pw_status::Result<()>`.

- [ ] **Step 1: Add the scenario function** (place after `scenario_ordered_expiry`)

```rust
/// Scenario 2: arm Boot(C0)@+30ms then immediately re-arm @+80ms. The 30 ms
/// deadline must be gone: the single wait must run to >= 80 ms and yield
/// exactly one Boot(C0). (If the re-arm stacked, next_deadline would be the
/// 30 ms entry and the elapsed-time check below would fail.)
fn scenario_rearm_replaces() -> Result<()> {
    pw_log::info!("scenario 2: re-arm replaces");
    let mut tm = Tm::new();
    let t0 = SystemClock::now();
    tm.arm_boot(C0, t0 + Duration::from_millis(30))
        .map_err(|_| Error::ResourceExhausted)?;
    tm.arm_boot(C0, t0 + Duration::from_millis(80))
        .map_err(|_| Error::ResourceExhausted)?;

    let deadline = tm.next_deadline().ok_or(Error::Internal)?;
    match syscall::object_wait(handle::TIMER_IRQ, signals::TEST_IRQ, deadline) {
        Err(Error::DeadlineExceeded) => {}
        Ok(_) => {
            pw_log::error!("scenario 2: unexpected event wakeup");
            return Err(Error::Internal);
        }
        Err(e) => return Err(e),
    }

    let now = SystemClock::now();
    if now < t0 + Duration::from_millis(80) {
        pw_log::error!("scenario 2: woke before the replaced 80ms deadline");
        return Err(Error::Internal);
    }
    match tm.poll(now) {
        Some(Expired::Boot(id)) if id == C0 => {}
        _ => {
            pw_log::error!("scenario 2: expected exactly Boot(C0)");
            return Err(Error::Internal);
        }
    }
    if tm.poll(SystemClock::now()).is_some() || tm.next_deadline().is_some() {
        pw_log::error!("scenario 2: stacked entry survived the re-arm");
        return Err(Error::Internal);
    }
    pw_log::info!("scenario 2: PASS");
    Ok(())
}
```

- [ ] **Step 2: Wire it into `run_test`**

```rust
fn run_test() -> Result<()> {
    scenario_ordered_expiry()?;
    scenario_rearm_replaces()?;
    Ok(())
}
```

- [ ] **Step 3: Build and run under QEMU**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
Expected: PASS; log contains `scenario 2: PASS`.

- [ ] **Step 4: Commit**

```bash
git add target/ast10x0/tests/timer/user/main.rs
git commit -m "ast10x0: timer test scenario 2 - re-arm replaces, never stacks

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Scenario 3 — event before deadline cancels the watchdog

**Files:**
- Modify: `target/ast10x0/tests/timer/user/main.rs` (add one function; add one line to `run_test`; extend imports)

**Interfaces:**
- Consumes: `Tm`, `C0`, boilerplate from Task 2; `constants::TEST_IRQ` from the
  `app_test_timer` codegen (extend the existing `use app_test_timer::…` line).
- Produces: `scenario_cancel_via_event() -> pw_status::Result<()>`; the suite is complete.

- [ ] **Step 1: Extend the codegen import** (first line of the `use` block)

```rust
use app_test_timer::{constants, handle, signals};
```

- [ ] **Step 2: Add the scenario function** (place after `scenario_rearm_replaces`)

```rust
/// Scenario 3: arm Boot(C0)@+500ms, then fire the test IRQ. object_wait must
/// return Ok (event beat the deadline); the "component reported in" analogue
/// then cancels the watchdog and nothing is left armed. The trigger is issued
/// before the wait — interrupt objects latch the pending signal, so this does
/// not race.
fn scenario_cancel_via_event() -> Result<()> {
    pw_log::info!("scenario 3: cancel via event");
    let mut tm = Tm::new();
    let t0 = SystemClock::now();
    tm.arm_boot(C0, t0 + Duration::from_millis(500))
        .map_err(|_| Error::ResourceExhausted)?;

    syscall::debug_trigger_interrupt(constants::TEST_IRQ)?;

    let deadline = tm.next_deadline().ok_or(Error::Internal)?;
    let wait = match syscall::object_wait(handle::TIMER_IRQ, signals::TEST_IRQ, deadline) {
        Ok(wait) => wait,
        Err(Error::DeadlineExceeded) => {
            pw_log::error!("scenario 3: deadline fired although the event was pending");
            return Err(Error::Internal);
        }
        Err(e) => return Err(e),
    };
    if !wait.pending_signals.contains(signals::TEST_IRQ) {
        pw_log::error!("scenario 3: woke without TEST_IRQ pending");
        return Err(Error::Internal);
    }
    syscall::interrupt_ack(handle::TIMER_IRQ, signals::TEST_IRQ)?;

    if SystemClock::now() >= t0 + Duration::from_millis(500) {
        pw_log::error!("scenario 3: event did not beat the 500ms deadline");
        return Err(Error::Internal);
    }
    tm.cancel_boot(C0);
    if tm.next_deadline().is_some() {
        pw_log::error!("scenario 3: deadline still armed after cancel");
        return Err(Error::Internal);
    }
    if tm.poll(SystemClock::now()).is_some() {
        pw_log::error!("scenario 3: cancelled watchdog fired");
        return Err(Error::Internal);
    }
    pw_log::info!("scenario 3: PASS");
    Ok(())
}
```

- [ ] **Step 3: Wire it into `run_test`**

```rust
fn run_test() -> Result<()> {
    scenario_ordered_expiry()?;
    scenario_rearm_replaces()?;
    scenario_cancel_via_event()?;
    Ok(())
}
```

- [ ] **Step 4: Build and run under QEMU**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/tests/timer:timer_test --test_output=streamed`
Expected: PASS; log contains all three `scenario N: PASS` lines and
`TEST_RESULT:PASS`.

- [ ] **Step 5: Commit**

```bash
git add target/ast10x0/tests/timer/user/main.rs
git commit -m "ast10x0: timer test scenario 3 - event before deadline cancels watchdog

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Regression sweep

**Files:** none created/modified (verification only; fix-forward if anything breaks).

**Interfaces:**
- Consumes: the complete suite from Tasks 1–4.
- Produces: green tree.

- [ ] **Step 1: Host unit tests still green**

Run: `bazel test //services/orchestrator/timer:orchestrator_timer_test //services/orchestrator/sm:orchestrator_sm_test`
Expected: PASS.

- [ ] **Step 2: Whole ast10x0 tree builds and tests without a runner**

Run: `bazel test //target/ast10x0/...`
Expected: green (firmware executions skipped, `no_panics_test` and builds pass).

- [ ] **Step 3: Whole ast10x0 tree under QEMU**

Run: `bazel test --config=virt_ast10x0 //target/ast10x0/...`
Expected: green including `//target/ast10x0/tests/timer:timer_test` — the new
suite must not perturb the existing ones (IRQ 44 collides with nothing).

- [ ] **Step 4: Commit only if fixes were needed** (otherwise nothing to commit)

If a fix was required, commit it with a message describing the actual fix:

```bash
git add <fixed files>
git commit -m "ast10x0: fix <what> found by timer suite regression sweep

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Self-review (done at plan-writing time)

- **Spec coverage:** goal/loop shape → Tasks 2–4 wait loops; scenario 1/2/3 →
  Tasks 2/3/4; sentinel + skip-without-runner → Task 1 steps 6–9; "still
  green" testing section → Task 5; `Instant: Copy + Ord` risk → resolved
  (verified manual impls in pigweed time crate, recorded in Global
  Constraints); IRQ-44 risk → Task 1 step 5/Task 5 step 3; QEMU granularity
  risk → lower-bound-only constraint.
- **Placeholders:** none; every step carries full file contents or exact diffs
  and exact commands.
- **Type consistency:** `Tm`, `C0`/`C1`, `run_test`, scenario function names,
  and codegen symbol names (`handle::TIMER_IRQ`, `signals::TEST_IRQ`,
  `constants::TEST_IRQ`, crate `app_test_timer`) are identical across tasks.
