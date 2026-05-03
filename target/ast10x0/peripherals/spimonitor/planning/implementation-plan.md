# SPI Monitor Implementation Plan

## Lifecycle Justification

The SPI monitor API is intentionally lifecycle-driven:

- `Uninitialized` -> hardware not yet programmed
- `Configured` -> policy tables and control fields staged and validated
- `Locked` -> write-disable bits asserted; runtime policy is immutable

This is not only an API preference. It is required by hardware behavior and
minimal-TCB goals.

### Why explicit lifecycle states are required

1. One-way hardware lock semantics
- SPIPF lock bits are effectively write-once for the boot/runtime session.
- If code can call lock-related operations out of order, policy may become
  partially programmed and permanently stuck.

2. Prevent unsafe intermediate exposure
- Programming command/region tables takes multiple writes.
- Without a staged `Configured` phase and explicit validation, a task could
  start using the monitor while rules are incomplete.

3. Enforce least-privilege steady state
- The expected runtime posture is read-mostly, policy-stable operation.
- Requiring a transition to `Locked` formalizes that post-boot mutable control
  is not part of normal operation.

4. Constrain bug blast radius in trusted code
- A typed lifecycle blocks classes of misuse at compile-time and runtime:
  - lock before tables are written
  - reprogram after lock
  - skipping readback checks

5. Improve auditability and attestation
- Lifecycle boundaries provide clear checkpoints for logs and tests:
  - "policy applied"
  - "policy verified"
  - "policy locked"
- This supports security review and incident triage.

### Operational model implied by lifecycle

- Boot/provisioning code owns transitions from `Uninitialized` to `Configured`.
- A validation gate (readback and invariants) is required before `Locked`.
- Runtime code consumes status from the locked monitor but does not mutate it.
- Any future "update mode" must be an explicit, authenticated state machine,
  not ad-hoc writes to locked policy tables.

### Test implications

- Unit tests: verify invalid transitions fail deterministically.
- Integration tests: verify lock permanence and no post-lock writes.
- Silicon validation: verify lock bits and policy table contents survive the
  full boot-to-runtime handoff window.

## Phase 1: Foundations

- Complete `registers.rs` accessors for required SPIPF registers.
- Stabilize public enums and errors in `types.rs`.
- Keep unsafe access contained to register constructor perimeter.

## Phase 2: Controller API

- Provide lifecycle API in `controller.rs`:
  - construct
  - apply policy
  - lock
  - read back status
- Validate transitions and lock semantics.

## Phase 3: Policy Model

- Introduce declarative policy object in `policy.rs`.
- Add predefined profiles in `profile.rs` for runtime and update modes.
- Add unit tests for policy encoding and bounds checks.

## Phase 4: Integration

- Hook monitor policy into trusted bring-up sequence.
- Add readback assertions in integration tests.
- Document silicon-only checks for lock permanence and routing behavior.
