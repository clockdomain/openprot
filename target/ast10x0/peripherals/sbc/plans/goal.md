# ECDSA Behavioral Parity Goal (AST1060)

*Scope: the **ECDSA-verify operation** of the `peripherals/sbc` SBC public-key
engine (operation #1). RSA is operation #2 of the same hardware block — its
own peripheral-parity-port with a separate goal doc & authority
(`rsa_aspeed.c`); see ADR-5. The shared façade/device/op layer is common to
both.*

> **Status: Phases 0–5 done; code implemented; Phase 6 §4.A QEMU tests
> PASSING; §4.B NIST KAT firmware IMPLEMENTED (builds, user-run on EVB).
> Phase 8 ADR-3 remains; §4.B silicon run pending (user).**
> Authority pinned (§0), spec reverse-engineered (§1), parity standard decided
> (§Objective), deltas ledger built with the lone intentional delta discharged
> (§2), three authorities separated (§2.3), skin/HAL ADRs recorded (§5), the
> numbered plan written and P5-OPEN resolved (§3). **Code:** §3 items 1–6
> implemented in `target/ast10x0/peripherals/sbc/` (façade §1.2 sequence,
> 10 µs poll, `verify_raw`, thin `hal_impl` skin); builds clean under
> `--config=virt_ast10x0` with `-D warnings`. **Parity is by-construction
> only** (cited line-for-line vs `zephyr-reference/`) and **not executable on
> QEMU** — the AST1060 QEMU SBC has no ECC-engine model (ADR-4), so the
> verdict path is unreachable there. Done-criteria are split (§4): QEMU-
> feasible gates (interface/operand-order/D3-timeout) are automatable; the
> **verdict parity + NIST KAT are HARDWARE-ONLY, tagged `#[ignore]`, and the
> user runs them on AST1060 silicon later** (also discharges P5-OPEN-A
> behaviorally + budget tuning). P5-OPEN-A's *address* is corroborated by the
> QEMU SoC memmap. **§4.A QEMU tests now implemented & green under
> `--config=virt_ast10x0`** (`tests/peripherals/sbc/ecdsa/qemu/`). Remaining:
> §4.B HW-tagged NIST KAT (user, on silicon — `tests/peripherals/sbc/ecdsa/evb/`),
> then Phase 8 ADR-3. **Peripheral renamed `ecdsa`→`sbc` (ADR-5): one SBC
> engine; RSA is op #2, its own parity-port.** Work lives in `openprot-ecdsa`
> (branch `ast10x0-ecdsa`).

## Objective

The `ast10x0/peripherals/ecdsa` port must reach behavioral parity with the
**authoritative model**: the upstream Zephyr `aspeed_ecdsa` driver —
`ecdsa_aspeed.c` + `ecdsa_aspeed_priv.h` — from the AspeedTech-BMC Zephyr fork,
pinned at **`zephyr @ cfe94dc149ffa0af7e1af668a27f57eecf0cd1e9`**. A frozen
verbatim copy is the normative artifact at
[zephyr-reference/](zephyr-reference/) (see
`zephyr-reference/PINNED_COMMIT.txt`; vendored content sha256-verified equal to
`git show cfe94dc:drivers/crypto/ecdsa_aspeed.c`). All behavior claims must be
grounded in that frozen copy, read directly.

**`Authority = AspeedTech-BMC/zephyr ecdsa_aspeed.c @ cfe94dc`.** This is the
normative reference because it is the model the deployed AST1060 firmware
actually runs — and it is the *exact same pinned Zephyr revision already used
as the HACE authority* (`hace/plans/zephyr-reference/PINNED_COMMIT.txt`): one
deployed Zephyr build supplies both crypto drivers, so pinning the same commit
keeps the two ports consistent.

**Informative-only:** `aspeed-rust/src/ecdsa.rs`. It is a second Rust port of
the same hardware, useful for cross-checking register *offsets* and the SRAM
parameter layout, but it is **not** the spec. Where it diverges from the pinned
Zephyr driver, the Zephyr driver wins and `aspeed-rust` is treated as buggy.
Known divergences already observed (Phase 0 evidence, see §2-preview) — the
trigger value, every inter-step delay, and the poll/timeout model — confirm
`aspeed-rust` is the *convenient anchor*, not the authority. The
`registers.rs` / kickoff-prompt comments that cite `aspeed-rust` as the
reference were written at the façade *staging stub* and do **not** constitute a
normative selection; this document supersedes them.

**Parity standard (decided 2026-05-16, human-owned fork): observable parity,
keep fixes.** The port must reproduce the pinned Zephyr driver's **observable
output exactly for every input any real consumer can produce** (the P-384 /
48-byte SHA-384 verify path, §0.2): identical valid/invalid verdict, identical
register-write order and operand placement (§1.2). Where the authority has a
**latent defect or unsafe behavior that no reachable input triggers**, the port
**keeps the safer/correct behavior** and that single point is recorded as an
*intentional delta* with a reachability trace (§2). Everything else is strict
observable identity.

Direct consequence for P1–P5 (§0.3), to be formalized in §2/Phase 3 under this
standard:
- **P4 (unbounded hang → bounded `poll_budget`/typed `SbcError::Timeout`):**
  *intentional delta, keep the fix.* The authority's only divergent observable
  is the wedged-engine **fault** path (§1.5) — not reachable on any valid
  input; on every reachable input the verdict and sequence are identical. Same
  shape as HACE-D1. The advisory `POLL_YIELD_NS` must still track the
  authority's **10 µs** poll interval (§1.2 step 9), not aspeed-rust's 5 µs.
- **P1/P2/P3 (trigger value `2`, 1 ms/5 ms settle delays):** these affect the
  *observable* register transaction and engine driving — **not** latent-bug
  fixes. Under "observable parity" they must **match the authority** (the
  bit-0/5 µs `aspeed-rust` forms are rejected as buggy), unless a Phase 3
  reachability trace proves the difference is unobservable. Default = conform.
- **P5:** conformance (already identical).

Scope is AST1060 only (P-384 verify path; see consumer contract §0.2). No other
target, curve, or operation (no sign, no keygen) is in scope — the deployed
consumer wires only P-384 verify.

---

## 0. Phase 0 — Authority & consumer chain (DONE)

### 0.1 Consumer chain (what actually calls the engine)

Deployed RoT firmware → HRoT HAL middlelayer → Zephyr crypto driver → engine:

1. `aspeed-zephyr-project/lib/hrot_hal/crypto/ecdsa_aspeed.c::aspeed_ecdsa_verify_middlelayer`
   (`:34-72`) — the application-facing entry. Builds an `ecdsa_key`
   (`curve_id = ECC_CURVE_NIST_P384`, `qx`, `qy`) and an `ecdsa_pkt`
   (`m` = digest, `r`, `s`, all `_len = length`), then
   `ecdsa_begin_session` → `ecdsa_verify` → `ecdsa_free_session` (`:63-71`).
   **Hard-rejects any `length != 48`** (`:48-51`).
2. Zephyr `ecdsa_aspeed.c::aspeed_ecdsa_session_setup` (`:142-164`) — single
   in-flight: `if (drv_state.in_use) return -EBUSY` (`:150-153`); copies the
   key into the single global `drv_state` (`NON_CACHED_BSS_ALIGN16`, `:44`);
   wires `ctx->ops.verify = aspeed_ecdsa_verify`.
3. `aspeed_ecdsa_verify` (`:126-140`) — re-validates
   `r_len == s_len == m_len == 48 && curve_id == ECC_CURVE_NIST_P384`, else
   `-EINVAL` (`:130-135`); dispatches to `aspeed_ecdsa_verify_trigger`.
4. `aspeed_ecdsa_verify_trigger` (`:46-124`) — the register sequence (Phase 1
   target).

### 0.2 Interface authority (the consumer-enforced contract)

- **Curve: NIST P-384 only** (`ecdsa_aspeed.c:131`,
  `hrot_hal/.../ecdsa_aspeed.c:53`).
- **Digest: exactly 48 bytes** = SHA-384 output; `m_len = r_len = s_len = 48`
  enforced twice (`ecdsa_aspeed.c:130-131`, middlelayer `:48`).
- **Operation: verify only.** `query_hw_caps = NULL`, no sign, no keygen
  (`ecdsa_aspeed.c:185-189`).
- **Concurrency: strictly one in-flight**, single global state, `-EBUSY` on
  overlap (`ecdsa_aspeed.c:44,150-153,178`). Matches the openprot device
  layer's documented non-reentrant contract.
- Inputs are passed as raw 48-byte big-endian-ordered scalars copied 32 bits at
  a time into SRAM (`ecdsa_aspeed.c:84-102`).

### 0.3 §2-preview — observed authority-vs-`aspeed-rust` divergences

Recorded now as Phase 0 evidence that `aspeed-rust` is the buggy convenient
anchor; **formal classification deferred to §2 / Phase 3** (requires the
Phase 2 parity-standard decision and reachability traces).

| # | Authority `ecdsa_aspeed.c` @ cfe94dc | `aspeed-rust/src/ecdsa.rs` (informative) |
|---|--------------------------------------|------------------------------------------|
| P1 | Trigger write: `SEC_WR(2, ASPEED_ECDSA_CMD)` — literal value **2** (`:110`) | `secure0bc().sec_boot_ecceng_trigger_reg().set_bit()` — **bit 0** (`:349-350`) |
| P2 | Reset settle: `k_usleep(1000)` ≈ **1 ms** (`:59`) | `delay_ns(5000)` = **5 µs** (`:329`) |
| P3 | Post-trigger hold: `k_usleep(5000)` ≈ **5 ms** before clearing CMD (`:111-112`) | `delay_ns(5000)` = **5 µs** (`:351-354`) |
| P4 | Completion poll: **unbounded** `do { k_usleep(10); } while(!(sts & BIT(20)))` — **10 µs** interval, **no timeout** (`:114-117`) | bounded `retry=1000`, `delay_ns(5000)` 5 µs, `Err(Busy)` on exhaustion (`:357-368`) |
| P5 | Result: `BIT(20)` done then `BIT(21)` ⇒ pass, else fail (`:119-123`) | `secure014` `BIT(20)`/`BIT(21)` identical (`:359-365`) — conformant |

**Immediate consequence for code already written:**
`ecdsa/constants.rs::POLL_YIELD_NS = 5_000` was derived from the *informative*
`aspeed-rust` (5 µs). The authority's poll interval is **10 µs**
(`ecdsa_aspeed.c:115`) and its timeout model is **unbounded**. The device
layer's bounded-poll/typed-`Timeout` design is a deliberate deviation (same
shape as HACE delta D1) — it is not wrong, but it must be recorded as a delta
**against the Zephyr authority** in §2, and `POLL_YIELD_NS` should track the
authority's 10 µs, not aspeed-rust's 5 µs. Do not silently treat it as
"matching the reference."

---

## 1. Reference behavior to replicate  (Phase 1 — DONE)

Language-neutral behavioral spec, reverse-engineered from the frozen
[zephyr-reference/ecdsa_aspeed.c](zephyr-reference/ecdsa_aspeed.c) (sha256
`f09b350…`, == `git show cfe94dc:drivers/crypto/ecdsa_aspeed.c`) read directly.
Every claim is cited `ecdsa_aspeed.c:line`. Behavior only — the driver's C
struct layout (`aspeed_ecdsa_ctx`, `aspeed_ecdsa_drv_state`) is *not* a port
target. Unknowns are `OPEN`, never guessed.

### 1.1 Two address spaces

The engine has **two independent bases**, both latched from devicetree at init
(`ecdsa_init`, `:179-180`; DT idx 0 / idx 1, `:192-193`):

- **Engine MMIO** — `ecdsa_base`; accessed by `SEC_RD/SEC_WR(val, off) =
  sys_{read,write}32(ecdsa_base + off)` (`:28-29`). Register offsets used:
  `ASPEED_SEC_STS = 0x14` (status), `ASPEED_ECDSA_CTRL = 0xb4` (reset/enable),
  `ASPEED_ECDSA_CMD = 0xbc` (trigger), `0x7c` (mode/gate word — name not in
  source), and the curve-parameter source window `ASPEED_ECDSA_PAR_{GX=0xa00,
  GY=0xa40,P=0xa80,N=0xac0}` (`:18-24`).
- **Engine SRAM** — `sram_base`; accessed by `SRAM_WR(val, off) =
  sys_write32(val, sram_base + off)` (`:30`). Write-only in this driver (no
  `SRAM_RD`). SRAM is the engine's working area for operands and the
  instruction word.

All transfers are **32-bit word** `sys_write32`/`sys_read32`; every scalar copy
is `for (i = 0; i < 48; i += 4)` — 12 little-endian (ARM LE host) words in
**ascending offset order**. The driver performs **no byte-swap or endianness
transform** on any operand; operand byte-convention is entirely the caller's
contract (§0.2). (`:63-102`.)

### 1.2 Verify register sequence (`aspeed_ecdsa_verify_trigger`, `:46-124`)

Exact order — this *is* the hardware contract:

1. `SEC_WR(0x0100f00b, 0x7c)` — mode/gate word #1 (`:54`). Value & position
   normative; **semantics OPEN** (not explained in source).
2. **Reset engine:** `SEC_WR(0x0, 0xb4)` then `SEC_WR(0x1, 0xb4)`, then
   `k_usleep(1000)` ≈ **1 ms settle** (`:57-59`).
3. **Load P-384 domain parameters MMIO→SRAM** (`:61-80`), each a 48-byte /
   12-word copy `SRAM_WR(SEC_RD(PAR + i), dst + i)`:
   - Gx: `0xa00` → SRAM `0x2000` (`:63-64`)
   - Gy: `0xa40` → SRAM `0x2040` (`:67-68`)
   - p : `0xa80` → SRAM `0x2100` (`:71-72`)
   - n : `0xac0` → SRAM `0x2180` (`:75-76`)
   - a : **constant 0** written to SRAM `0x2140` (`SRAM_WR(0, …)`, `:79-80`).
     The driver writes zero for the curve `a` coefficient; **rationale OPEN**
     (P-384 has `a = -3 mod p`; the source neither computes nor justifies this
     — record the behavior, do not infer the reason).
   The domain parameters are **sourced from engine MMIO** (`0xa00…`), i.e. the
   hardware exposes G/p/n; whether that window is ROM-fixed is **OPEN** —
   normative behavior is *read-then-copy*, not the values.
4. `SEC_WR(0x0300f00b, 0x7c)` — mode/gate word #2 (`:82`). Semantics **OPEN**.
5. **Load caller operands SRAM** (each 48-byte / 12-word, `:84-102`):
   - Qx → SRAM `0x2080` (`:85-86`)
   - Qy → SRAM `0x20c0` (`:89-90`)
   - r  → SRAM `0x21c0` (`:93-94`)
   - s  → SRAM `0x2200` (`:97-98`)
   - m (the 48-byte SHA-384 digest) → SRAM `0x2240` (`:101-102`)
6. `SEC_WR(0x0, 0x7c)` — mode/gate word #3 = 0 (`:104`). Semantics **OPEN**.
7. `SRAM_WR(1, 0x23c0)` — write **instruction word `1`** to SRAM `0x23c0`
   (`:107`). Meaning of the opcode value **OPEN**; value & position normative.
8. **Trigger:** `SEC_WR(2, 0xbc)`, `k_usleep(5000)` ≈ **5 ms**, `SEC_WR(0,
   0xbc)` (`:110-112`). Trigger is the **literal value `2`** to
   `ASPEED_ECDSA_CMD` (not a bit-0 set — this is the P1 divergence vs.
   `aspeed-rust`, §0.3); the bit/field meaning is **OPEN**, the written value
   is normative. The 5 ms hold separates assert and de-assert.
9. **Completion wait — UNBOUNDED:**
   `do { k_usleep(10); sts = SEC_RD(0x14); } while (!(sts & BIT(20)));`
   (`:114-117`). Poll interval **10 µs**; status read **after** the sleep
   (sleep-then-read, minimum one 10 µs delay before the first read). **No
   timeout, no retry budget, no error exit** — a wedged engine hangs forever.
10. **Result decode:** re-read `ret = SEC_RD(0x14)` (`:119`); `ret & BIT(21)`
    ⇒ return `0` (signature valid); else return `-1` (signature invalid)
    (`:120-123`). Status bit 20 = operation-complete; bit 21 = verify-pass.
    Other bits of `0x14` are **unused / OPEN**. No engine teardown,
    status-clear, or `0x7c`/CMD reset is performed on the success or fail path
    (contrast HACE, which writes `hace30 = 0` to idle the engine).

### 1.3 Dispatch & validation state machine

- **Session open** `aspeed_ecdsa_session_setup` (`:142-164`): if
  `drv_state.in_use` → `-EBUSY` (`:150-153`); else set `in_use = true`,
  `memcpy` the caller key into the single global `drv_state.data.key`
  (`:155-158`), bind `ctx->ops.verify` and `ctx->device` (`:160-161`).
- **Verify dispatch** `aspeed_ecdsa_verify` (`:126-140`): reject unless
  `r_len == 48 && s_len == 48 && m_len == 48 && key->curve_id ==
  ECC_CURVE_NIST_P384` → `-EINVAL` (`:130-135`). Operands to the trigger:
  `m,r,s` from the **live `pkt`**, `qx,qy` from the **session-copied global
  key** (`:137-139`).
- **Session close** `aspeed_ecdsa_session_free` (`:166-174`): `in_use = false`
  (`:171`). No engine state is cleared on close.
- **Init** `ecdsa_init` (`:176-183`): `in_use = false`; latch `ecdsa_base` /
  `sram_base` from DT.

### 1.4 Memory / concurrency model

Single global `drv_state` (`NON_CACHED_BSS_ALIGN16`, `:44`); exactly **one
verify in flight** enforced by the `in_use` flag (`-EBUSY` on overlap). The key
is copied into the global at session open; `pkt` operands are read live during
the trigger. SRAM operand region is overwritten every verify (no persistence
assumed). Non-cached/16-aligned global ⇒ the engine DMAs/reads the SRAM and
status directly; coherency is handled by placement, not barriers.

### 1.5 Completion / error model (summary)

| Outcome | Mechanism | Cite |
|---------|-----------|------|
| Bad length / wrong curve | `-EINVAL` before any register write | `:130-135` |
| Concurrent use | `-EBUSY` at session open | `:150-153` |
| Signature valid | `0`, status `BIT(21)` set after `BIT(20)` | `:119-123` |
| Signature invalid | `-1`, `BIT(20)` set, `BIT(21)` clear | `:119-123` |
| Engine wedged | **none — unbounded hang** (no timeout path) | `:114-117` |

The absence of any timeout path (1.2 step 9) is the single most consequential
behavioral fact for the port: the openprot device layer's bounded
`poll_budget` → typed `SbcError::Timeout` is therefore a **deviation from
the authority's observable behavior**, to be classified in §2 (Phase 3) under
the Phase 2 parity standard — exactly the HACE-D1 situation.

### 1.6 OPEN items (carried into Phase 3 — not guessed)

- O1: Semantics of `0x7c` mode words `0x0100f00b` / `0x0300f00b` / `0x0`
  (values & ordering normative; meaning absent from source).
- O2: `ASPEED_ECDSA_CMD` trigger encoding — source writes literal `2`; field
  meaning unknown (drives the P1 divergence classification).
- O3: Curve-`a` written as `0` for P-384 (which has `a = -3 mod p`) — behavior
  normative, rationale absent from source.
- O4: SRAM instruction word `1` @ `0x23c0` — opcode meaning unknown.
- O5: Whether the `k_usleep` durations (1 ms / 5 ms / 10 µs) are required
  hardware minimums or conservative padding — source states only the values.
- O6: Engine MMIO curve-parameter window (`0xa00…0xac0`) provenance
  (ROM-fixed?) — read-then-copy behavior normative, source values unknown.
- O7: Status reg `0x14` bits other than 20/21 — unused by the driver.
- O8 (raised Phase 3): `clear_status()` body semantics — the authority
  performs no status-clear on reachable paths (`:119-123`); the façade op
  exists solely to service the D3 fault path and must not be emitted on the
  reachable valid/invalid paths. See §2.2.

## 2. Deltas vs. the authority  (Phase 3 — DONE)

Standard applied: **observable parity, keep fixes** (§Objective). Authority
behavior re-verified against the **frozen pinned** copy
[zephyr-reference/ecdsa_aspeed.c](zephyr-reference/ecdsa_aspeed.c) read
directly — *not* memory and *not* `aspeed-rust`. Classification ∈
{ **conformance** (port target == authority; not a delta) · **intentional
delta** (port deviates by decision, reachability-traced) · **out-of-scope by
decision** }.

Port "behavior" here means the port's **target** behavior (what Phase 5 must
implement). The façade ops `start_verify` / `verify_is_done` / `clear_status`
([registers.rs:85-106](../registers.rs)) are currently `todo!()` stubs, so
**no register-sequence delta physically exists yet**; rows D1/D2/D4 are
*conformance obligations on the Phase-5 implementation*. The only behavior
already committed in code is the wait policy (device.rs/op.rs/constants.rs).

| ID | Authority behavior (verbatim + frozen `file:line`) | Port target behavior | Classification |
|----|----------------------------------------------------|----------------------|----------------|
| **D1** | Trigger = **literal `2`** written to `ASPEED_ECDSA_CMD` (`0xbc`): `SEC_WR(2, ASPEED_ECDSA_CMD)` (`ecdsa_aspeed.c:110`) | `start_verify` must emit a 32-bit write of **value `2`** to engine offset `0xbc` | **Conformance (obligation).** Port target == authority. `aspeed-rust`'s `sec_boot_ecceng_trigger_reg().set_bit()` (bit-0 ⇒ value `1`) is the **rejected buggy informative form**. ⚠ See HZ1: the `ast1060_pac` `secure0bc` field accessor encodes the bit-0 form — the façade must write the raw value, not use that named field naively. |
| **D2** | Settle delays in the sequence: post-reset `k_usleep(1000)` ≈ **1 ms** (`:59`); post-trigger hold `k_usleep(5000)` ≈ **5 ms** before `SEC_WR(0,0xbc)` (`:111-112`) | `start_verify` must preserve the same ordering with delays **≥ the authority's** (1 ms after reset; ~5 ms trigger hold) | **Conformance (obligation).** Observable engine-driving timing. `aspeed-rust`'s `delay_ns(5000)` (5 µs) for both is the rejected buggy form. Delays are *advisory minimums* per O5 — Phase 5 reproduces the authority's, not aspeed-rust's. |
| **D3** | Completion wait is **unbounded**: `do { k_usleep(10); sts = SEC_RD(0x14); } while (!(sts & BIT(20)));` — 10 µs interval, **no timeout / no error exit**; a wedged engine hangs forever (`:114-117`) | Bounded `poll_budget` loop on the safe `verify_is_done()` predicate; on exhaustion → façade cleanup + typed `SbcError::Timeout` (`op.rs::wait_verify_done`); advisory yield = authority's **10 µs** | **THE LONE INTENTIONAL DELTA — keep the fix.** Reachability-traced & discharged below (§2.1). Identical to HACE-D1 in shape and justification. |
| **D4** | Result decode: after `BIT(20)`, `ret = SEC_RD(0x14)`; `ret & BIT(21)` ⇒ `0` (valid) else `-1` (invalid) (`:119-123`). **No engine teardown / status-clear on either reachable path.** | Port maps the same `BIT(20)`-then-`BIT(21)` decode to its verify result; `clear_status()` is invoked **only on the D3 timeout (fault) path**, never on the reachable valid/invalid paths | **Conformance.** Bit semantics identical. The extra `clear_status()` lives only on the unreachable wedged path (part of D3); on every reachable input the port performs the same status reads and no teardown, matching the authority. (`clear_status` body semantics: **OPEN — O8**, see §1.6.) |
| **D5** | Single monolithic `aspeed_ecdsa_verify_trigger` with inline unbounded poll (`:46-124`) | Port splits into Confined-`unsafe` façade ops + Cooperative-Yield Bounded-Poll device/adapter (façade/device/op layers) | **Conformance (architectural, non-observable).** "Observable parity" governs register transactions + verdict, not code shape. The split changes neither the emitted MMIO/SRAM sequence (D1/D2) nor the verdict (D4); only D3's bounded-vs-unbounded poll is observable, and that is the declared intentional delta. |
| **D6** | Consumer wires **verify only**, **P-384 only**, **48-byte SHA-384** operands; `query_hw_caps = NULL`; no sign/keygen (`ecdsa_aspeed.c:130-135,185-189`; middlelayer `:48-53`) | Port scope = P-384 verify only (§Objective, §0.2); no sign/keygen | **Out-of-scope by decision == conformance with the consumer contract.** Nothing the deployed consumer can reach is omitted. |

### 2.1 D3 reachability trace (discharge)

Delta discharged ⇔ authority lines read **and** real consumers traced.

- **Authority lines, frozen source, read directly:** the wait is
  `do { k_usleep(10); sts = SEC_RD(ASPEED_SEC_STS); } while (!(sts & BIT(20)));`
  (`zephyr-reference/ecdsa_aspeed.c:114-117`) — no counter, no timeout, no
  error return. The *only* behavioral divergence the bounded port introduces is
  on the path where `BIT(20)` is **never** asserted.

- **Consumer trace (real code, not speculation):**
  1. Application entry `aspeed-zephyr-project/lib/hrot_hal/crypto/ecdsa_aspeed.c::aspeed_ecdsa_verify_middlelayer`
     (`:34-72`) — **hard-rejects `length != 48`** before any engine call
     (`:48-51`); always P-384 (`ek.curve_id = ECC_CURVE_NIST_P384`, `:53`);
     `r/s/m` all set to the same 48 (`:59-61`).
  2. Driver dispatch `ecdsa_aspeed.c::aspeed_ecdsa_verify` (`:126-140`)
     **re-rejects** unless `r_len==s_len==m_len==48 && curve==P-384` →
     `-EINVAL` *before any register write* (`:130-135`).
  3. ⇒ Every input that reaches the engine is **exactly one P-384 verify with
     constant 48-byte operands and the single fixed instruction word `1`**
     (`ecdsa_aspeed.c:107`). There is **no input-size, iteration-count, or
     operand-value path that varies the engine's completion latency** — the
     verify is a fixed EC computation; nothing a consumer supplies selects
     whether `BIT(20)` asserts.

- **Conclusion:** for every *reachable* input the engine completes and both the
  unbounded authority and the bounded port read the identical `BIT(20)` →
  `BIT(21)` verdict at the same point — **observable parity holds on the entire
  reachable input space.** The sole divergence (`Timeout` vs. infinite hang)
  occurs only when the engine is genuinely wedged — a hardware-fault condition
  **not selected by any consumer input**. Keeping the bounded fix is strictly
  safer and changes no reachable observable. **Discharged.**

- **Residual obligation (honest limit, not hand-waved):** the trace proves
  *input-independence* of completion (no reachable input sits near a timeout
  boundary), **not** an absolute latency number. If `poll_budget` were set
  below the engine's true P-384-verify latency, a *correct* input could
  spuriously time out — a real observable-parity break. The authority gives no
  latency figure (only its own `k_usleep` values; O5/O6 OPEN). Therefore
  Phase 5/6 carries a binding obligation: **the chosen `poll_budget` (×
  advisory 10 µs) must be validated to exceed worst-case verify latency on
  QEMU/silicon** before D3 is considered closed for the done-criteria; it must
  not be assumed. This is the Cooperative-Yield pattern's documented tuning
  liability, made explicit here.

### 2.2 New OPEN / hazard items raised in Phase 3

- **HZ1 (PAC accessor hazard, D1):** `ast1060_pac`'s `secure0bc`
  `sec_boot_ecceng_trigger_reg()` field is the **bit-0** encoding inherited
  from the buggy `aspeed-rust` form; the authority writes the **literal value
  `2`** (`ecdsa_aspeed.c:110`). The Phase-5 façade `start_verify` must write
  the raw `0xbc` value `2` (confined `unsafe`), **not** call that named field
  setter. Recorded so the convenient PAC name does not silently re-introduce
  the P1 bug.
- **O8 (added to §1.6 scope):** `clear_status()` body semantics are
  unspecified by the authority (it performs *no* status-clear on reachable
  paths, `:119-123`); its Phase-5 implementation exists only to service the D3
  fault path and must **not** be emitted on the valid/invalid reachable paths.

### 2.3 The three authorities (Phase 4 — DONE)

Three independent obligations. **Never conflated** — each answers a different
question and is satisfied separately.

1. **Parity authority** — the pinned Zephyr `ecdsa_aspeed.c @ cfe94dc`
   ([zephyr-reference/](zephyr-reference/), §0–§2). Question: *"same as the
   deployed driver?"* Target = identical register transaction + verdict on
   every reachable input (§1.2). Sole intentional deviation = D3 (bounded
   timeout), discharged §2.1.

2. **Correctness authority — PINNED.** *Independent* published P-384 ECDSA
   known-answer tests: **NIST CAVP CAVS 11.0 FIPS 186-3 ECDSA `SigVer.rsp`,
   section `[P-384,SHA-384]`, all 15 records** (3 valid + 12 invalid:
   Message/R/S/Q changed). Vendored verbatim + pinned (URL, retrieval date,
   sha256 of full file and section) at
   `tests/peripherals/sbc/ecdsa/evb/nist-reference/` (`PINNED.txt`); the device
   table is generated to `evb/vectors.rs` (`Qx/Qy/R/S` verbatim NIST;
   `m = SHA-384(Msg)` computed at vendoring time, since NIST gives the raw
   message and the engine consumes the digest). Question: *"is the
   accept/reject verdict mathematically correct per the standard?"*
   Deliberately **non-overlapping** with parity (Zephyr) — these prove the
   verdict itself is correct, not equivalence to the deployed driver. The
   earlier "aspeed-rust embedded vectors" were rejected as the authority
   (informative ref, half hand-mutated) — normative-over-convenient at the
   vector level.

3. **Interface authority** — `EcdsaVerify<P384>`
   ([../../../../../hal/blocking/src/ecdsa.rs](../../../../../hal/blocking/src/ecdsa.rs)).
   *Verify-the-mandate conclusion, recorded verbatim:* the trait mandates
   **shape only** — a single
   `verify(&P384PublicKey, digest, &P384Signature) -> Result<(), Self::Error>`.
   It does **not** mandate the ECDSA algorithm, and after the 2026-05-16 HAL
   cleanup (ADR-2, §5) it **enforces only a structural input check**
   (`from_coordinates` rejects the all-zero `r`/`s`/point) — **not** full
   `1 ≤ r,s < n` or on-curve validation. ⇒ **Signature/point mathematical
   validity is the port's responsibility, not a trait guarantee.** Same
   conclusion shape as HACE ("openprot mandates *shape*, not *algorithm*").
   The port must not assume the trait layer validated anything beyond non-zero.

## 3. Implementation plan  (Phase 5 — DONE; this is the plan, not the code)

Each item ends with `Acceptance:`. Grounded in §1.2 / §2 / ADR-1. The
Cooperative-Yield bounded-poll device/adapter (`device.rs`, `op.rs`) **already
exists** from the prior structural task — Phase 5 *wires* it, does not rebuild
it (see item 5; the old "implement bounded poll" work is struck as done).

**P5-OPEN — both resolved by decision 2026-05-16 (human-owned). Caveats kept,
not buried.**

- **A. Where is the engine's scratch memory (SRAM)? — DECIDED: use
  `0x7900_0000`.** The ECDSA engine copies all its operands into a block of
  scratch RAM and we need its start address on AST1060. The normative Zephyr
  driver does *not* hardcode it — it reads it from the board device-tree at
  runtime (`ecdsa_aspeed.c:180,193`), so there is no normative *constant* to
  copy. The only concrete number available is the **informative** `aspeed-rust`
  hardcode `SBC_SRAM_BASE = 0x7900_0000`. **Decision: adopt `0x7900_0000`**
  as a named façade constant.
  *Status — CORROBORATED (address), behavioral validation HW-deferred.* The
  value is **independently corroborated** by the QEMU AST1060 SoC memmap:
  `qemu-ast10x0-i2c/hw/arm/aspeed_ast10x0.c:24` maps
  `ASPEED_DEV_SECSRAM = 0x79000000` and the machine backs it — an independent
  source agreeing with the `aspeed-rust` hardcode, so the *address* is no
  longer "unproven". The earlier "validated by the Phase-6 KAT run" claim is
  **withdrawn**: QEMU does not model the ECC engine (see ADR-4 / §4), so no
  KAT run on QEMU can exercise a verdict — a wrong base would *not* surface as
  a mismatch there. Behavioral confirmation of the base (operands actually
  consumed correctly) is therefore **hardware-only, deferred to the
  user-executed silicon KAT**. If a SoC/PAC datasheet later states otherwise,
  that source wins; single `pub const`, one fix point.

- **B. PAC coverage & where raw access lives — DECIDED: confine it in the
  façade.** Confirmed approach: the mode register at offset `0x7c` (and
  likewise the curve-parameter window `0xa00–0xac0` and the scratch-RAM
  region, none modelled by `ast1060_pac`) are accessed by **raw memory offset
  inside the single audited `unsafe` layer of the façade** — the
  Confined-`unsafe` MMIO Façade pattern, no raw access above it. One factual
  check still owed during item 1 (not a decision): confirm the PAC *does* name
  `secure014`/`secure0b4`/`secure0bc` so only the genuinely-unmodelled offsets
  fall to raw access, keeping the `unsafe` surface minimal and enumerated.

1. **Façade surface (`registers.rs`).** Replace the 3 coarse stubs with the
   confined-`unsafe` op set §1.2 actually needs: a constructor that also
   carries the SRAM base (P5-OPEN-A); `start_verify(qx,qy,r,s,m: &[u8;48])`
   (full pre-trigger sequence, item 2); `verify_is_done() -> bool`
   (`secure014` bit-20); `verify_passed() -> bool` (`secure014` bit-21);
   `clear_status()` (fault-path only, O8). All raw `.bits()`/offsets stay
   confined below the façade; no PAC type escapes.
   `Acceptance:` builds; façade exposes exactly these safe ops; the SRAM/param
   raw accesses are isolated to ≤1 audited `unsafe` block each with a SAFETY
   note; P5-OPEN-A/B resolved & cited in the module doc.

2. **Pre-trigger sequence in `start_verify`** — reproduce §1.2 steps 1–8
   **in exact order**: `0x7c←0x0100f00b`; reset `0xb4←0,1` + 1 ms settle (D2);
   P-384 G/p/n MMIO→SRAM + `a=0` (§1.2.3); `0x7c←0x0300f00b`; Qx/Qy/r/s/m→SRAM
   (12 ascending LE words, **no byte-swap**, §1.1); `0x7c←0`; instruction
   word `1`@SRAM `0x23c0`; trigger **raw `0xbc←2`** then 5 ms hold then
   `0xbc←0` (**D1 + HZ1: literal `2`, NOT the PAC `sec_boot_ecceng_trigger_reg`
   bit-0 accessor**). Delays use the injected yield/`delay`, ≥ authority
   values (D2; aspeed-rust's 5 µs rejected).
   `Acceptance:` emitted MMIO/SRAM transaction is byte-order- and
   sequence-identical to `zephyr-reference/ecdsa_aspeed.c:54-112` (cross-check
   each line); no `sec_boot_ecceng_trigger_reg()` call anywhere (HZ1 guard).

3. **Completion + result decode.** `verify_is_done()` = `secure014 & (1<<20)`;
   `verify_passed()` = `secure014 & (1<<21)` (§1.2 steps 9–10). **No engine
   teardown / status-clear on the reachable valid/invalid paths** (authority
   does none, `:119-123`); `clear_status()` is invoked **only** on the D3
   timeout path (O8).
   `Acceptance:` valid-vector ⇒ `done && passed`; invalid-vector ⇒
   `done && !passed`; neither path calls `clear_status()`.

4. **Reconcile the wait policy (`constants.rs`).** `POLL_YIELD_NS`
   **5_000 → 10_000** (authority `k_usleep(10)` = 10 µs, §1.2 step 9; the old
   5 µs was the rejected aspeed-rust value). Keep `DEFAULT_POLL_BUDGET`
   generous; document it as a **tuning obligation** discharged only by the
   Phase-6 latency validation (§2.1 residual).
   `Acceptance:` `POLL_YIELD_NS == 10_000` with a doc line citing
   `ecdsa_aspeed.c:115`; budget-tuning obligation noted in `constants.rs`.

5. **Wire the bounded-poll op (`op.rs`).** Point `wait_verify_done()` at the
   real `verify_is_done()`; add result decode (`verify_passed()`); compose the
   internal `verify(qx,qy,r,s,m) -> Result<(), SbcError>`:
   `start_verify` → bounded poll → `Ok(())` if passed / `Err(VerificationFailed)`
   if done-but-not-passed / `Err(Timeout)`+`clear_status()` on budget
   exhaustion (D3). Add `SbcError::VerificationFailed` (enum is
   `#[non_exhaustive]`; `Timeout` already present). Length/curve are guarded
   exactly as the authority (§1.3) — **no `r,s<n`/on-curve pre-check added**
   (observable parity: the authority does none; the engine produces the
   verdict).
   `Acceptance:` internal `verify` is callable **without the trait**; returns
   the three outcomes correctly against valid/invalid/wedged inputs.

6. **Thin skin (`hal_impl.rs`, new) — ADR-1.** Implement `EcdsaVerify<P384>`
   in its own module: extract operands from `&P384PublicKey`/`&P384Signature`
   via `zerocopy::IntoBytes::as_bytes()` (`#[repr(C)]` halves; the §4
   operand-order test pins this), take the SHA-384 digest bytes, call the
   internal `verify` (item 5), map `SbcError` → `Self::Error`/`Error::kind()`.
   No driver type becomes generic over or shaped by the trait.
   `Acceptance:` impl is ≤ ~20 lines of pure boundary translation; deleting
   `hal_impl.rs` leaves the internal driver fully compiling and usable
   (ADR-1 regression check); `mod.rs`/BUILD updated.

7. ~~Implement the cooperative-yield bounded-poll device/adapter.~~ **Struck —
   already delivered** by the prior structural task (`device.rs`/`op.rs`);
   Phase 5 only wires it (item 5).

## 4. Done criteria

**Split decided 2026-05-16 (human-owned fork): QEMU has no ECC-engine model
(ADR-4), so the verdict-bearing gates are HARDWARE-ONLY and the user executes
them later on silicon.** The three §2.3 authorities are gated as follows.

### 4.A QEMU-feasible (automatable now, `TODO (Phase 6)` if/when built)
- **Interface/structural:** the port (not the trait) rejects malformed
  `r/s`/point — pure Rust, no engine (§2.3.3).
- **Operand-order pin (mandatory):** test asserting `#[repr(C)]` field order
  of `P384PublicKey`/`P384Signature` ↔ SRAM operand layout (§1.2 step 5), so
  a future HAL struct refactor cannot silently break parity via `as_bytes()`.
- **D3 delta (positive test):** on QEMU the ECC engine is absent so
  `secure014` bit-20 never sets ⇒ `verify_raw` deterministically returns
  `SbcError::Timeout`. Assert exactly that — it is a real positive test of
  the bounded-poll/timeout path (the lone intentional delta), and the *only*
  end-to-end behavior QEMU can exercise.

### 4.B HARDWARE-ONLY — IMPLEMENTED, user-executed on silicon (not QEMU)
> `tests/peripherals/sbc/ecdsa/evb/` — pw_kernel firmware, `hardware`-tagged +
> `qemu_enabled`-incompatible (excluded from `--config=virt_ast10x0`/CI per
> ADR-4). Builds clean (`--config=k_ast1060_evb`); the **user runs it on an
> AST1060 EVB**. One run satisfies all three of:
- **Correctness (HW):** all 15 pinned NIST CAVP P-384/SHA-384 `SigVer`
  verdicts match (§2.3.2) — the independent correctness authority.
- **Parity (HW):** the same accept/reject outcomes are what the pinned Zephyr
  driver's register sequence (§1.2) produces on these inputs — verdict-level
  parity confirmation (the by-construction sequence, now exercised).
- **P5-OPEN-A + budget:** a green run proves the SRAM base is actually
  consumed correctly and the poll budget exceeds real verify latency
  (§2.1 residual); a wrong base / wedge surfaces as an `engine timeout` line.

Until 4.B is run on hardware, behavioral parity remains **by-construction
only** (cited line-for-line vs `zephyr-reference/`); this is stated, not
hidden.

**Status** (`target/ast10x0/tests/peripherals/sbc/ecdsa/`):
- **`qemu/` (4.A) — IMPLEMENTED & PASSING.** pw_kernel `system_image_test`,
  `qemu_only`; green under `--config=virt_ast10x0`: operand-order `#[repr(C)]`
  pin, structural reject, and the D3 bounded-timeout positive test
  (`verify_raw` → `SbcError::Timeout`, deterministic since QEMU has no ECC
  engine per ADR-4). `TEST_RESULT:PASS`.
- **`evb/` (4.B) — scaffold only**, tagged `hardware` + `qemu_enabled`-
  incompatible (excluded from `--config=virt_ast10x0`/CI). NIST KAT / verdict
  parity bodies are the user's to write & run on AST1060 silicon; that run
  also discharges P5-OPEN-A behaviorally + the budget tuning.

## 5. Architecture decisions

### ADR-1 — The interface trait is a *skin*, not the skeleton
The port is architected against the **behavioral/parity authority** (§1) — the
Confined-`unsafe` façade → Cooperative-Yield bounded-poll device. The
`EcdsaVerify<P384>` impl is an **outer adapter layer only**: a dedicated thin
boundary module that translates trait types ↔ the port's own plain types
(byte arrays, `SbcError`), calls the port's internal verify, and maps the
error via `Error::kind()`. The internal driver MUST be fully usable **without
the trait** and MUST NOT be generic over or shaped by it (mirrors HACE:
`digest.rs` wears the digest traits *over* the device; the device is not built
from them). **Regression signal:** if a trait change forces an edit in
`registers.rs`/`device.rs`/`op.rs`, the skin has fused to the skeleton — that
is a defect, not an expected ripple. Phase 5 must place the impl in its own
`hal_impl`-style module, not in the device/façade.

### ADR-2 — HAL `ecdsa` trait cleanup (2026-05-16)
[`hal/blocking/src/ecdsa.rs`](../../../../../hal/blocking/src/ecdsa.rs) reduced
1102→287 lines (assessment:
[interface-authority-assessment.md](interface-authority-assessment.md)).
Removed (zero real consumers, verified — the one `P256` grep hit was a
`typenum` build artifact): `generate_and_store_keypair` + its `key_vault`
coupling; 6 decorative curve markers (kept only `P384`, the sole curve with
concrete types). Fixed: return-by-value coordinate accessors; honest contracts
(only the checkable non-zero invariant is promised/enforced — `r,s<n` /
on-curve declared out of scope, since `Curve` exposes no domain params);
single documented error rule (data→`ErrorKind`, ops→`Self::Error`); doc/code
drift. **Rationale:** make the skin (ADR-1) genuinely thin and the validation
contract honest, so the port inherits no false guarantee (feeds §2.3.3).
Non-breaking: builds clean (`//hal/blocking:blocking`,
`//target/ast10x0/peripherals:peripherals`), no in-tree consumers. Fixing the
*remaining* HAL design questions is outside this port's parity scope.

### ADR-4 — QEMU does not model the ECC engine (validation constraint)
The AST1060 QEMU SBC model
[`qemu-ast10x0-i2c/hw/misc/aspeed_sbc.c`](../../../../../../qemu-ast10x0-i2c/hw/misc/aspeed_sbc.c)
is an **OTP / secure-boot-status stub only**. Grounded:
- `R_STATUS` (0x14) is read-only and carries OTP/ABR bits 0–14 only —
  **bit-20 (done) / bit-21 (pass) are never set** (`aspeed_sbc.c:28-43,230`).
- Writes to `0x7c`/`0xb4`/`0xbc` (mode/ctrl/**trigger**) fall through to
  generic `s->regs[addr]=data` — the trigger does nothing (`:229-244`).
- `ASPEED_SBC_NR_REGS = 0x93c>>2` (`include/hw/misc/aspeed_sbc.h:20`) ⇒ the
  P-384 param window `0xa00–0xacf` is out of range; reads return 0 (`:69-74`).

**Consequence:** running `verify_raw` on QEMU stores/ignores the register
writes and the bounded poll spins to budget → deterministic
`SbcError::Timeout`. The accept/reject **verdict is unreachable on QEMU**;
behavioral parity & NIST-KAT correctness are **hardware-only** (§4.B). The
SRAM base `0x7900_0000` is independently corroborated by the same QEMU SoC
memmap (`aspeed_ast10x0.c:24`, `ASPEED_DEV_SECSRAM`) — address confirmed,
behavior not (P5-OPEN-A). This is a Phase-7 *read-the-emulator-device-model*
finding that reshaped Phase 6, not a code defect.

### ADR-5 — One SBC engine: `peripherals/ecdsa` → `peripherals/sbc` (2026-05-16)
ECDSA and RSA are **not separate peripherals** — they are two operations of
one hardware block. Grounded in the pinned authority `rsa_aspeed.c @ cfe94dc`
vs `ecdsa_aspeed.c @ cfe94dc`: both drive the same `secure` block
(`0x7e6f_2000`) via identical `SEC_RD/SEC_WR`, the same scratch SECSRAM
(`0x7900_0000`), the **same trigger register `0xbc`** (RSA writes `1`,
`rsa_aspeed.c:76`; ECDSA writes `2`, `ecdsa_aspeed.c:110`), and the **same
status register `0x14`** (RSA polls `BIT(4)` `rsa_aspeed.c:81`; ECDSA
`BIT(20/21)`); Kconfig `ECDSA_ASPEED select RSA_ASPEED`.

Decision: model the block as **one** Confined-`unsafe` MMIO façade.
`peripherals/ecdsa` → **`peripherals/sbc`** (`git mv`, history preserved);
engine-level types renamed `Ecdsa{Registers,Device,Op,Error}` →
`Sbc{Registers,Device,Op,Error}`, `ECDSA_SRAM_BASE` → `SBC_SRAM_BASE`. The
shared façade/device/op (cooperative-yield bounded-poll) is common; the
ECDSA-verify operation (`verify_raw`, the `EcdsaVerify<P384>` `hal_impl`
skin) and the future RSA operation are op-specific layers on top. Rejected:
a second façade over the same block (duplicated `unsafe`, no shared
non-reentrant/exclusivity contract — violates the Confined-`unsafe` MMIO
Façade "one audited site per block" principle). Tests moved
`tests/peripherals/ecdsa` → `tests/peripherals/sbc/ecdsa` (RSA tests will be
`tests/peripherals/sbc/rsa`).

**RSA is its own peripheral-parity-port:** separate goal doc under
`plans/` (authority = pinned `rsa_aspeed.c`; correctness = NIST RSA KAT),
**not** folded into this ECDSA goal. This document remains the ECDSA-operation
parity record.

### ADR-3 — SW/HW selection & isolation
`TODO (Phase 8)` — capture the SW/HW-selection and isolation constraints
(cf. HACE goal §5) once the verify path is specified.
