# SPI Monitor Boot Use Case Test: Planning

## Overview

Create a Bazel-based test under `target/ast10x0/tests/spim/test_boot_uc/` that validates the SPI monitor boot sequence. This follows the structure and patterns established by existing tests like `smc/`.

## Structure

```
target/ast10x0/tests/spim/test_boot_uc/
├── BUILD.bazel              # Bazel build rules
├── system.json5             # Kernel system configuration
├── target.rs                # Main test binary entry point
├── README.md                # Documentation
└── PLANNING.md              # This file
```

## Dependencies

- `//target/ast10x0/peripherals:spimonitor` — SPI monitor peripheral crate
- `//target/ast10x0:entry` — Cortex-M entry point
- `@pigweed//pw_kernel/*` — Pigweed kernel support
- `@pigweed//pw_log/rust:pw_log` — Logging framework

## Build Rules (in BUILD.bazel)

Following the SMC test pattern:

1. **`target_linker_script`** — Generate linker script from system config
2. **`rust_binary`** (name: `target`) — Compile test binary
3. **`system_image`** (name: `test_boot_uc`) — Package kernel + test
4. **`system_image_test`** (name: `test_boot_uc_test`) — QEMU test harness
5. **`rust_binary_no_panics_test`** (name: `no_panics_test`) — Panic detection

## Test Coverage (target.rs)

Phase-by-phase validation:

1. **Phase 1: Hold**
   - Initialize monitor controller
   - Call `bmc_boot_hold()` (once peripheral traits exist)
   - Verify mux is set to ROT
   - Verify flash reset issued

2. **Phase 2: Policy Configuration**
   - Load PFM (or hardcoded) policy data
   - Call `configure_monitor_policy()`
   - Verify address regions loaded
   - Verify no region overlap

3. **Phase 3: Release**
   - Call `bmc_boot_release()`
   - Verify mux is set to HOST
   - Verify soft-reset completed

4. **Runtime Verification**
   - Read monitor status
   - Verify enforcement active
   - Check for spurious violations

## System Configuration (system.json5)

Minimal kernel config with logging support for QEMU.

## Build Commands

```bash
# Build only
bazelisk build --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc

# Run under QEMU
bazelisk test --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc_test

# Streamed output
bazelisk test --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc_test --test_output=streamed
```

## Prerequisites

Before implementing this test, the following must exist or be stubbed:

1. **Peripheral Crate**: `//target/ast10x0/peripherals:spimonitor` with:
   - `Monitor` trait (abstract interface)
   - `MonitorInstance` enum
   - `MuxSelect` enum
   - Boot functions: `bmc_boot_hold()`, `configure_monitor_policy()`, `bmc_boot_release()`

2. **PAC Crate**: `ast10x0-pac` with SPI monitor register definitions (SPIPF1-4 blocks)

3. **Error Types**: `BootError` and `Result<T>` types

4. **Test Execution**: `--config=virt_ast10x0` Bazel configuration for QEMU AST1030 emulation

## Phased Implementation

### Phase A: Stub and Build Infrastructure ✓
- [x] Create `BUILD.bazel` with all rules
- [x] Create minimal `system.json5`
- [x] Create `target.rs` with scaffold (prints messages, no real hardware calls)
- [ ] Verify builds under `--config=virt_ast10x0`

### Phase B: Peripheral Crate Traits ✓ (Partial)
- [x] Define `Monitor` trait in peripheral crate
- [x] Define `MonitorInstance`, `MuxSelect`, `MonitorStatus` enums/structs
- [x] Define boot-level types: `BootPhase`, `BootError`, `BootConfig`, `BootResult`
- [ ] Define boot functions (stubs returning Ok): `bmc_boot_hold()`, `configure_monitor_policy()`, etc.
- [x] Add to `peripherals/lib.rs` exports

### Phase C: Monitor Implementation via PAC ✓ (Complete)
- [x] Create `ast1060_monitor.rs` with `Ast1060Monitor` struct
- [x] Implement `Monitor` trait using PAC register bindings
- [x] Use `ast10x0-pac` to read/write monitor MMIO registers (SpiMonitorRegisters)
- [x] Handle register effects (mux routing, status flags, policy lock)
- [x] Placeholder implementations with TODO comments for datasheet validation
- [x] Add comprehensive unit tests for encoding/extraction functions
- [x] Export `PacMonitor` from module

### Phase D: Test Implementation
- [ ] Implement Phase 1 test (hold)
- [ ] Implement Phase 2 test (configure policy)
- [ ] Implement Phase 3 test (release)
- [ ] Implement runtime verification

### Phase E: Validation
- [ ] Add state assertions
- [ ] Add error path testing
- [ ] Run under QEMU successfully
- [ ] Verify panic detection passes

## Expected Test Output (successful run)

```
[INFO] === SPI Monitor Boot Use Case Test ===
[INFO] [Phase 1] Boot Hold
[INFO]   - Initialize monitor controller
[INFO]   - Switch mux to ROT
[INFO]   - Reset flash
[INFO]   ✓ Hold phase complete (stub)
[INFO] [Phase 2] Configure Policy
[INFO]   - Read PFM metadata
[INFO]   - Extract region definitions
[INFO]   - Load address privilege regions
[INFO]   - Verify region overlap
[INFO]   ✓ Policy configuration complete (stub)
[INFO] [Phase 3] Release
[INFO]   - Switch mux to HOST
[INFO]   - Soft reset monitor
[INFO]   - Verify release completed
[INFO]   ✓ Release phase complete (stub)
[INFO] [Phase 4] Runtime Verification
[INFO]   - Read monitor status
[INFO]   - Verify enforcement active
[INFO]   - Check for spurious violations
[INFO]   ✓ Runtime verification complete (stub)
[INFO] === All test phases passed! ===
[SUCCESS] Boot use case test passed
```

## Key Distinctions

**QEMU vs PAC**:
- **QEMU** (`virt_ast10x0`) is the execution environment—the hardware emulator that runs the test code
- **PAC** (`ast10x0-pac`) is the register access layer—provides typed Rust bindings to MMIO registers
- They are **complementary**, not alternatives. PAC is used on both real hardware and QEMU.

## Differences from Planning Docs

This test differs from the conceptual documents in that it:
- Uses actual Bazel build infrastructure
- Integrates with Pigweed kernel instead of bare cortex-m
- Executes under `virt_ast10x0` QEMU emulation (not on real hardware)
- Uses peripheral crate exports (not standalone library)
- Runs as part of kernel system image, not standalone binary

## Next Steps

1. Confirm this structure aligns with project expectations
2. Identify where SPI monitor peripheral crate should live (alongside other peripherals in `target/ast10x0/peripherals/`)
3. Confirm PAC (`ast10x0-pac`) has SPI monitor register definitions, or create them
4. Create Phase A scaffolding (BUILD.bazel, system.json5, target.rs skeleton)
