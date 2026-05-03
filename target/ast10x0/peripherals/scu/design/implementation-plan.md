# AST10x0 SCU Implementation Plan

## Goal

Introduce a small SCU module that centralizes shared AST10x0 control-plane
operations without collapsing unrelated peripheral behavior into one large API.

## Phase 1: Register And Type Foundation

- add `registers.rs` with a PAC-backed SCU wrapper and one unsafe constructor
  boundary
- add `types.rs` with the minimal enums and errors needed for initial reset,
  clock, and routing support
- keep public APIs limited to the first concrete users

Exit criteria:

- no public API requires callers to pass raw SCU register addresses
- raw MMIO stays contained to the SCU register layer

## Phase 2: Reset And Clock Helpers

- add `reset.rs` helpers for assert/deassert sequencing and status readback
- add `clock.rs` helpers for gate, ungate, source select, and divider read or
  write where justified
- keep domain coverage intentionally partial until more callers exist

Exit criteria:

- trusted bring-up code can perform common reset and clock steps without open-
  coding SCU bit manipulation

## Phase 3: Routing And SPI Monitor Integration

- add `routing.rs` for shared mux and path-selection controls
- expose the SCU operations required to bring `spimonitor` closer to AST1060
  parity:
  - passthrough enable
  - internal SPI master detour selection
  - external mux selection
  - monitor-related multi-function pin adjustments where they are clearly SCU-
    owned
- keep SPIPF policy table programming in `spimonitor`

Exit criteria:

- `spimonitor` can rely on SCU helpers for shared route state instead of
  reaching into raw SCU registers directly

## Phase 4: Status And Shared Bring-Up Reads

- add `status.rs` for read-only SoC-wide information that multiple modules may
  consume
- start with revision or strap information only when a real caller exists

Exit criteria:

- shared read-only SCU state no longer needs ad-hoc raw PAC access in each
  consumer

## Testing Strategy

- unit tests for enum-to-bitfield translations where the code is pure
- host tests for bounds checks and invalid routing combinations where possible
- integration coverage through the first consuming modules rather than a large
  standalone SCU test surface

## Non-Goals

- full coverage of every SCU register at module bring-up time
- board policy orchestration inside the SCU module
- duplicating peripheral-local abstractions already owned by `smc`,
  `spimonitor`, or `uart`

## Decision Rule For Growth

Add new SCU helpers only when at least one of the following is true:

- a nearby peripheral module needs the same SCU field in multiple places
- a trusted bring-up sequence becomes hard to audit without a typed helper
- future parity work requires the same SCU operation across more than one
  caller

If none of those conditions hold, leave the behavior out of the initial SCU
surface.