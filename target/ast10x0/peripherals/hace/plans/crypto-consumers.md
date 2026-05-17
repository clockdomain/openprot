# Crypto-block consumers & use cases — companion to the HACE / SBC port docs

**Status:** reference companion. Non-normative *as a whole* — every row is a
distilled pointer into a normative source; the cited line in that source wins on
any conflict. Companion to:

- HACE: [goal.md](goal.md), [zephyr-behavior.md](zephyr-behavior.md)
- SBC (separate worktree/branch `openprot-ecdsa @ ast10x0-ecdsa`):
  `target/ast10x0/peripherals/sbc/plans/goal.md` (ECDSA) and
  `.../sbc/plans/rsa/goal.md` (RSA) — cited here by path, not vendored.
- Driver shape: [crypto-driver-sketch.md](crypto-driver-sketch.md) — the
  USART-framed layered-driver sketch built on the §3 deployment model below.

**Pinned authority (one deployed build supplies every block below):**
`AspeedTech-BMC/zephyr @ cfe94dc149ffa0af7e1af668a27f57eecf0cd1e9` (the same
revision pinned by both ports — see `hace/plans/zephyr-reference/PINNED_COMMIT.txt`
and `sbc/plans/zephyr-reference/PINNED_COMMIT.txt`).

**Universal consumer chain.** Every hardware-reachable block follows one shape:

> Deployed RoT firmware → HRoT HAL crypto middlelayer → Zephyr crypto driver → engine

The traces below stop at the **HRoT HAL middlelayer** — that is the
interface-authority boundary the ports conform to, and it hard-enforces the
operand contract, so nothing above it can change what the engine sees. The RoT
firmware *above* the middlelayer is real but deliberately **not** traced into
parity scope (HACE: §5; SBC: delta D6 / R6).

---

## 1. Consumer / use-case matrix

| Block | Operation | Traced consumer (call site) | Use case | Authority citation | Parity scope |
|-------|-----------|-----------------------------|----------|--------------------|--------------|
| **HACE** | SHA digest — **streaming** | `hash_device_firmware` / `flash_hash_contents`: repeated `update(4096)` from a fresh session | **PFR / field-firmware integrity** — SHA over flash images, page-streamed | `hace_aspeed.c` `aspeed_hash_update` `:489–492`; trace in [zephyr-behavior.md](zephyr-behavior.md) §D2 (`:267–270`) | In scope. 4 KB pages enter at `bufcnt==0` → D2 branch dormant; output = standard SHA |
| **HACE** | SHA-384 digest — **sub-block** | DICE: `update(48)`, `update(48)` → SHA-384 (block 128) | **DICE measured-boot / layered device identity & attestation** | [zephyr-behavior.md](zephyr-behavior.md) §D2 trace (`:271–272`) | In scope. `48+48 < 128` → sub-block early-return; D2 branch never reached |
| **HACE** | SHA transcript hash | **None — routed to in-process software by mandatory decision** | SPDM transcript hashing (spans network round-trips) | [goal.md](goal.md) §5.2.1 (`:711–718`) | **Explicit non-consumer.** HW path would let a slow SPDM peer starve PFR/DICE/attestation behind it (DoS on the RoT) |
| **HACE** | HMAC | `aspeed_hash_setkey` / `aspeed_hash_digest_hmac` (in the pinned driver; RFC-2104 threshold `key_len > block_size`) | Keyed MAC (no specific `aspeed-zephyr-project` call site named) | `hace_aspeed.c:619–659`; [goal.md](goal.md) D3 (`:279`) | **In driver, but gated by a separate KAT authority by decision** (§2.1); not byte-matched in the HACE goal |
| **HACE** | AES ECB/CBC, 128/256 — **raw key** | `aspeed_aes_crypt` via `aspeed_crypto_session_setup` | Symmetric confidentiality with a software-supplied key | `hace_aspeed.c:130–135` | In scope. Port adds a block-multiple input bound the C omits (delta A4) |
| **HACE** | AES — **OTP / secret-vault sideload key** | Non-`CAP_RAW_KEY` opaque 1-byte handle → `SELECT_VAL_KEY_1/2` + `HACE_CMD_AES_KEY_FROM_OTP` | Confidentiality bound to a **non-software-visible provisioned key** (sealing / protected storage on the RoT) | `hace_aspeed.c:113–128`; `hace_aspeed.h:16,193–199`; [goal.md](goal.md) A6 (`:376`) | **Select logic in scope** (handle→register/cmd, bit-exact). **Crypto E2E separated** — OPEN ISSUE §2.6 (no software oracle for an OTP key) |
| **SBC** | ECDSA **P-384 verify** | `aspeed_ecdsa_verify_middlelayer` — `aspeed-zephyr-project/lib/hrot_hal/crypto/ecdsa_aspeed.c:34–72` | **Secure-boot / firmware-image signature verification** | `sbc/plans/goal.md` §0.1 (`:96–128`); authority `ecdsa_aspeed.c:34–72,126–140` | Verify-only, P-384 only, 48-byte SHA-384 digest. No sign/keygen (D6 — out of scope = conformance with the consumer contract) |
| **SBC** | RSA modexp — **public exponent** | `sig_verify_aspeed` — `hrot_hal/crypto/rsa_aspeed.c:56–89` | RSA **signature verify** (`out = in^e mod m`); PKCS#1 v1.5 unpad + digest `memcmp` is **consumer-side**, not the engine | `sbc/plans/rsa/goal.md` §0.1 (`:75–94`); authority `rsa_aspeed.c:56–89` | Raw modexp only; padding/hashing stays the caller's (R6) |
| **SBC** | RSA modexp — **private exponent** | `decrypt_aspeed` — `hrot_hal/crypto/rsa_aspeed.c:18–42` | RSA **decrypt** (`out = in^d mod m`) | `sbc/plans/rsa/goal.md` §0.1 (`:75–94`); authority `rsa_aspeed.c:18–42` | Same `aspeed_rsa_trigger`; differs only in which exponent / bit-length |

---

## 2. Cross-cutting observations

1. **One firmware build, two ports, one authority commit.** HACE (digest/HMAC/
   AES) and SBC (ECDSA/RSA) are distinct engines but co-deployed; pinning the
   same Zephyr revision keeps the consumer contracts mutually consistent.
2. **Every traced consumer is the RoT's HRoT HAL crypto middlelayer**, never an
   application directly. The middlelayer is the enforced contract boundary
   (hard length/curve/operation checks); it is *itself* called by deployed RoT
   firmware that is intentionally out of parity scope because it provably cannot
   alter engine-visible behavior.
3. **Concurrency is uniform: strictly one in-flight per engine.** Every driver
   carries a single global `drv_state` + `in_use` / `-EBUSY` (HACE
   `aspeed_crypto_session_setup`; SBC `aspeed_{ecdsa,rsa}_session_setup`). Both
   ports replace the runtime flag with structural borrow-arbitrated exclusivity
   (HACE delta A1; SBC ADR-A1) — output-identical, overlap becomes a compile
   error.
4. **The only deliberate non-consumer is SPDM transcript hashing** — kept in
   software on purpose (availability/DoS, not performance). Worth keeping
   visible so a future "just use HACE for SPDM" change is recognised as a
   security regression, not an optimisation.

---

## 3. Deployment model — Zephyr-monolithic → microkernel

The consumer set above maps cleanly onto two consumption modes. This is **not a
free choice per consumer** — it is forced by the engine being a non-multiplexable
shared singleton ([goal.md §5.1](goal.md), normative). "Consumed as an in-process
lib crate" is true for exactly one of the two modes.

| | **Software path (in-process lib crate)** | **Hardware-engine path (one driver server)** |
|---|---|---|
| Consumers (from §1) | SPDM transcript, HKDF/session keys, signatures, DICE, secret-keyed AES, small one-shot hashes (default) | PFR/image/manifest measurement, bulk non-secret AES |
| Packaging | Pure trait-impl crate (`digest`/`mac`/cipher), linked **into each consumer's own address space** | HW driver crate linked into **exactly one** process; consumers reach it by whole-object RPC |
| Engine | None | The shared HACE singleton (hash⇄AES same engine; SBC a *separate* singleton) |
| Authority | [goal.md §5.2.1, §5.2.3](goal.md), §5.3 rows 1/3/5 | [goal.md §5.2.2](goal.md), §5.3 rows 2/4 |

**Why the HW path cannot be a per-consumer in-process lib.** Under monolithic
Zephyr the chokepoint (HRoT HAL middlelayer + global `drv_state` + `in_use`) is a
function call plus a flag. Splitting into a microkernel turns that **same
chokepoint into a process boundary**: N processes each linking the HW driver and
poking the singleton would recreate the cross-process held-across-yield lock
[goal.md §5.2.2](goal.md) explicitly forbids. The engine must therefore be owned
by **one** process that serializes whole-object, run-to-completion requests via an
internal queue (the principled replacement for `in_use`/`-EBUSY`) — never
`begin/update/finish` across IPC.

**The owning process** is the microkernel successor to the HRoT HAL crypto
middlelayer: a dedicated **RoT crypto/measurement driver server** holding the
borrow-arbitrated `HaceDevice` and the `.ram_nc` DMA context in its address
space. SBC is a *separate* engine with the same singleton discipline → its own
server (effectively a secure-boot-verify service, since its only traced consumer
is the HRoT verify path).

**Code-vs-address-space.** "Lib crate" describes how the port is *packaged*
(a `peripherals` crate); it is orthogonal to *which process* instantiates it. The
HW driver being a crate does **not** make per-consumer instantiation safe —
ownership stays singleton-per-engine.

**Boot-time caveat.** DICE measured-boot and early secure-boot verify (the
earliest consumers in §1) run before a full user-space exists and cannot RPC to a
server that is not up yet. They take the in-process software path or a minimal
early-boot driver instance — **not** the runtime RPC server.

---

## 4. Maintenance

When a new consumer or use case is traced (real code in the pinned tree, not
speculation), add a row with its call site and an authority line citation, and
classify its parity scope the same way the port goals do. If a row's authority
citation drifts from the pinned commit, the row is stale — re-trace before
trusting it.
