# Kickoff Prompt — ECDSA MMIO Façade (AST10x0)

Purpose: start the **Confined-`unsafe` MMIO Façade** newtype for the AST10x0
ECDSA engine. Pattern reference: `design-patterns` skill, catalog entry
`confined-unsafe-mmio-facade` (the entry's Checklist is the acceptance gate).

> **Scope note:** `openprot/hal/blocking/src/ecdsa.rs` is the *trait* side.
> The façade is a peripheral **register wrapper** at
> `target/ast10x0/peripherals/ecdsa/registers.rs`, mirroring
> `target/ast10x0/peripherals/hace/registers.rs` (`HaceRegisters`). Keep them
> separate — this task is the façade only.

---

## Facts to front-load (the pattern's Implementation needs these)

1. **PAC + register-block type** for the ECDSA/crypto engine — the one fact the
   agent must *confirm, not guess* (e.g. the `ast1060_pac` ECDSA register
   block).
2. **Target file:** `target/ast10x0/peripherals/ecdsa/registers.rs` (+ `mod.rs`
   wiring).
3. **Reference instance to mirror:** `target/ast10x0/peripherals/hace/registers.rs`.
4. **Curated API scope:** minimal — constructor + private `regs()` +
   `!Send`/`!Sync`; stub operations only. Do **not** expose the full register
   set or design the ECDSA API yet (that comes later via the
   `peripheral-parity-port` workflow).
5. **Stop condition:** façade + `mod.rs` only; no driver, no behavior.

---

## Recommended prompt (copy-paste)

> Create the Confined-`unsafe` MMIO Façade newtype for the AST10x0 ECDSA
> engine, following the `design-patterns` catalog entry
> `confined-unsafe-mmio-facade`. Mirror
> `target/ast10x0/peripherals/hace/registers.rs` (`HaceRegisters`) exactly in
> shape.
>
> New file: `target/ast10x0/peripherals/ecdsa/registers.rs`, type
> `EcdsaRegisters`, over the `ast1060_pac` ECDSA register block — find and
> confirm the correct register-block path before writing, don't guess.
>
> Single `unsafe fn new(base)` + `unsafe fn new_global()`, one private
> `regs()`, `PhantomData<*mut ()>`, `#[derive(Copy, Clone)]`. Expose only a
> minimal intent-named API for now (stub the operations; I'll fill them as the
> driver lands). No driver, no behavior, no PAC types escaping the façade.
>
> Acceptance: satisfies every box in the entry's Checklist. Stop after the
> façade + `mod.rs` wiring.

## Minimal version

> Apply the `confined-unsafe-mmio-facade` pattern to create `EcdsaRegisters`
> for AST10x0 ECDSA, mirroring `HaceRegisters`. Confirm the PAC register block
> first. Façade only — stop there.

---

## Why the prompt is shaped this way

- "Confined-`unsafe` MMIO Façade" / "apply the confined-unsafe-mmio-facade
  pattern" matches the `design-patterns` skill trigger → the catalog entry and
  its Checklist load automatically as the acceptance gate.
- "Find and confirm the register block before writing, don't guess" — the
  façade's entire correctness rests on the correct base pointer; that is the
  pattern's one real safety obligation.
- "Stub the curated operations" — prevents prematurely designing the ECDSA API
  before its behavioral spec exists; the API surface is filled later under
  `peripheral-parity-port`.
