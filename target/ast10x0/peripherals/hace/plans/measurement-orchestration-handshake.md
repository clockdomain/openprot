# ADR — Flash-region measurement across three isolated userspace drivers

*Design ADR / handshake sketch. **Proposal, not implemented.** Companion to
`flash-hashing-consumer-evidence.md` (how the deployed firmware feeds flash to
HACE) and `goal.md` §5 (user-space driver shape / SW-HW selection). Citations:
`goal.md §N`, the `design-patterns` catalog entries, and the
`userspace-driver-client` skill invariants — named, same discipline as the
other plans docs.*

## Context

Three protection domains, no shared memory by default:

| Domain | Authority | Whole-object client seam |
|---|---|---|
| **F** flash driver | the SPI flash | `read(region) -> bytes` |
| **C** crypto / HACE driver | the singleton HACE engine | `hash(algo, input) -> digest` |
| **M** measurement service | interprets the PFR manifest | orchestrator (holds F + C clients) |

Scenario: M must measure a flash region `R = {dev, off, len, algo}` named by
the manifest and compare the digest to the manifest's expected value `E`.

## Forces (each rejects a "natural" design)

- **Whole-object, run-to-completion across every boundary** —
  `userspace-driver-client` Invariant 1; `goal.md` §5.2(2). No
  `begin/update*/finish`, no lock/session held across IPC.
- **HACE is a non-multiplexable singleton** — `goal.md` §5.1 (ASPEED: no
  concurrent streaming; single global ACC context; `in_use`/`-EBUSY`). A
  session pinned across an IPC/scheduler yield starves PFR/DICE/attestation =
  availability/DoS (`goal.md` §5.2(1)).
- **Separation of duties** — `userspace-driver-client` skill: orchestration
  needing another driver's data belongs in the *consumer* composing two
  clients, **not** a cross-domain op handing one server another's authority
  (confused deputy).
- **Deployed reality** — the reference firmware already stages flash → a
  non-cached 4 KB RAM page buffer, then HACE SG-DMAs from RAM, never from
  flash MMIO (`flash-hashing-consumer-evidence.md`, cited there).

### Rejected designs (cited reason)

1. **M pumps `begin/update*×N/finish` to C over IPC.** Held HACE session
   across IPC — Invariant 1 + `goal.md` §5.2(1)/(2). Rejected.
2. **C reads flash itself (C becomes a flash client).** Confused deputy —
   skill *Separation of duties*. Rejected **as literally stated**.

### The conflict this ADR exists to resolve

`goal.md` §5.2(2) already decided C exposes
`hash_region(flash/mem descriptor) -> digest` with "the driver runs the page
loop itself." Read as a raw address that is design 2 (confused deputy). The
two reconcile through exactly one mechanism: **the descriptor is a
call-scoped capability/lease, not an address.** The skill blesses this: "a
call-scoped memory lease … is a zero-copy optimization of the hand-off, not a
change in trust."

## Decision

**Consumer-composed; one whole-object transaction per boundary; a call-scoped
read-only lease is the only thing that crosses, and M owns it.**

```
 M (orchestrator)             F (flash)               C (HACE)
   manifest ⇒ R, expected E
   ├─① read(R) ───────────────►│ page loop run-to-completion IN F
   │◄──────── bytes → buf B ────┤ (B owned by M; non-cached if HACE-DMA'd)
   ├─② hash(algo, lease(B, RO, this-call)) ─────────────────────►│
   │   one whole-object request                                  │ run-to-completion IN C:
   │                                                             │  HACE SG-DMA over leased B;
   │                                                             │  cooperative-yield bounded
   │                                                             │  poll; typed Timeout on
   │◄────────────────────────────── digest D ────────────────────┤  wedge; engine released
   │                                                             │  BEFORE reply
   └─③ M: D == E ?  (measurement/verify policy stays in M)
       lease auto-revoked at ② return; no state spans a boundary
```

- Each boundary is one `Transport::transact` (bytes-in→bytes-out). F's page
  loop lives in F; C's HACE session lives and dies inside C's single request
  handler — never across IPC (Invariant 1, `goal.md` §5.2).
- M never delegates "read flash"; it delegates "read *this buffer*, for
  *this one call*." Trust unchanged (skill).
- C's internals are the already-built stack: Confined-`unsafe` MMIO façade →
  `cooperative-yield-bounded-poll-device` (typed `HaceError`/`SbcError::Timeout`
  on a wedged engine) → cross-process exclusivity is **C's internal request
  queue**, *not* a Rust borrow — the explicit liability in the
  `borrow-arbitrated-engine-exclusivity` entry ("cross-process sharing needs a
  real lock or service queue") and `goal.md` §5.2(2)'s "serialized by a real
  queue … the principled replacement for `in_use`/`-EBUSY`."

## The hinge (explicit decision dependency — resolve before implementing)

- **Platform HAS a call-scoped lease/capability primitive** (Hubris-style):
  the §5.2(2) `hash_region(lease) -> digest` is sound — C acts strictly under
  M's region-scoped, one-shot, read-only delegated authority. **Preferred.**
- **No lease primitive:** M must copy bytes, and then **a single flat SHA over
  a region larger than one transferable buffer is not expressible across three
  processes without a held C session** — the irreducible constraint. Ordered
  fallbacks:
  1. whole-region lease (= the preferred case);
  2. **co-locate flash-read + hash behind one "measure" server** so the page
     loop is in-process run-to-completion (`goal.md` §5.3: firmware/image
     measurement → HW via whole-object RPC, driver runs the page loop) — i.e.
     the *byte path* collapses to one bounded server op; M still holds the
     manifest authority and only names `R`;
  3. a segmented/Merkle digest **only if** `E` uses the same scheme — PFR
     manifests define a *flat* SHA, so (3) is normally **invalid**; do not
     adopt it to dodge (1)/(2).

Net: three *control* domains are fine, but the *byte path must collapse to one
whole-object op* — via a region lease (preferred) or a co-located
measure-server. M is always the orchestrator; C never gains standing flash
authority; no HACE session ever crosses IPC.

## Status / scope

Proposal. Not implemented; no code. The deciding input is the **lease/
capability primitive question** above — that is the open item a HACE/services
implementer must resolve first (it selects preferred vs fallback-2). Consistent
with and bounded by `goal.md` §5; does not change any pinned parity behavior.
