# AST10x0 SPI Monitor Boot Use Case Test

This package provides a test harness for validating the SPI monitor boot sequence on AST10x0 under the `virt_ast10x0` QEMU configuration.

## Overview

The SPI monitor boot sequence consists of four phases:

1. **Hold Phase** — Initialize monitor controller, switch mux to ROT, reset flash
2. **Policy Configuration** — Load address privilege regions from provisioned data
3. **Release Phase** — Switch mux back to host, soft-reset monitor, enable enforcement
4. **Runtime Verification** — Confirm monitor enforcement is active

## Current Coverage

**Phase A (Scaffold)** - Infrastructure and placeholder tests:
- Bazel build infrastructure (BUILD.bazel, system.json5)
- Minimal test binary with phase logging
- QEMU integration via system_image_test
- Panic detection (no_panics_test)

## Build and Run

### Build only

```console
bazelisk build --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc
```

### Run under QEMU

```console
bazelisk test --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc_test
```

### Run with streamed output (shows kernel boot log)

```console
bazelisk test --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:test_boot_uc_test \
  --test_output=streamed
```

### Panic detection test

```console
bazelisk test --config=virt_ast10x0 //target/ast10x0/tests/spim/test_boot_uc:no_panics_test
```

## Logging

This test uses **Pigweed's pw_log** framework for structured logging output. Log messages are emitted via the kernel's console backend and appear in the QEMU test output.

Logging macros available in the test:
- `pw_log!()` — General logging (info level by default)
- `defmt!()` — Alternative binary format logging via defmt

All phase transitions and key operations are logged for easy verification during test execution.

## SPI Monitor Policy

The test incorporates SPI NOR flash command filtering and address privilege configuration from the aspeed-rust reference implementation:

### Allowed Commands
The monitor is configured to allow 27 standard SPI NOR flash opcodes:
- **Read operations**: `0x03` (Read), `0x0b` (Fast Read), `0x6b`/`0x6c` (Fast Read with modes)
- **Status operations**: `0x01` (Write Status), `0x05` (Read Status), `0x35` (Read Status 2)
- **Erase operations**: `0x20` (Sector), `0xd8` (Block), `0xb7`/`0xe9` (Suspend/Resume)
- **Program operations**: `0x02` (Program 1B), `0x32` (Program 256B)
- **Utility operations**: `0x06` (Write Enable), `0x04` (Write Disable), `0x9f` (Read JEDEC ID)

### Address Privilege Regions
Configured regions (when PFM loader is ready):
- **Read-blocked**: PFM metadata region (0x0300_0000 - 0x0304_0000)
- **Write-blocked**: BMC firmware region (0x0000_0000 - 0x0020_0000)

See [aspeed-rust/tests-hw/src/spim_test.rs](../../../aspeed-rust/tests-hw/src/spim_test.rs) for command table patterns.

## Expected Output (Successful Run)

```
[INFO] === SPI Monitor Boot Use Case Test ===
[INFO] [Phase 1] Boot Hold
[INFO]   - Initialize monitor controller
[INFO]   - Switch mux to ROT
[INFO]   - Reset flash
[INFO]   ✓ Hold phase complete (stub)
[INFO] [Phase 2] Configure Policy
[INFO]   - Allowed SPI Commands: 27 opcodes configured
[INFO]     Commands: Read (0x03), Fast Read (0x0b), Sector Erase (0x20), etc.
[INFO]   - Address Privilege Regions: (stub - awaiting PFM loader)
[INFO]     Would configure read-blocked and write-blocked regions
[INFO]     Current region count: 0
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

## Implementation Phases

### Phase A: Stub and Build Infrastructure ✓
- [x] CREATE BUILD.bazel with all rules
- [x] Create minimal system.json5
- [x] Create target.rs with scaffold
- [ ] Verify builds under --config=virt_ast10x0

### Phase B: Peripheral Crate Traits
- [ ] Define Monitor trait in //target/ast10x0/peripherals:spimonitor
- [ ] Define MonitorInstance and MuxSelect enums
- [ ] Define boot functions
- [ ] Add to peripherals/lib.rs exports

### Phase C: Monitor Implementation via PAC
- [ ] Implement Monitor trait using ast10x0-pac
- [ ] Read/write monitor MMIO registers
- [ ] Handle register effects
- [ ] Test code works on QEMU and real hardware

### Phase D: Test Implementation
- [ ] Implement Phase 1 test (hold)
- [ ] Implement Phase 2 test (policy configuration)
- [ ] Implement Phase 3 test (release)
- [ ] Implement Phase 4 test (runtime verification)

### Phase E: Validation
- [ ] Add state assertions
- [ ] Add error path testing
- [ ] Run under QEMU successfully
- [ ] Verify panic detection passes

## Key Distinctions

**QEMU vs PAC**:
- **QEMU** (`virt_ast10x0`) is the execution environment—the hardware emulator
- **PAC** (`ast10x0-pac`) is the register access layer—typed MMIO bindings
- They are complementary: PAC provides register bindings, QEMU provides the emulated hardware

## Related Documentation

- [Planning Document](PLANNING.md) — Design and phased implementation plan
- [Boot Sequence Usage Model](../../../peripherals/spimonitor/planning/boot-sequence-usage-model.md) — Detailed boot flow
- [Boot Implementation Guide](../../../peripherals/spimonitor/planning/boot.md) — Pseudo-code and patterns
- [Overview and Usage Model](../../../peripherals/spimonitor/planning/overview-and-usage-model.md) — Architecture overview

## References

Reference test structure from:
- [AST10x0 SMC Test](../../smc/README.md)

See aspeed-zephyr-project source for actual boot sequence:
- Hold/release: `lib/hrot_hal/gpio/gpio_aspeed.c#L85`
- Policy config: `apps/aspeed-pfr/src/intel_pfr/intel_pfr_spi_filtering.c#L32`
