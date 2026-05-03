# AST10x0 SPI Monitor: Overview and Usage Model

## Purpose

The AST10x0 SPI monitor blocks (SPIPF1..SPIPF4) are policy and observability
peripherals that sit on SPI paths and enforce command/address constraints while
collecting transaction context. They are not flash controllers themselves. The
SMC/FMC/SPI controllers move data; SPI monitor blocks gate and inspect the
traffic.

In this crate, the SPI monitor module is intended to provide a small, auditable,
register-first API for:

- command allow-list enforcement
- read/write address privilege filtering
- monitor routing and passthrough configuration
- lock-down of policy registers after setup

## What The Monitors Are

On AST10x0, each monitor instance has an independent register block:

- SPIPF1 at 0x7E79_1000 (SPIM0)
- SPIPF2 at 0x7E79_2000 (SPIM1)
- SPIPF3 at 0x7E79_3000 (SPIM2)
- SPIPF4 at 0x7E79_4000 (SPIM3)

These blocks typically expose:

- global monitor control bits
- command table entries (allow / lock / valid-once behavior)
- address filter/privilege tables for read and write paths
- register write-lock bits for tamper resistance after configuration

The monitor should be treated as a policy firewall for SPI transactions, not a
transport endpoint.

## Why We Need Them

### 1) Platform Security Policy Enforcement

Firmware often needs tighter control than "all SPI opcodes are legal."
Examples:

- forbid erase/program opcodes in runtime regions
- allow read-only opcodes for immutable code partitions
- permit updates only in bounded windows during authenticated update flow

Without SPI monitor policy, software bugs in upper layers can accidentally
issue destructive commands.

### 2) Least-Privilege Runtime Behavior

A monitor enables enforcing different policies across lifecycle states:

- boot: broader command set for provisioning/bring-up
- runtime: reduced command set and locked policy
- recovery/update: temporarily expanded permissions under strict checks

This supports a minimal-TCB posture by making policy hardware-enforced rather
than purely convention-based.

### 3) Operational Safety and Debugging

Even when full blocking is not enabled, monitor routing and status are valuable
for diagnostics:

- prove policy was programmed before handoff
- verify lock bits are asserted
- trace command-table state during test and failure triage

## Usage Model In This Crate

The intended model mirrors other peripheral modules in this repository:

1. Register Layer (current starting point)
- thin, typed wrapper around PAC register blocks
- single unsafe perimeter for pointer ownership
- safe raw read/modify/write helpers by register intent

2. Type/Policy Layer
- enums/bitflags for command slot state, privilege mode, lock policy,
  passthrough/routing mode
- explicit error enums for invalid slot/index/state transitions

3. Controller/Workflow Layer
- lifecycle APIs that enforce legal sequencing, for example:
  configure -> validate -> lock -> operational
- helper methods for common policy profiles (read-only runtime,
  update-window-enabled, manufacturing-open)

4. Integration Layer
- wiring from SMC-facing code paths or platform bring-up code
- optional test helpers for host/QEMU where monitor behavior is partially
  modeled

## Recommended Initial API Shape

- instance selection by enum: Spim0..Spim3
- create accessor from PAC pointer via instance mapping
- command table operations:
  - find slot
  - add/replace command
  - lock command or table
- address privilege operations:
  - configure read/write blocked regions
  - lock privilege tables
- finalization:
  - lock global control and table writes
  - expose readback verification methods

## Constraints and Reality Checks

- QEMU coverage for SPI monitor functionality may be incomplete; policy logic
  must still be tested via unit/host tests and silicon validation.
- Register lock semantics are one-way in many flows; APIs should make this
  explicit and difficult to misuse.
- This module should stay focused on monitor policy. Flash transport and DMA
  remain in SMC/SPI controller modules.

## Relationship To Zephyr-Style Systems

In monolithic-kernel designs, one driver may configure all monitors and all
controllers in one address space. In this repository's direction, monitor
configuration is still centralized in trusted code, but runtime use should
preserve minimal-TCB boundaries by avoiding unnecessary cross-controller shared
state.

The monitor crate therefore acts as a reusable hardware-policy component with a
clear separation from flash data-plane logic.

## Proposed Module Tree

The SPI monitor module should follow the same layering pattern used by `smc/`.

```text
target/ast10x0/peripherals/spimonitor/
  mod.rs
  registers.rs
  types.rs
  controller.rs
  policy.rs
  profile.rs
  planning/
    overview-and-usage-model.md
    implementation-plan.md
```

### File Responsibilities

- `mod.rs`
  - public module surface and curated re-exports
  - keeps consumers from depending on internal file boundaries

- `registers.rs`
  - low-level PAC-backed register wrapper
  - single unsafe perimeter for register block ownership
  - raw read/write/modify methods by register intent

- `types.rs`
  - all public enums/bitflags/errors for monitor semantics
  - examples: monitor instance id, allow-command flags, privilege direction,
    lock status, policy validation errors

- `controller.rs`
  - stateful typed facade over `registers.rs`
  - sequencing guardrails: init -> configure -> lock -> operational
  - table manipulation helpers with bounds/lock checks

- `policy.rs`
  - pure data policy model and validators (no MMIO)
  - enables host-side testing of policy logic independent of hardware

- `profile.rs`
  - predefined policy bundles for common use cases
  - examples: `runtime_read_only`, `firmware_update_window`,
    `manufacturing_open`

- `planning/implementation-plan.md`
  - phased bring-up checklist and test matrix

## Usage Model By Layer

1. Policy construction (`policy.rs`, `profile.rs`)
- Build or select a policy profile in trusted setup code.

2. Hardware apply (`controller.rs` + `registers.rs`)
- Translate policy into command table and address filter entries.
- Read back and verify programmed state.

3. Policy lock
- Assert write-disable bits for command/address/control registers.
- Expose read-only status methods for runtime attestation.

4. Runtime operation
- Monitor remains active as hardware guardrail.
- No dynamic policy edits in steady state.

## Phased Implementation Plan

### Phase 1: Register and Type Foundation

- complete `registers.rs` coverage for currently needed SPIPF registers
- add `types.rs` enums/errors/flags
- add host tests for bitfield conversions and table-index bounds

### Phase 2: Controller Semantics

- add `controller.rs` with typed lifecycle and safe mutation APIs
- support allow-command table operations (add/remove/find/lock)
- support address privilege table operations (read/write direction)

### Phase 3: Policy and Profiles

- add `policy.rs` declarative policy structs
- add `profile.rs` built-in profiles for boot/runtime/update modes
- add policy validator tests (overlap, out-of-range, illegal transitions)

### Phase 4: Integration and Verification

- wire monitor setup into trusted platform bring-up path
- add QEMU smoke tests for register programming/readback
- define silicon-only validation checklist for lock permanence and routing
