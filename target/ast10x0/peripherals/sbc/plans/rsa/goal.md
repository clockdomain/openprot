# RSA Behavioral Parity Goal (AST1060)

*Scope: the **RSA modexp operation** of the `peripherals/sbc` SBC public-key
engine (operation #2). Shares the engine, the Confined-`unsafe` façade, and
the cooperative-yield device/op layer with ECDSA (operation #1) per
`../goal.md` ADR-5. This is a **separate peripheral-parity-port** with its own
authority (`rsa_aspeed.c`) and its own correctness authority (NIST RSA KAT) —
not folded into the ECDSA goal.*

> **Status: Phases 0–3 complete (authority §0; spec §1; parity standard
> §Objective; deltas ledger §2, lone intentional delta R1 discharged).
> Phases 4–8 NOT yet done.** Plan (§3) and done criteria (§4) are
> `TODO (Phase N)`; OPEN items RO1/RO3 + hazard HZ-R1 (§2.2) carried, not
> guessed. Do not treat anything below a `TODO` marker as decided.

## Objective

The RSA modexp path of `ast10x0/peripherals/sbc` must reach behavioral parity
with the **authoritative model**: the upstream Zephyr `aspeed_rsa` driver —
`rsa_aspeed.c` + `rsa_aspeed_priv.h` — from the AspeedTech-BMC Zephyr fork,
pinned at **`zephyr @ cfe94dc149ffa0af7e1af668a27f57eecf0cd1e9`** (the *same*
deployed revision as the HACE and ECDSA authorities — one Zephyr build runs
all three). Frozen verbatim at [zephyr-reference/](zephyr-reference/)
(sha256-verified, see `PINNED_COMMIT.txt`); every behavioral claim must be
grounded in that copy, read directly.

**`Authority = AspeedTech-BMC/zephyr rsa_aspeed.c @ cfe94dc`.** It is the model
the deployed AST1060 firmware runs.

**Informative-only:** `aspeed-rust`'s RSA path (second port of the same
hardware; useful for cross-checking register offsets only). Where it diverges
from the pinned Zephyr driver, the Zephyr driver wins and `aspeed-rust` is
treated as buggy (same normative-over-convenient discipline that caught
`aspeed-rust` wrong on the ECDSA trigger/delays — `../goal.md` §0.3).

**Parity standard (decided 2026-05-17, human-owned fork): observable parity,
keep fixes.** The port must reproduce the pinned `rsa_aspeed.c` **observable
output exactly for every input any real consumer can produce** (§0.1/§0.2):
for the same key + input, the identical modexp result bytes — same
byte-reversal in (§1.2 steps 4–6), same key-length word (step 7), same
de-reversed + leading-zero-stripped result and `out_len` (step 10). Where the
authority has a latent defect or unsafe behavior that **no reachable input
triggers**, the port keeps the safer/correct behavior, recorded as an
*intentional delta* with a reachability trace; everything else is strict
observable identity.

Direct consequences for §2/Phase 3 under this standard:
- **Unbounded poll → bounded `SbcOp`/typed `SbcError::Timeout`:** *intentional
  delta, keep the fix* — the only divergent observable is the wedged-engine
  **fault** path (§1.5), unreachable on valid input; identical shape and
  justification to ECDSA D3 (already discharged in `../goal.md` §2.1). The
  shared `POLL_YIELD_NS` is already the authority's 10 µs (`rsa_aspeed.c:79`)
  — conformant.
- **Byte-reversal / key-len word / result strip (§1.2):** these *are* the
  observable transform — must **match the authority exactly**; any
  `aspeed-rust` divergence is rejected as buggy (normative-over-convenient).
  Default = conform.
- **RO3 (engine clock):** not a parity *delta* — it is an enabling
  precondition. Under "observable parity" the port must produce the
  authority's output, which requires the engine actually running; if that
  needs the same clock-enable `rsa_init` performs (`:163`), doing so is
  **conformance with authority init**, not a deviation. Resolved in Phase 5.

Scope is AST1060 only: the RSA **modexp** primitive (public-exponent
`enc/verify`, private-exponent `dec/sign`). PKCS#1 v1.5 padding / digest
comparison is **not** in the engine — it is consumer-side (§0.1) and out of
this port's scope.

---

## 0. Phase 0 — Authority & consumer chain (DONE)

### 0.1 Consumer chain (what actually calls the engine)

Deployed RoT firmware → HRoT HAL middlelayer → Zephyr crypto driver → engine:

1. `aspeed-zephyr-project/lib/hrot_hal/crypto/rsa_aspeed.c`
   (`decrypt_aspeed:18-42`, `sig_verify_aspeed:56-89`) — application entry.
   `sig_verify_aspeed` does **raw RSA only** via the engine (`rsa_verify` =
   public-exponent modexp), then `memcmp(plain_text + out_len - match_length,
   match, match_length)` (`:81`) — **the PKCS#1 v1.5 unpad / digest compare is
   in the consumer, not the engine**.
2. Zephyr `rsa_aspeed.c::aspeed_rsa_session_setup` (`:121-146`) — single
   in-flight: `if (drv_state.in_use) return -EBUSY` (`:129-132`); `memcpy`s
   the caller `rsa_key` into the single global `drv_state`
   (`NON_CACHED_BSS_ALIGN16`, `:39`); wires `ops.{encrypt,verify}` →
   `aspeed_rsa_enc` (exponent `e`) and `ops.{decrypt,sign}` →
   `aspeed_rsa_dec` (exponent `d`) (`:139-142`).
3. `aspeed_rsa_enc`/`aspeed_rsa_dec` (`:103-119`) — same `aspeed_rsa_trigger`,
   differing only in which exponent (`key->e`/`key->d`) and its bit-length.
4. `aspeed_rsa_trigger` (`:41-101`) — the register/SRAM sequence (Phase 1
   target).

### 0.2 Interface authority (consumer-enforced contract)

- **Operation:** modular exponentiation `out = in^x mod m`, `x ∈ {e, d}`.
  No padding, no hashing in the engine (`query_hw_caps = NULL`, `:174`).
- **Key:** `struct rsa_key { m, e, d, m_bits, e_bits, d_bits }` (from
  `rsa_aspeed_priv.h` / `<zephyr/crypto/rsa_structs.h>`); copied into the
  global at session open.
- **Packet:** `rsa_pkt { in_buf, in_len, out_buf, out_len, out_buf_max }`.
- **Input bound:** `data_len > 512 ⇒ -EINVAL` (`:54-57`) — max 512-byte
  (4096-bit) operand. Modulus/exponent up to the same.
- **Concurrency:** strictly one in-flight, single global state, `-EBUSY` on
  overlap (`:39,129-132,154`). Matches the SBC engine's shared non-reentrant
  contract (ADR-5) — RSA and ECDSA contend for the *same* engine.
- **Result:** big-endian, leading zeros stripped; `out_len` = significant
  byte count (`:83-97`).

### 0.3 §2-preview — Phase-0 facts (full spec is Phase 1, full deltas Phase 3)

Recorded as orientation; **not** the Phase-1 spec. All cite the frozen
`zephyr-reference/rsa_aspeed.c`.

- **Same SBC engine as ECDSA** (ADR-5): `SEC_RD/SEC_WR` over the `secure`
  block, scratch SECSRAM (`SBC_SRAM_BASE = 0x7900_0000`, already pinned &
  QEMU-corroborated for ECDSA — applies to RSA), status reg `0x14`, trigger
  reg `0xbc`. RSA-distinct: poll bit is `BIT(4)` (`:81`) (ECDSA `BIT(20/21)`);
  trigger writes `1` then `0` (`:76-77`) (ECDSA writes `2`).
- **Key-length register `0xb0`** = `e_bits << 16 | m_bits` (`:75`).
- **SRAM operand layout (byte-reversed in):** zero `0x1800` (`:59`);
  exponent → `sram[0..]` reversed (`sram[i]=e[e_len-1-i]`, `:61-63`);
  modulus → `sram+0x400` reversed (`:65-68`); data → `sram+0x800` reversed
  (`:70-73`); **result ← `sram+0x1400`**, read back reversed with
  leading-zero strip, `RSA_MAX_LEN = 0x400` (`:83-95`); zero `0x1800` after
  (`:98`).
- **Completion:** unbounded `do { k_usleep(10); } while(!(sts & BIT(4)))`
  (`:78-81`) — **no timeout** (same hang shape as ECDSA D3; the port's
  bounded-poll/`SbcError::Timeout` will again be the lone intentional delta,
  to be reclassified in §2 under the Phase-2 standard).
- `ASPEED_SEC_MCU_MEMORY_MODE = 0x5c` is `#define`d but **unused** in
  `aspeed_rsa_trigger` — **OPEN (RO1)**: do not assume it is written.
- Endianness OPEN (RO2): operands are loaded **byte-reversed** (little-endian
  limbs); the port must reproduce the exact reversal, not guess.

---

## 1. Reference behavior to replicate  (Phase 1 — DONE)

Language-neutral behavioral spec, reverse-engineered from the frozen
[zephyr-reference/rsa_aspeed.c](zephyr-reference/rsa_aspeed.c) (sha256
`1b73a05e…`) read directly. Every claim cites `rsa_aspeed.c:line`. Behavior
only — the C structs (`aspeed_rsa_ctx`, `rsa_key`, `rsa_pkt`) are *not* a port
target. Unknowns are `OPEN`, never guessed.

### 1.1 Two address spaces (shared with ECDSA — ADR-5)

- **Engine MMIO** — `rsa_base` (DT idx 0, `:165,178`); `SEC_RD/SEC_WR(off) =
  sys_{read,write}32(rsa_base + off)` (`:24-25`). This is the same SBC
  `secure` block ECDSA uses. Offsets touched: `ASPEED_SEC_STS = 0x14`
  (status), `ASPEED_SEC_RSA_KEY_LEN = 0xb0`, `ASPEED_SEC_RSA_TRIG = 0xbc`
  (`:16-19`). `ASPEED_SEC_MCU_MEMORY_MODE = 0x5c` is `#define`d but **never
  accessed** in the trigger path (RO1).
- **Engine SRAM** — `sram_base` (DT idx 1, `:166,179`), addressed as a raw
  byte buffer `char *sram` (`:48`); the `0x1800`-byte scratch (`SBC_SRAM_BASE
  = 0x7900_0000`, already pinned & QEMU-corroborated for ECDSA).

Operands are **byte-reversed** between caller buffers and SRAM (caller buffers
are big-endian; the engine consumes little-endian limbs). This is normative
and exact (cited per step) — the port must reproduce the precise reversal,
not approximate it.

### 1.2 Modexp sequence (`aspeed_rsa_trigger`, `:41-101`)

`out = in^x mod m`, `x` = `e` (enc/verify) or `d` (dec/sign); the caller
passes the chosen exponent + its bit-length as `e`/`e_bits` (`:103-119`).

1. `m_len = (m_bits+7)/8`, `e_len = (e_bits+7)/8` — byte lengths from bit
   lengths (`:45-46`).
2. **Reject** `data_len > 512 ⇒ -EINVAL`, *before any register/SRAM write*
   (`:54-57`) — max 512-byte (4096-bit) input operand.
3. `memset(sram, 0, 0x1800)` — zero the full scratch (`:59`).
4. **Exponent → SRAM `0x0`, byte-reversed:** `sram[i] = e[e_len-1-i]`,
   `i ∈ [0,e_len)` (`:61-63`).
5. **Modulus → SRAM `0x400`, byte-reversed:** `(sram+0x400)[i] =
   m[m_len-1-i]` (`:65-68`).
6. **Input data → SRAM `0x800`, byte-reversed:** `(sram+0x800)[i] =
   data[data_len-1-i]` (`:70-73`).
7. **Key-length register:** `SEC_WR(e_bits << 16 | m_bits, 0xb0)` — exponent
   bit-length in the high 16 bits, modulus bit-length in the low 16 (`:75`).
8. **Trigger:** `SEC_WR(1, 0xbc)` then `SEC_WR(0, 0xbc)` — a 1→0 pulse, **no
   delay between** (contrast ECDSA's 5 ms hold) (`:76-77`).
9. **Completion — UNBOUNDED:** `do { k_usleep(10); sts = SEC_RD(0x14); }
   while (!(sts & BIT(4)));` — 10 µs poll, status **bit 4** = done, **no
   timeout / no error exit** (`:78-81`). (No pass/fail bit — RSA is modexp;
   there is no verdict, only the computed result.)
10. **Result ← SRAM `0x1400`, de-reversed + leading-zero strip:** read
    `RSA_MAX_LEN = 0x400` bytes; iterate `j` from `0x3ff` down to `0`; skip
    leading zero bytes (decrementing `result_nbytes`) until the first
    non-zero, then copy the rest big-endian into `dst`; `*out_len =
    result_nbytes` (`:83-97`).
11. `memset(sram, 0, 0x1800)` — zero the scratch again (`:98`); `return 0`
    (`:100`). (RSA *does* clear scratch on the success path — contrast ECDSA,
    which performs no teardown on reachable paths.)

**No reset / mode-register step.** Unlike ECDSA (`secure0b4` reset + 1 ms,
`0x7c` mode words), RSA touches **only** `0xb0`, `0xbc`, `0x14`. Confirmed by
reading the whole function — there is no other engine MMIO write.

### 1.3 Dispatch & validation state machine

- **enc/verify** `aspeed_rsa_enc` (`:112-119`): trigger with `e = key->e`,
  `e_bits = key->e_bits` (public exponent).
- **dec/sign** `aspeed_rsa_dec` (`:103-110`): trigger with `e = key->d`,
  `e_bits = key->d_bits` (private exponent). Both share one
  `aspeed_rsa_trigger`; the *only* difference is which exponent.
- **Session open** `aspeed_rsa_session_setup` (`:121-146`): `in_use ⇒
  -EBUSY` (`:129-132`); `memcpy` the caller `rsa_key` into the single global
  `drv_state.data.key` (`:137`); bind `ops.{encrypt,verify}=enc`,
  `ops.{decrypt,sign}=dec` (`:139-142`) — so **sign ≡ dec ≡ `^d`**,
  **verify ≡ enc ≡ `^e`**: raw modexp, no padding/hash.
- **Session free** (`:148-157`): `in_use = false`. **Init** `rsa_init`
  (`:159-169`): `clock_control_on(clk)` (RO3), `in_use = false`, latch
  `rsa_base`/`sram_base` from DT.

### 1.4 Memory / concurrency model

Single global `drv_state` (`NON_CACHED_BSS_ALIGN16`, `:39`); one op in flight
via `in_use`/`-EBUSY`. Key copied into the global at session open; `pkt`
operands read live during the trigger. The `0x1800` scratch is zeroed **both
before and after** every op (`:59,98`) — no cross-op residue. The engine is
**physically shared with ECDSA** (ADR-5); the deployed system serializes
across drivers, the port serializes through one non-reentrant `SbcDevice`.

### 1.5 Completion / error model

| Outcome | Mechanism | Cite |
|---------|-----------|------|
| Input too large | `-EINVAL`, before any write | `:54-57` |
| Concurrent use | `-EBUSY` at session open | `:129-132` |
| Success | `0`, `out_len` set, big-endian result in `dst` | `:83-100` |
| Engine wedged | **none — unbounded hang** (no timeout) | `:78-81` |

No verdict/status-of-correctness bit exists: the engine returns the modexp
result; *correctness* is the math (NIST RSA KAT, Phase 4) and any
padding/digest check is consumer-side (§0.1). The unbounded poll is the same
shape as ECDSA D3 — the port's bounded `SbcOp` loop → `SbcError::Timeout`
will again be the lone intentional delta (classified in §2 under Phase 2).

### 1.6 OPEN items (carried into Phase 3 — not guessed)

- **RO1:** `ASPEED_SEC_MCU_MEMORY_MODE = 0x5c` is defined but never accessed
  in `aspeed_rsa_trigger`/`rsa_init` (whole file read). Behavior: **not
  written**. Its purpose/whether other firmware sets it is unknown — the port
  must not write it absent evidence.
- **RO2:** the operand byte-reversal (steps 4–6) and result de-reversal
  (step 10) are *fully specified above* and normative; "OPEN" only in the
  sense that the port must reproduce the **exact** index arithmetic, verified
  against the frozen lines, not reimplemented from memory.
- **RO3 (raised Phase 1):** `rsa_init` calls `clock_control_on(config->
  clock_dev, clk_id)` (`:163`) — the RSA path enables an engine **clock**;
  ECDSA's `ecdsa_init` did **not**. Whether the SBC public-key engine clock
  is shared with / already enabled by the ECDSA path, or RSA needs an
  explicit SCU clock-enable, is **OPEN** — resolve in Phase 5 against the SCU
  driver; do not assume the engine is clocked just because ECDSA worked.

## 2. Deltas vs. the authority  (Phase 3 — DONE)

Standard: **observable parity, keep fixes** (§Objective). Authority
re-verified against the frozen
[zephyr-reference/rsa_aspeed.c](zephyr-reference/rsa_aspeed.c) read directly,
*not* `aspeed-rust`. "Port behavior" = the Phase-5 *target*; no RSA driver
code exists yet, so R2–R7 are **conformance obligations on the Phase-5
implementation**, not physical deltas. The only RSA behavior already in code
is the shared `SbcOp` bounded-poll loop (reused from the ECDSA op).

| ID | Authority `rsa_aspeed.c` @ cfe94dc (verbatim + `file:line`) | Port target | Classification |
|----|------------------------------------------------------------|-------------|----------------|
| **R1** | Completion wait **unbounded**: `do { k_usleep(10); sts = SEC_RD(0x14); } while (!(sts & BIT(4)));` — 10 µs, no timeout (`:78-81`) | Shared `SbcOp` bounded `poll_budget` on `BIT(4)`; exhaustion → `SbcError::Timeout` after façade cleanup | **THE LONE INTENTIONAL DELTA — keep fix.** Discharged §2.1 (by reference to ECDSA D3 — *identical shared loop* — plus RSA input-bound trace). |
| **R2** | Operands byte-reversed: exp→SRAM `0x0`, mod→`0x400`, data→`0x800` (`:61-73`); key-len `SEC_WR(e_bits<<16|m_bits, 0xb0)` (`:75`); trigger `0xbc` 1→0 (`:76-77`); result ← `0x1400`, de-reversed + leading-zero strip, `RSA_MAX_LEN 0x400` (`:83-97`) | Reproduce this transaction **exactly** (byte-for-byte SRAM layout, key-len word, result strip + `out_len`) | **Conformance (Phase-5 obligation).** This *is* the observable transform. `aspeed-rust` divergences rejected (normative-over-convenient). |
| **R3** | Scratch `memset(sram, 0, 0x1800)` **both before and after** the op (`:59,98`) | Same zero-before + zero-after | **Conformance (obligation).** Reachable-path behavior (no cross-op residue). NB the *opposite* of ECDSA, which performs no teardown — RSA's authority *does* clear, so the port must. |
| **R4** | RSA touches **only** `0xb0`/`0xbc`/`0x14` — **no** `secure0b4` reset, **no** `0x7c` mode words (whole `aspeed_rsa_trigger` read) | RSA façade ops emit only those three; **must not** route through ECDSA's `start_verify` (which resets + writes mode words) | **Conformance (architectural obligation).** See HZ-R1 (§2.2). |
| **R5** | `rsa_init` enables an engine **clock**: `clock_control_on(clock_dev, clk_id)` (`:163`); `ecdsa_init` did not | Engine clocked before RSA modexp — by the same clock-enable, or proven already-on | **Conformance-with-authority-init (obligation), Phase-5.** Not a parity *delta* (enabling precondition); OPEN RO3 — discharge against the SCU driver, do not assume. |
| **R6** | `sign≡dec≡^d`, `verify≡enc≡^e` — raw modexp; PKCS#1 v1.5 unpad + digest `memcmp` is **consumer-side** (`hrot_hal/.../rsa_aspeed.c:81`), not the engine | Port implements modexp only; padding/digest stays the caller's | **Out-of-scope by decision = conformance with the consumer contract.** Nothing the deployed consumer reaches is omitted. |
| **R7** | `ASPEED_SEC_MCU_MEMORY_MODE = 0x5c` defined but **never written** (whole file read) | Do not write `0x5c` | **Conformance** (do-nothing == authority). RO1 — recorded so a future "configure mode reg" guess cannot creep in. |

### 2.1 R1 reachability trace (discharge)

- **Authority lines, frozen, read directly:** `do { k_usleep(10); sts =
  SEC_RD(ASPEED_SEC_STS); } while (!(sts & BIT(4)));`
  (`zephyr-reference/rsa_aspeed.c:78-81`) — no counter, no timeout, no error
  return. The only behavioral divergence the bounded port introduces is on
  the path where `BIT(4)` is **never** asserted.
- **Consumer trace (real code):** `aspeed-zephyr-project/lib/hrot_hal/crypto/
  rsa_aspeed.c::decrypt_aspeed` (`:18-42`) / `sig_verify_aspeed` (`:56-89`) →
  `rsa_begin_session` → `aspeed_rsa_{enc,dec}` (`rsa_aspeed.c:103-119`) →
  `aspeed_rsa_trigger`. Every reaching input is bounded: `data_len > 512 ⇒
  -EINVAL` *before any register write* (`:54-57`); key size is fixed for the
  session. ⇒ modexp completion is a function of (fixed) key/operand size, not
  of any value that sits near a timeout boundary; **no consumer input selects
  whether `BIT(4)` asserts.**
- **Conclusion:** for every reachable input the engine completes and bounded
  port == unbounded authority at the same point — observable parity holds on
  the whole reachable space. The sole divergence (`Timeout` vs. infinite
  hang) is the wedged-engine **fault**, not a reachable input. Same shared
  `SbcOp` loop already discharged for ECDSA (`../goal.md` §2.1); this trace
  adds the RSA-specific input bound. **Discharged.**
- **Residual obligation (stronger than ECDSA's — stated, not assumed):** RSA
  4096-bit modexp latency is **substantially larger** than ECDSA P-384
  verify. The shared `DEFAULT_POLL_BUDGET` being adequate for ECDSA does
  **not** imply adequacy for RSA. Phase 6 carries a binding obligation:
  validate the chosen budget exceeds worst-case RSA modexp latency on
  QEMU-N/A→**silicon** before R1 is closed for the done-criteria; a too-low
  budget would spuriously time out a *correct* RSA op (a real
  observable-parity break). Do not inherit ECDSA's budget discharge.

### 2.2 New OPEN / hazard items raised in Phase 3

- **HZ-R1 (façade-op separation hazard, R4):** the shared `SbcRegisters`
  façade's existing `start_verify` performs ECDSA-specific `secure0b4`
  reset + `0x7c` mode-word writes + a `0xbc`←2 trigger. The RSA path must be
  **separate façade ops** emitting only the byte-reversed SRAM load, key-len
  `0xb0`, and `0xbc`←1→0 / `0x14` `BIT(4)`. Naively reusing `start_verify`
  for RSA would inject MMIO writes the RSA authority never does — an
  immediate observable-parity break. Recorded so the shared-façade
  consolidation (ADR-5) does not tempt op-conflation.
- **RO3 (engine clock, R5):** carried from §1.6 — `rsa_init`'s
  `clock_control_on` has no ECDSA analogue; resolve in Phase 5 against the
  SCU driver (is the SBC PKE clock shared/already-enabled, or RSA-specific?).
  Not guessed.
- **RO1 (`0x5c`):** carried from §1.6 — defined, never written; port must
  not write it absent evidence.

## 3. Implementation plan
`TODO (Phase 5)` — reuse the shared `SbcRegisters`/`SbcDevice`/`SbcOp`
(ADR-5); add RSA façade ops (load operands byte-reversed, key-len `0xb0`,
trigger `0xbc`←1/0, poll `0x14` `BIT(4)`, read+de-reverse result `+0x1400`)
and an RSA operation layer + HAL skin.

## 4. Done criteria
`TODO (Phase 6)` — split like ECDSA §4: QEMU-feasible (no ECC/RSA engine on
QEMU per ADR-4 → bounded-timeout positive test + operand-layout pin) vs.
HARDWARE-ONLY **NIST RSA KAT** (independent correctness authority, Phase 4) on
the AST1060 EVB, tests under `tests/peripherals/sbc/rsa/`.

## 5. Architecture decisions
Shared-engine consolidation is recorded in `../goal.md` ADR-5 (normative for
both operations). RSA-specific ADRs `TODO` as they arise.
