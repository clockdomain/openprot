# Kickoff Prompt — ECDSA Device Layer (AST10x0)

Purpose: add the **Cooperative-Yield Bounded-Poll Device** binding on top of the
`EcdsaRegisters` façade. Pattern reference: `design-patterns` skill, catalog
entry `cooperative-yield-bounded-poll-device` (its Checklist is the acceptance
gate).

> **Scope note:** this is the *wait-policy layer* — `EcdsaDevice<Y: FnMut(u32)>`
> + the bounded poll loop + the borrow-split type-erased adapter. It composes
> **on top of** `target/ast10x0/peripherals/ecdsa/registers.rs` (the façade)
> and **below** the future typestate/protocol layer. It does **not** design the
> ECDSA verify semantics — those land later under `peripheral-parity-port`.

---

## Facts to front-load (the pattern's Implementation needs these)

1. **Reference instance to mirror:** `target/ast10x0/peripherals/hace/device.rs`
   (`HaceDevice<Y>`) + `hace/digest.rs` (borrow-split, type erasure, the two
   bounded poll loops). Copy its shape — do not reinvent.
2. **Façade dependency (confirm, don't guess):** the loop must poll a *safe
   façade predicate*. `EcdsaRegisters::verify_is_done()` is currently a
   `todo!()` stub. The device layer wires the *structural* seam now (bounded
   loop calling that predicate, yield between polls, typed timeout); the
   predicate body itself stays stubbed until `peripheral-parity-port` fills it
   (it maps to `secure014` bit-20, per `aspeed-rust/src/ecdsa.rs`).
3. **Yield argument:** advisory ns constant, mirroring HACE's `POLL_YIELD_NS`
   (reference ECDSA poll interval ≈ 5 µs — `delay_ns(5000)`).
4. **Budget:** explicit `poll_budget: u32` + default constant +
   `with_timeout_polls` override; exhaustion → typed `EcdsaError::Timeout`.
5. **Stop condition:** device binding + adapter + bounded-poll seam + timeout
   error. No verify behavior, no typestate layer.

---

## Recommended prompt (copy-paste)

> Add the Cooperative-Yield Bounded-Poll Device layer for the AST10x0 ECDSA
> engine, following the `design-patterns` catalog entry
> `cooperative-yield-bounded-poll-device`. Mirror
> `target/ast10x0/peripherals/hace/device.rs` (`HaceDevice<Y>`) and the
> borrow-split / type-erased adapter + bounded poll loops in
> `hace/digest.rs` exactly in shape.
>
> New `EcdsaDevice<Y: FnMut(u32)>` over the `EcdsaRegisters` façade
> (`target/ast10x0/peripherals/ecdsa/`): construction gate injecting the
> yield strategy, `poll_budget` + default const + `with_timeout_polls`,
> non-reentrant contract documented at the gate. Build the operation adapter
> by borrow-splitting the device (`Copy` the façade handle + budget, reborrow
> `yield_fn` as `&mut dyn FnMut(u32)` — type-erased so the adapter is not
> generic over `Y`). The completion wait is the bounded loop:
> `for _ in 0..poll_budget { if regs.verify_is_done() { … } (yield_fn)(ns); }`
> → typed `EcdsaError::Timeout` on exhaustion after façade cleanup.
>
> The façade predicate `verify_is_done()` stays a `todo!()` stub for now —
> wire the seam to it, don't implement the verify semantics (that's
> `peripheral-parity-port`). No PAC types, no `unsafe` above the façade.
>
> Acceptance: satisfies every box in the entry's Checklist. Stop after the
> device binding + adapter + bounded-poll seam + timeout error.

## Minimal version

> Apply the `cooperative-yield-bounded-poll-device` pattern to add
> `EcdsaDevice<Y>` over `EcdsaRegisters`, mirroring `HaceDevice`/`HaceDigest`.
> Façade predicate stays stubbed; wire the bounded-poll/yield/timeout seam
> only. Stop there.

---

## Why the prompt is shaped this way

- Naming the catalog entry triggers the `design-patterns` skill → the entry
  and its 7-box Checklist load as the acceptance gate.
- "Mirror `HaceDevice`/`HaceDigest` exactly" — there is now a conforming
  reference instance; shape-matching it is the cheapest path to conformance.
- "Predicate stays stubbed, wire the seam only" — same staging discipline as
  the façade kickoff: the structural pattern is established before the
  behavioral spec exists; verify semantics are filled under
  `peripheral-parity-port`, not invented here.

## Known caveat

Unlike HACE (whose façade ops were already real when its device layer was
wired), `EcdsaRegisters`'s predicates are still `todo!()`. This task therefore
produces a *structurally* conforming device layer whose poll loop is
behaviorally inert until the parity-port phase fills `verify_is_done()` — the
Checklist is satisfiable on structure, but not exercisable end-to-end until
then.
