# I2C Integration Test (AST10x0)

## Overview

This directory contains a system integration test for the I2C driver on
the AST10x0 platform. It validates that the I2C server and client
processes start, the IPC channel binds, the platform's pre-kernel SCU
init runs, and the controller comes up — under a multi-process QEMU
simulation.

## Test Structure

### Components

- **`target.rs`**: Kernel entry point for the AST10x0 virt target.
- **`server_main.rs`**: I2C server process — wires the wait_group with
  the IPC channel and IRQ, supplies `wait_for_i2c_irq` as the
  peripheral's yield closure, and hands over to `runtime::run`.
- **`client_main.rs`**: Minimal I2C client process — binds the IPC
  channel handle and shuts down. No I2C transactions are issued because
  QEMU's bare model doesn't have a slave attached.
- **`system.json5`**: System configuration — memory layout, IPC
  channel, MMIO mapping, IRQ object.
- **`BUILD.bazel`**: Bazel build rules and test targets.

### Memory Layout

```
ARM Cortex-M4 @ 200 MHz, 768KB SRAM (0x00000000 - 0x000BFFFF)

0x00000000 - 0x00000800: Vector table (2 KB — sized for IRQ 110+)
0x00000800 - 0x00020000: Kernel code
0x00020000 - 0x00060000: I2C app flash (256KB, server + client)
0x00060000 - 0x00080000: Kernel RAM (128KB)
0x00080000 - 0x000A0000: App RAM (128KB)
```

The vector table is bigger than the usart image's because the I2C bus
0 NVIC line (110) is much higher than UART5's (8); the table needs
slots through the highest IRQ used.

### Process Definition

1. **I2C Server** (`i2c_server_bin`):
   - Owns I2C bus 0: MMIO `0x7e7b0000`, IRQ 110.
   - Runs `Backend::new(BUS_ID, I2cConfig::default(), wait_for_i2c_irq)`.
   - Enters the dispatch loop in `runtime::run`.
   - Depends on `//drivers/i2c/server:i2c_server` and
     `//target/ast10x0/backend/i2c:i2c_backend_ast10x0`.

2. **I2C Client** (`i2c_client_app`):
   - Constructs `IpcI2cClient` and shuts down.
   - Depends on `//drivers/i2c/client:i2c_client`.

## Pre-Kernel Platform Init

I2C has SCU-touching init that must happen **before** any userspace
process runs, because the SCU lives outside any process's MPU
mappings. [`target/ast10x0/entry.rs`](../../entry.rs) handles it:

```rust
init_i2c_global();                            // SCU reset + I2CG0C/I2CG10
Pinctrl::apply_pinctrl_group(PINCTRL_I2C0);   // SCU4xx pin mux for bus 0
kernel::main(Arch, &mut INIT_STATE);
```

Add one `apply_pinctrl_group(PINCTRL_I2C<n>)` line per bus the board
actually uses. Other I2C images on this target reuse this entry.

## AST10x0 Backend

The reference backend at
[`//target/ast10x0/backend/i2c`](../../backend/i2c) holds **one**
`Ast1060I2c<'static, fn(u32)>` for the bus declared in `system.json5`.
Wire-level `bus: u8` is validated against the configured `bus_id`;
mismatched requests get `ResponseCode::InvalidBus`. Single-bus per
image is adequate for MCTP-over-I2C; the trait stays multi-bus-shaped
so a future multi-bus backend slots in unchanged. See
[`drivers/i2c/MIGRATION_PLAN.md` §5](../../../../drivers/i2c/MIGRATION_PLAN.md#5-single-bus-wire-multi-bus-ready).

## Yield Closure (`wait_for_i2c_irq`)

`server_main.rs` defines:

```rust
fn wait_for_i2c_irq(_ns: u32) {
    let _ = syscall::object_wait(handle::WG, signals::I2C, Instant::MAX);
    let _ = syscall::interrupt_ack(handle::I2C_IRQ, signals::I2C);
}
```

Passed to `Backend::new` as the peripheral driver's yield. When the
peripheral's `wait_completion` polling loop calls the yield, the task
is descheduled until the I2C IRQ fires. Net behavior: between issuing
an I2C op and its completion, the server task is fully passive — the
kernel runs other tasks.

See [`drivers/i2c/MIGRATION_PLAN.md` §7](../../../../drivers/i2c/MIGRATION_PLAN.md#7-yield-closure-decision)
for the full design discussion (busy-loop vs IRQ-wait vs WFE).

## Running the Test

### Prerequisites

- `bazelisk` installed and in PATH.
- A `qemu-system-arm` build that supports `ast1030-evb` (the vendored
  `qemu-ast10x0-i2c` works).
- Working Rust/Bazel build environment.

### Run the Test

```bash
# Boot the system image under QEMU
bazelisk test //target/ast10x0/tests/i2c:i2c_test --config=virt_ast10x0

# With verbose output
bazelisk test //target/ast10x0/tests/i2c:i2c_test --config=virt_ast10x0 -s
```

### Expected Outcome

QEMU exits with status 0 — server initialized cleanly, client bound
its IPC handle, and `debug_shutdown(Ok(()))` propagated through the
kernel.

## Test Flow

1. **Pre-kernel init** (`entry.rs`): SCU global I2C registers + bus 0
   pin mux applied.
2. **Kernel boots**: scheduler, MPU, processes set up.
3. **Server starts**:
   - `wait_group_add` for the IPC channel and the I2C IRQ.
   - `Backend::new(0, I2cConfig::default(), wait_for_i2c_irq)` runs
     `Ast1060I2c::new` (controller reset, timing, IER).
   - `runtime::run` blocks on `object_wait`.
4. **Client starts**:
   - `IpcI2cClient::new(handle::I2C)` binds the channel handle.
   - Logs and calls `debug_shutdown(Ok(()))`.
5. **Test completion**: kernel's shutdown path invokes
   `cortex_m_semihosting::debug::exit(EXIT_SUCCESS)`; QEMU exits 0.

## What's NOT Exercised Yet

- Real I2C transactions (probe/write/read). QEMU's `aspeed_i2c` model
  is in place but the smoke test doesn't attach a slave device. Adding
  `-device tmp105,bus=…,address=0x50` (or similar) plus a richer
  `client_main.rs` is the natural next step.
- The IRQ → drain → `Signals::USER` notification path.
  Software-triggering via `syscall::debug_trigger_interrupt(110)` works
  to validate the wiring; a real listener-task companion app that
  `object_wait`s for `USER` and then issues `SlaveReceive` would close
  the loop.

## Build Artifacts

```
bazel-bin/target/ast10x0/tests/i2c/
├── i2c                    # System image (kernel + apps)
├── i2c_test               # Test runner
├── i2c_server_bin         # Compiled server binary
└── i2c_client_app         # Compiled client binary
```

## Related Documentation

- [I2C Driver Model](../../../../drivers/i2c/README.md) — protocol +
  trait + dispatch + runtime (generic).
- [I2C Migration Plan](../../../../drivers/i2c/MIGRATION_PLAN.md) —
  design tradeoffs, single-bus rationale, IRQ topology, yield closure
  discussion.
- [`target/ast10x0/entry.rs`](../../entry.rs) — pre-kernel platform
  init.
- [`target/ast10x0/backend/i2c`](../../backend/i2c) — AST10x0 backend
  implementation.

## License

Licensed under the Apache-2.0 license. See LICENSE file in repository
root.
