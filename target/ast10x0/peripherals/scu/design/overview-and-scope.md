# AST10x0 SCU: Overview and Scope

## Purpose

The AST10x0 System Control Unit (SCU) is the platform control plane for SoC-
wide settings that do not belong to a single data-path peripheral. It owns
shared state such as reset control, clock gating or divider selection, and mux
or routing configuration that determines how other peripherals are connected.

In the `target/ast10x0/peripherals` crate, the SCU module should exist as a
small, auditable register-first library for programming that shared control
plane. It should not become a catch-all HAL.

## Why A Dedicated SCU Module Belongs Here

### 1. Shared hardware state needs one clear home

Several AST10x0 peripherals depend on SCU-owned configuration before their own
local register programming is meaningful:

- clock enable and divider setup
- reset assert/deassert sequencing
- pin or function mux selection
- SPI monitor passthrough and routing selection
- controller-to-monitor detour paths and related glue logic

If each peripheral reaches directly into raw SCU registers, the same shared
state gets encoded multiple times with slightly different assumptions.

### 2. SPI monitor parity will require SCU-backed helpers

The current `spimonitor` scaffold in this repository intentionally focuses on
SPIPF register access. That is correct for the first layer, but parity with the
existing AST1060 implementation will require SCU interactions for:

- passthrough enable and mode selection
- internal SPI master detour selection
- external mux selection
- monitor-related multi-function pin adjustments

Those controls belong in a shared SCU access layer instead of being duplicated
inside `spimonitor` or `smc`.

### 3. Minimal-TCB still needs a shared control plane

This repository's direction is to keep policy and trusted hardware sequencing
explicit and reviewable. A dedicated SCU module supports that goal by keeping
the unsafe register perimeter small while providing typed operations for the
few SoC-wide controls that trusted code must own.

## What The SCU Module Should Be

The SCU module should provide:

- a thin PAC-backed register wrapper with a single unsafe constructor boundary
- typed helpers for small, well-scoped shared control domains
- pure data enums and errors for reset, clock, mux, and routing choices
- narrowly targeted controller helpers where sequencing matters

The SCU module should not provide:

- a board-support package
- peripheral-local configuration that is better expressed in the peripheral
  module itself
- a giant façade over every SCU register in one unstructured API
- policy logic that belongs in a higher-level bring-up or security module

## Responsibilities And Boundaries

### Responsibilities that fit SCU

- reset control for shared or cross-cutting peripherals
- clock stop / ungate operations
- clock source and divider selection when consumed across modules
- mux and routing controls that connect one peripheral block to another
- SoC-wide strap, revision, or status reads used by multiple modules

### Responsibilities that do not fit SCU

- SMC command sequencing and flash protocol behavior
- SPI monitor command allow-list or address filter programming
- UART FIFO, DMA, or line configuration
- per-driver policy objects for runtime privilege decisions

The dividing line is simple: if the register controls SoC-wide plumbing or a
shared precondition for multiple peripherals, it belongs in SCU. If it controls
the operation of one block after routing is already established, it belongs in
that block's module.

## Relationship To Other Peripheral Modules

### `spimonitor/`

`spimonitor` should continue to own SPIPF register semantics, policy tables,
and lifecycle state. SCU should only expose the shared routing and mux knobs
that determine how a monitor instance is connected.

### `smc/`

`smc` should own flash controller configuration, transfer behavior, and memory
window semantics. SCU may provide prerequisite clock, reset, or route setup
that `smc` consumes during trusted bring-up.


## Recommended Initial API Shape

The first SCU API should stay deliberately small and layered.

1. Register layer
- raw PAC-backed accessors for the SCU register block
- narrowly named helpers around the specific registers we plan to support

2. Types layer
- enums for reset domains, clock domains, divider choices, mux selections, and
  SPI-monitor-related route choices
- explicit error types for unsupported combinations or invalid instances

3. Control layer
- typed helpers for:
  - assert/deassert reset
  - gate/ungate clocks
  - set or read divider selections
  - configure SPI monitor passthrough or detour routing
  - read SoC identification or status values used in bring-up

This keeps SCU focused on shared control-plane operations without embedding
board policy into the module.

## Proposed Module Tree

```text
target/ast10x0/peripherals/scu/
  mod.rs
  registers.rs
  types.rs
  reset.rs
  clock.rs
  routing.rs
  status.rs
  design/
    overview-and-scope.md
    implementation-plan.md
```

## File Responsibilities

- `mod.rs`
  - public surface and curated re-exports
  - prevents consumers from depending on internal file layout

- `registers.rs`
  - PAC-backed register wrapper
  - one unsafe perimeter for SCU register block ownership
  - raw read/modify/write helpers for targeted SCU registers

- `types.rs`
  - enums and errors shared by the rest of the module
  - examples: reset targets, clock domains, divider values, mux selectors,
    route IDs

- `reset.rs`
  - reset-related helpers and typed sequencing operations
  - assert, deassert, and status readback where relevant

- `clock.rs`
  - clock gate, source, and divider helpers used across peripherals

- `routing.rs`
  - shared mux and route controls
  - first likely consumer is SPI monitor and SPI controller connectivity

- `status.rs`
  - read-only SCU state useful across modules, such as revision or strap data

## Concrete Phase-1 Targets

The first implementation should only cover controls already justified by nearby
users or likely immediate consumers:

- reset control used by bring-up sequences
- clock gating or divider reads already needed by existing peripherals
- SPI monitor passthrough and routing controls needed for future parity work
- SoC revision or strap reads if they are already consumed in trusted code

Anything beyond that should wait until a concrete caller exists.

## Design Rules

1. Keep the unsafe boundary in `registers.rs` only.
2. Prefer typed enums over raw bit masks in public APIs.
3. Avoid generic "write arbitrary SCU register" escape hatches.
4. Add helpers only when there is a nearby consumer or verified hardware need.
5. Preserve the boundary between shared SoC plumbing and peripheral-local
   behavior.

## Open Questions

- Which SCU register groups are stable enough to expose first without dragging
  in unrelated platform policy?
- Should reset and clock helpers be stateless free functions over a shared SCU
  handle, or small typed controller wrappers by domain?
- How much SPI monitor routing should live in SCU versus thin orchestration in
  `spimonitor`?

The default bias should be to start small and grow only from demonstrated
callers.