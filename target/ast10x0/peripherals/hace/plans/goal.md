# HACE Behavioral Parity Goal (AST1060)

## Objective

The `ast10x0/peripherals/hace` port must reach **total behavioral parity** with the
**authoritative model**: the upstream Zephyr `aspeed_hace` driver — `hace_aspeed.c`
+ `hace_aspeed.h` + `hash_aspeed_priv.h` — from the AspeedTech-BMC Zephyr fork,
pinned at the exact `aspeed-zephyr-project/west.yml` revision
`zephyr @ cfe94dc149ffa0af7e1af668a27f57eecf0cd1e9`. A frozen verbatim copy of those
three files is the normative artifact at
[zephyr-reference/](zephyr-reference/) (see `zephyr-reference/PINNED_COMMIT.txt`);
all behavior claims below are grounded in that copy, read directly — not inferred
from `aspeed-rust`. This is the normative reference because it is the model the
deployed AST1060 firmware actually runs. See [zephyr-behavior.md](zephyr-behavior.md)
for the consumer/streaming analysis, and
[crypto-consumers.md](crypto-consumers.md) for the cross-block consumer / use-case
matrix (HACE + SBC).

`aspeed-rust/src` (`hace_controller.rs`, `hash.rs`, `hash_owned.rs`, `hmac.rs`) is
**informative, not normative** — a second port of the same hardware, useful for
cross-checking register sequencing, but *not* the spec. Where `aspeed-rust` diverges
from the pinned Zephyr driver, the Zephyr driver wins and `aspeed-rust` is treated as
buggy.

Parity standard (decided): **observable byte-for-byte parity on every reachable
input** — the port reproduces the pinned Zephyr driver's output exactly for every
input any real consumer can produce (see [zephyr-behavior.md](zephyr-behavior.md)
consumer analysis), explicitly including the production-dominant 4 KB-aligned
streaming path. Where the Zephyr driver has a latent defect that no reachable input
triggers, the port keeps the safer/correct behavior; that single divergence is
recorded as the lone *intentional delta* (D2). Everything else is strict identity.

Scope is AST1060 only (little-endian Cortex-M). No other target is considered.

**AES (symmetric crypto) scope — added 2026-05-16.** The HACE engine is also a
symmetric block-cipher engine; the *same* pinned driver file is unified hash +
crypto. The AES authority is therefore the **same commit**, `hace_aspeed.c` @
`cfe94dc`, plus `hace_aspeed.h` and the now-vendored `crypto_aspeed_priv.h`
(crypto session context) — all in [zephyr-reference/](zephyr-reference/), read
directly. No informative second port exists for AES (`aspeed-rust` has no AES),
so the pinned driver is the sole reference. Decided forks (Phase 2):

- **Parity standard: same as digest** — observable byte-for-byte parity on
  every reachable input; keep correctness fixes for latent defects no reachable
  consumer triggers, recorded as intentional deltas.
- **Surface in scope: AES-128 / AES-256, ECB and CBC, raw key; plus the
  OTP/secret-vault *select logic* (delta A6 — handle→register/cmd, mock-tested;
  vault crypto E2E separated, §2.6).** Out of scope **by decision** (recorded
  in §2.3, not silently dropped): CFB/OFB/CTR, AES-192, DES/3DES.
- AES correctness authority is independent published **NIST AESAVS/CAVP KATs**
  (§2.4) — distinct from parity, exactly as RFC-4231 is for HMAC.

---

## 1. Reference behavior to replicate

### 1.1 Hardware contract

A hash pass is driven through six registers in this exact order:

1. `hace1c` — write `hash_intflag = 1` to clear the completion latch.
2. `hace20` — source address: SG-list pointer when `method & HACE_SG_EN`, else the
   linear `buffer` pointer.
3. `hace24` — digest in/out address (`ctx.digest`).
4. `hace28` — written with the **same digest address** as `hace24`.
5. `hace2c` — byte length of this pass.
6. `hace30` — command word (`method`); this write starts the engine.

Completion = `hace1c.hash_intflag` re-asserts. Cleanup/idle = write `hace30 = 0`.

### 1.2 Command word

`method = HACE_CMD_ACC_MODE(1<<8) | HACE_SHA_BE_EN(1<<3) | HACE_SG_EN(1<<18) | algo_bits`.

- ACC_MODE always set: digest buffer is live chaining state (engine reads it in,
  updates, writes back). This is what makes multi-call streaming work.
- SHA_BE_EN: digest emitted big-endian.
- SG_EN: source is the 2-entry SG descriptor list; `HACE_SG_LAST(1<<31)` OR'd into the
  final descriptor's length terminates it. **HMAC clears SG_EN** and uses the linear
  buffer — the `hace20` source-select branch is mandatory.
- Algorithm bit patterns (port the full table from `hace_controller.rs`):
  SHA1, SHA224, SHA256, SHA384, SHA512, SHA512_224, SHA512_256. Only SHA-256/384/512
  are reachable through aspeed-rust's public API and are the parity requirement;
  the others are defined but unwired in aspeed-rust and are out of scope.

### 1.3 Memory model

Single global `HashContext`, `#[repr(C, align(64))]`, in `.ram_nc`. SG descriptors,
`buffer`, and `digest` are DMA targets. Exactly one operation in flight at any time
across all API surfaces. The single `.ram_nc` static is the faithful analogue of
Zephyr's per-symbol non-cacheable placement: `NON_CACHED_BSS_ALIGN16` is a linker
section attribute on one driver-owned symbol, **not** a sub-allocation of a
partitioned non-cacheable arena (no per-device carve-out exists) —
[zephyr-behavior.md](zephyr-behavior.md) §4.3.

### 1.4 IV / endianness (AST1060)

- `copy_iv_to_digest` / `load_iv`: each IV `u32` written in **native (little-endian)**
  byte order into `digest`, length `iv.len() * 4` bytes. SHA-256 → 32 bytes;
  SHA-384/512 → 64 bytes (full digest buffer). Parity already holds
  (`to_ne_bytes` == aspeed-rust's pointer-cast on LE).
- Digest readback: the canonical observable output is the **standard digest byte
  string** as produced by aspeed-rust's scoped API and asserted by `hash_test.rs`.
  Port must yield this. Use `from_ne_bytes` packing so `Digest::as_bytes()` round-trips
  to the standard digest (see Delta D4).

### 1.5 Digest (hash) state machine

- **init**: set `method`; load IV into `digest`; set `block_size`; zero `bufcnt`,
  `digcnt`. Does **not** clear `buffer`/`digest` (scoped semantics).
- **update**:
  - Add `input.len()` into 128-bit counter (`digcnt[0]`, carry → `digcnt[1]`).
  - If `bufcnt + len < block_size`: append to `buffer`, return (no engine pass).
  - Else: build SG list of whole blocks only (sg[0] = buffered partial, sg[1] =
    block-aligned input span, last entry `HACE_SG_LAST`); one engine pass over the
    block-aligned `total_len`; copy tail (`len % block_size`) back to `buffer[0..]`.
- **finalize**: `fill_padding(0)`; single `HACE_SG_LAST` descriptor over the padded
  buffer; final pass; extract `digest_size` bytes; cleanup (zero `bufcnt`, `digcnt`,
  `buffer`, `digest`; `hace30 = 0`).

### 1.6 Padding (`fill_padding`)

Merkle–Damgård: write `0x80`, zero-fill to the 56-byte (block 64) / 112-byte
(block 128) boundary, then the message **bit-length** big-endian as an 8-byte field
(block 64) or 16-byte field (block 128, `high = (digcnt[1]<<3)|(digcnt[0]>>61)`,
`low = digcnt[0]<<3`). Already correct in `helpers.rs`; reused unchanged.

### 1.7 HMAC — NOT streaming  *(authoritative: `aspeed_hash_setkey` / `aspeed_hash_digest_hmac`)*

Grounded in the pinned driver, **not** `aspeed-rust/hmac.rs`. HMAC parity is
governed by a separate declared authority (§2.1) — this is the behavioral record.
**The port does not implement this engine-native path**: §3 item 2 implements HMAC
in software (RFC-2104 over the port's hasher). This subsection is a cross-reference
to confirm the chosen threshold/XOR match the driver's RFC-2104-correct behavior,
not an implementation target.

- **setkey** (`aspeed_hash_setkey`): if `key_len > block_size` (**RFC-2104-correct,
  algorithm-dependent: 64 for SHA-256, 128 for SHA-384/512** — *not* a flat 128)
  reduce the key via `aspeed_hash_digest` (one-shot H(K)), else copy the raw key
  into `key_buff`. Then zero-pad `key_buff` to `block_size`; set
  `ipad = opad = key_buff`; XOR the **full `block_size`** of `ipad` with `0x36`,
  `opad` with `0x5c`.
- **key reduction** (`aspeed_hash_digest`): clear `HACE_SG_EN` (Direct Access /
  linear `buffer`); load IV; single padded linear pass over the key;
  `key_buff = digest`; `key_len = digest_size`.
- **update** (one-shot, computes full HMAC of *this* input; last call wins):
  1. `digcnt[0] = block_size`; `bufcnt = block_size`; `buffer = ipad ‖ input`;
     `digcnt[0] += input.len()`; `bufcnt += input.len()`; clear `HACE_SG_EN`.
  2. `fill_padding(0)`; `copy_iv_to_digest`; engine pass → inner digest.
  3. `digcnt[0] = block_size + digest_size`; `bufcnt = block_size + digest_size`;
     `buffer = opad ‖ inner`; `fill_padding(0)`; `copy_iv_to_digest`; engine pass.
- **finalize**: read `digest_size` bytes; cleanup.

### 1.8 Owned move-based API (`hash_owned.rs`)

Same digest behavior as scoped, plus: `init` consumes the controller and returns an
owned context; `finalize` returns `(digest, controller)`; `cancel` cleans up and
returns the controller. On an exact block boundary the owned path sets `bufcnt = 0`
(see Delta D2 — the port adopts this for the scoped path too).

### 1.9 AES (symmetric crypto) reference behavior

All claims grounded in the frozen [zephyr-reference/](zephyr-reference/) copy
@ `cfe94dc`, read directly: `hace_aspeed.c` (unified hash+crypto driver),
`hace_aspeed.h` (crypto register/command bits), `crypto_aspeed_priv.h`
(session context). The crypto path is a **distinct hardware sub-engine** from
the hash path: different registers, different status bit, its own context — but
the same physical HACE engine and the same single-in-flight constraint (§5.1).

#### 1.9.1 Hardware contract (crypto sub-engine)

A crypto pass is driven via the crypto register file (`hace_aspeed.h`, crypto
control regs from `ASPEED_HACE_CMD 0x10`; `hace_aspeed.h:15`). Sequence per
`crypto_trigger` (`hace_aspeed.c:78–103`), in this exact order:

1. If `hace_sts.crypto_engine_sts` set → engine busy, return `-EBUSY`
   (`hace_aspeed.c:83–87`). *(This is the crypto analogue of the hash
   `in_use`/`-EBUSY`; see §2.3-A1 / §5.1.)*
2. `hace_sts = HACE_CRYPTO_ISR` (`BIT(12)`) — clear the completion latch
   (`hace_aspeed.c:88`; `hace_aspeed.h:39`).
3. `crypto_data_src = &src_sg` (`hace_aspeed.c:90`).
4. `crypto_data_dst = &dst_sg` (`hace_aspeed.c:91`).
5. `crypto_ctx_base = data->ctx` (`hace_aspeed.c:92`).
6. `crypto_data_len = src_sg.len` (`hace_aspeed.c:93`).
7. `crypto_cmd_reg = data->cmd` — **this write starts the engine**
   (`hace_aspeed.c:94`).

Completion = poll `hace_sts.crypto_int == 1`, bounded `timeout_ms = 3000`
(`aspeed_crypto_wait_completion`, `hace_aspeed.c:63–76`, called at
`hace_aspeed.c:102`); timeout → error return (no partial output). After a
successful pass the driver issues `cache_data_invd_all()`
(`hace_aspeed.c:140`) — DMA coherency for `dst`.

#### 1.9.2 Command word (`data->cmd`)

Composed in `aspeed_crypto_session_setup` (`hace_aspeed.c:242–365`). Base:
`HACE_CMD_DES_SG_CTRL | HACE_CMD_SRC_SG_CTRL | HACE_CMD_MBUS_REQ_SYNC_EN`
(`hace_aspeed.c:264`; bits `BIT(19)|BIT(18)|BIT(20)`, `hace_aspeed.h:18,19,17`).
For `CRYPTO_CIPHER_ALGO_AES`: `|= HACE_CMD_AES_KEY_HW_EXP | HACE_CMD_AES_SELECT`
(`hace_aspeed.c:269`; `BIT(13)` hardware key expansion, `AES_SELECT == 0`,
`hace_aspeed.h:25,22`). Key length (`hace_aspeed.c:289–301`): `keylen` 16 →
`HACE_CMD_AES128 (0)`, 24 → `HACE_CMD_AES192 (1<<2)`, 32 →
`HACE_CMD_AES256 (2<<2)`, else `-EINVAL` (`hace_aspeed.h:34–36`). Mode
(`hace_aspeed.c:303–356`): `HACE_CMD_ECB (0)` / `HACE_CMD_CBC (1<<4)`
(`hace_aspeed.h:29,30`). Direction: encrypt `|= HACE_CMD_ENCRYPT (BIT(7))`,
decrypt `|= HACE_CMD_DECRYPT (0)` (`hace_aspeed.h:28,27`). `cmd` is fixed at
session-setup time and reused for every block op of the session.

#### 1.9.3 Session context & memory model

`struct aspeed_crypto_ctx { uint8_t ctx[64]; struct aspeed_sg src_sg;
struct aspeed_sg dst_sg; uint32_t cmd; }` (`crypto_aspeed_priv.h:20–24`); held
in `aspeed_crypto_drv_state { data; bool in_use; }` (`:26–29`). Layout of the
64-byte `ctx`:

- `ctx[0..16)` — the **IV** (CBC only; `memcpy(data->ctx, iv, 16)`,
  `hace_aspeed.c:186,200`). ECB never writes it.
- `ctx[16..16+keylen)` — the **raw key**
  (`memcpy(data->ctx + 16, ctx->key.bit_stream, ctx->keylen)`,
  `hace_aspeed.c:114`).

`src_sg`/`dst_sg`/`ctx` are DMA targets handed to the engine by physical
address (`hace_aspeed.c:90–92`); `src_sg.len` and `dst_sg.len` are the byte
length **OR'd with `BIT(31)`** (single/last SG entry terminator,
`hace_aspeed.c:130–133`). Exactly one crypto op in flight (§5.1).

#### 1.9.4 Operation state machine (one-shot, per block call)

There is **no init/update/finalize streaming** for crypto: a session is set up
once, then each `cipher_pkt` is a complete run-to-completion op.

- **session setup** (`aspeed_crypto_session_setup`, `hace_aspeed.c:242–365`):
  reject if `state->in_use` → `-EBUSY` (`:254–256`); require `CAP_SYNC_OPS`,
  else `-EINVAL` (async unsupported, `:259–262`); compose `cmd` (§1.9.2);
  bind the ECB/CBC op handler; `state->in_use = true` (`:363`);
  `ctx->ops.cipher_mode = mode` (`:361`).
- **core crypt** (`aspeed_aes_crypt`, `hace_aspeed.c:105–141`): copy raw key to
  `ctx[16..]` (`:114`); `src_sg.addr = in_buf`, `dst_sg.addr = out_buf`,
  `src_sg.len = dst_sg.len = in_len | BIT(31)` (`:130–133`);
  `crypto_trigger` (§1.9.1); on success `cache_data_invd_all()` (`:140`),
  return 0. **No input-length / block-multiple validation** in the driver —
  the engine consumes `in_len` bytes as given (relied-upon, see §2.3-A4).
- **ECB** (`aspeed_aes_crypt_ecb`, `hace_aspeed.c:170–178`):
  `pkt->out_len = pkt->in_len`; crypt `in_buf → out_buf`. No IV.
- **CBC encrypt** (`aspeed_aes_encrypt_cbc`, `hace_aspeed.c:180–192`):
  `memcpy(ctx, iv, 16)`; **`memcpy(pkt->out_buf, iv, 16)`** — the IV is
  emitted in-band as the first 16 output bytes; `pkt->out_len = in_len + 16`;
  ciphertext written at `out_buf + 16`. **Observable output = `IV ‖ CT`.**
- **CBC decrypt** (`aspeed_aes_decrypt_cbc`, `hace_aspeed.c:194–205`):
  `memcpy(ctx, iv, 16)`; `pkt->out_len = in_len - 16`; the engine consumes
  `in_buf + 16` for `in_len - 16` bytes. **Expected input = `IV ‖ CT`**; the
  leading 16 bytes are skipped, not decrypted.
- **session free** (`aspeed_crypto_session_free`, `hace_aspeed.c:370–377`):
  `state->in_use = false`. (No key/ctx zeroization — see §2.3-A3.)

#### 1.9.5 Capabilities & registration

`query_hw_caps` returns `HACE_CAPS_SUPPORT = CAP_OPAQUE_KEY_HNDL | CAP_RAW_KEY |
CAP_SEPARATE_IO_BUFS | CAP_SYNC_OPS` (`hace_aspeed.c:60–61,767–769`). Driver
registered via `crypto_funcs` (`hace_aspeed.c:796–803`):
`cipher_begin_session = aspeed_crypto_session_setup`,
`cipher_free_session = aspeed_crypto_session_free`,
`cipher_async_callback_set = NULL` (sync-only). The cryptographic core (the
AES-128/256 ECB/CBC transform itself) is standard AES — the engine emits
exactly the NIST-defined ciphertext for the given key/IV/mode (§2.4).

---

## 2. Deltas vs. the authoritative driver (source-verified @ cfe94dc)

All four rows are now grounded in the frozen [zephyr-reference/](zephyr-reference/)
copy, read directly. **D2 is the only intentional delta.** D1 and D4 turned out to
be *conformance*, not divergence (the earlier "vs. `aspeed-rust`" framing was
measuring against the wrong reference); they remain listed so the record is
unambiguous.

| ID | Pinned Zephyr driver (verbatim) | Port behavior | Classification |
|----|----------------------------------|---------------|----------------|
| D1 | **Bounded** wait: `aspeed_hash_wait_completion(dev, 3000)` → `reg_read_poll_timeout(…, 1, 3000)`, returns error on timeout (`hace_aspeed.c:380–393`) | Bounded `poll_budget` → `HaceError::Timeout`, `yield_fn` between polls | **Conformance, not a delta.** Authority already bounds the wait; port matches the observable failure semantics (error return, no partial output). Only the budget unit / yield differ — output-identical. |
| D2 | **Has the stale-`bufcnt` latent bug**: `aspeed_hash_update` updates `bufcnt` only when `remainder != 0`; on an exact block boundary `bufcnt` keeps its prior value (`hace_aspeed.c:489–492`) | Always `bufcnt = remaining` (correct SHA on every input) | **The lone intentional delta (decided: keep the fix).** Unreachable by any consumer: PFR streams 4 KB pages from `bufcnt==0` (boundary branch leaves it `0`, correct); DICE uses sub-block updates (never reaches the branch). Byte-identical to the authority on every reachable input; safer on the pathological one nothing produces. |
| D3 | Authority **does** implement HMAC (`aspeed_hash_setkey`/`aspeed_hash_digest_hmac`); key-reduction threshold is RFC-2104-correct `key_len > block_size` (`hace_aspeed.c:619–659`) | HMAC governed by a *separate* authority by deliberate choice (§2.1) | **Out of this goal's parity scope by decision** — not because the authority lacks HMAC (it doesn't). `aspeed-rust`'s flat `>128` is wrong and is *not* adopted. |
| D4 | Emits the canonical big-endian digest — raw `memcpy(pkt->out_buf, data->digest, digest_size)` of the `SHA_BE_EN` engine output (`hace_aspeed.c:513,583,614`) | `from_ne_bytes` everywhere → same canonical digest | **Conformance, not a delta.** Port output = authority output. `aspeed-rust` *owned*'s `from_be_bytes` byte-swap is an `aspeed-rust` bug the port must not adopt. |

The D2 verification obligation is **discharged**: the exact-block-boundary branch
was read directly from the pinned source and the consumer traces are in
[zephyr-behavior.md](zephyr-behavior.md) §5.

### 2.1 HMAC authority (separate by deliberate choice)

**Correction of record:** the authoritative Zephyr `aspeed_hace` driver *does*
implement HMAC (`aspeed_hash_setkey` + `aspeed_hash_digest_hmac`); the earlier
"Zephyr HACE has no HMAC" claim was wrong — it was based on the thin
`aspeed-zephyr-project` HAL wrapper, which simply never calls those driver ops, not
on the driver itself. Its key-reduction threshold is the **RFC-2104-correct,
algorithm-dependent** `key_len > block_size` (64 / 128), documented in §1.7.

By **explicit decision**, HMAC is governed by a *separate declared authority* rather
than byte-parity with the driver. That authority has two distinct, non-overlapping
parts — do not conflate them:

- **Algorithm correctness authority = the RFCs/KATs only.** RFC 2104 (construction)
  validated by published HMAC-SHA known-answer vectors (RFC 4231 / RFC 2202), with
  the pinned Zephyr driver (§1.7) as the *behavioral model* for the implementation
  shape (key reduction, full-`block_size` ipad/opad XOR, one-shot semantics). This
  is what fixes the `> block_size` key threshold (the driver's / RFC-2104's, **not**
  `aspeed-rust`'s flat `>128`).
- **API-surface authority = the openprot MAC trait** ([hal/blocking/src/mac.rs](../../../../../hal/blocking/src/mac.rs)).
  **Note (verified):** that trait is algorithm-agnostic — `MacInit`/`MacOp`/
  `MacAlgorithm`/`KeyHandle` constrain only the *interface* (init/update/finalize,
  opaque key handle, output type). It contains **no** RFC reference and mandates
  **no** HMAC semantics; no openprot spec document does either (only a glossary
  acronym in `docs/.../terminology.md`). So openprot imposes the *shape*, never the
  *algorithm*. Do not read "openprot requires RFC-2104" — it does not; RFC-2104 is
  *our* declared choice, sourced here.

The port's HMAC must therefore (a) pass the RFC-4231/-2202 KATs, (b) use the
RFC-2104-correct `> block_size` threshold, and (c) satisfy the openprot MAC trait
interface — three independent obligations from two authorities. HMAC
parity-vs-driver is *not* a gate of this goal; digest parity vs. the pinned driver
(§2 D1/D2/D4, §3–§4) is.

### 2.2 OPEN ISSUE — HMAC-SHA512 long-key (RFC-4231 #6/#7)

Status as of this investigation (QEMU `ast1030-evb`, KAT harness):

- **Green:** all SHA-256/384/512 digests (NIST one-shot, 4 KB-aligned streaming,
  D2 boundary); HMAC-SHA256 all RFC-4231 cases incl. streamed; HMAC-SHA384 all
  cases incl. the 131-byte > block-size key reduction; HMAC-SHA512 cases 1–4 and
  with a pre-reduced 64-byte key.
- **Failing:** HMAC-SHA512 with `key_len > 128` (RFC-4231 #6/#7) — a wrong but
  valid-looking SHA-512 tag (deterministic for a fixed binary; no crash).

Isolation (all verified by sub-tests in the KAT harness):

- `SHA-512(131-byte key)` standalone is **correct**.
- The full HMAC-SHA512 #6 math reconstructed via the public digest API is
  **correct** (`dbg sha512 hmac6 manual`).
- `hmac.rs` with a *pre-reduced* 64-byte key (non-reduce branch) is **correct**.
- Only `hmac.rs`'s reduce branch (3 chained SHA-512 sub-hashes, first = the
  131-byte key reduction) is wrong — **only** for the SHA-512 monomorphization
  (SHA-256/384 identical structure pass).
- The defect is **memory-layout-sensitive**: forcing `HmacKey` alignment moved
  the failing case (#6→#4); moving HMAC buffers into a `static` made a
  previously-green SHA streaming case HardFault. ⇒ this is a memory/DMA/aliasing
  fault whose victim shifts with layout, **not** a SHA-512 algorithm error.

Conclusion: black-box rebuild iteration is not converging and risks regressions.
Next investigation must use **instrumented tooling** — QEMU memory/DMA tracing or
GDB watchpoints on the corrupted region, and/or reading the upstream QEMU
`aspeed-hace` model for SG-DMA addressing/length constraints on 128-byte-block
(SHA-384/512) operations — not further speculative edits.

**Fresh layout-sensitivity confirmation (2026-05-16, AES work).** Adding the
AES `.ram_nc` `CryptoContext` static (`SHARED_CRYPTO_CTX`) and the
`HaceDevice.crypto_ctx` field shifted this fault's signature: the
`hmac-sha512 rfc4231-6` wrong tag moved from `actual[0..8]=5a6e818a5a40625b
last4=693158be` (pre-AES) to `be109eb6a7012d1f last4=a208e5b9` (post-AES), with
**the full 32-case green set and the failing case both unchanged** (only the
victim *value* moved). This is exactly the predicted "victim shifts with
memory layout" behavior — independent corroboration that §2.2 is a
memory/DMA/aliasing fault, not a SHA-512 algorithm error. It is **not** a
regression of the AES change (no green case lost; the gate is the green set,
which held) and debugging it remains out of scope here.

### 2.3 AES deltas vs. the authoritative driver (source-verified @ cfe94dc)

Parity standard = observable byte-for-byte on every reachable input, keep
fixes. The **cryptographic output is standard AES** (NIST-gated, §2.4); the
rows below are about *framing / lifecycle / scope*, not the cipher transform.

| ID | Pinned Zephyr driver (verbatim + `file:line`) | Port behavior | Classification |
|----|-----------------------------------------------|---------------|----------------|
| A1 | Crypto exclusivity is a runtime flag: `aspeed_crypto_session_setup` rejects overlap with `-EBUSY` if `state->in_use` (`hace_aspeed.c:254–256`), plus a HW busy-bit check `hace_sts.crypto_engine_sts` in `crypto_trigger` (`:83–87`) | No `in_use` flag, no busy-bit read; AES is the **3rd operation** of the one owned `HaceDevice`, obtained only by an exclusive `&mut` borrow-split (`design-patterns :: borrow-arbitrated-engine-exclusivity`) | **Intentional structural delta (decided).** Same replacement, and the same classification, already applied to the digest/HMAC ops (see the resolved `goal-tech-debt.md`); overlap becomes a compile error. Output-identical; the `-EBUSY` return is unreachable when exclusivity is structural. Recorded as the intentional structural delta, mirroring SBC. |
| A2 | CBC framing is **IV in-band**: encrypt prepends the 16-byte IV to the output (`out = IV ‖ CT`, `out_len = in_len + 16`; `hace_aspeed.c:186–191`); decrypt expects `in = IV ‖ CT` and skips the first 16 bytes (`:200–205`) | The openprot cipher trait carries the nonce/IV **separately** (`CipherInit::init(key, nonce, mode)`, see §2.4); the port maps `nonce → ctx[0..16]` and the ciphertext buffer is **just `CT`** (no in-band IV prefix/strip) | **Intentional delta (decided: follow the trait shape).** The AES-CBC *ciphertext bytes are byte-identical* to the driver (and to NIST CAVS) for the same key/IV — only the driver's in-band IV *framing convention* differs, an I/O wrapper, not the transform. Directly analogous to D4 (canonical-digest framing). Reachability: the openprot consumers drive the typed `cipher` trait with a separate `Nonce`; none expect a driver-style `IV ‖ CT` blob. A KAT asserts port `CT` == NIST `CT`. |
| A3 | `aspeed_crypto_session_free` clears only `in_use`; the raw key in `ctx[16..]` and the IV in `ctx[0..16]` are **not zeroized** (`hace_aspeed.c:370–377`) | Port zeroizes the key/IV region of the context on session/op drop | **Intentional delta (decided: keep the fix; security).** Leaving key material resident in a `.ram_nc` DMA buffer on a RoT is a defect no consumer relies on; zeroizing changes no cipher output. Safer on every input; observable only as cleared scratch. |
| A4 | `aspeed_aes_crypt` performs **no input-length / block-multiple validation**; `in_len` is passed to the engine as-is (`hace_aspeed.c:130–135`) | Port rejects non-16-byte-multiple `in_len` for ECB/CBC with a typed `InvalidInput` before programming the engine | **Intentional delta (decided: keep the fix; safety).** Adds a bound the C omits; cannot change output for any valid (block-aligned) input — the only reachable production case. Mirrors the SBC port's "Rust-side bound the C omits" deltas. |
| A5 | Driver also wires **CFB/OFB/CTR** (via the CBC IV-prepend handlers, `hace_aspeed.c:325–356`), **AES-192** (`:294`), and **DES/TDES** (`:272–281`) | Not implemented | **Out of scope by decision (Phase 2).** Not a defect — deliberately deferred. CFB/OFB/CTR-via-CBC-handler in particular is a behaviorally surprising path; if a later increment adds it, its parity classification is re-opened then. Recorded here so the omission is explicit, not silent. |
| A6 | **OTP/secret-vault sideload-key** path: non-`CAP_RAW_KEY` keys are an opaque 1-byte handle → slot select `SELECT_VAL_KEY_1/2` (MMIO `sbase+0xc` bit 0) + `HACE_CMD_AES_KEY_FROM_OTP` (`BIT(24)`) OR'd into `cmd`; invalid id → `-EINVAL` (`hace_aspeed.c:113–128`, `hace_aspeed.h:16,193–199`) | **Split (decided 2026-05-16):** vault-*select logic* implemented & bit-exactly tested (handle → register/cmd, invalid-id error); vault *crypto E2E* not run here | **Parity re-opened for the select logic — in scope this increment.** The handle→register/command-word mapping is fully pinned and pure (no silicon): **ported and compile-verified** — asserted bit-for-bit vs. the frozen source by compile-time `const` assertions (`constants.rs`, checked every build; a wrong value is a hard compile error) plus named `#[cfg(test)]` cases. **Crypto E2E remains separated (OPEN ISSUE §2.6):** a vault key is non-software-visible by design (OTP), so no KAT oracle exists; end-to-end needs real HACE silicon + an out-of-band-provisioned slot (one-time/destructive), gated exactly like the AES KAT (§2.5). The vault path is also the principled resolution of A3 (no software-resident key) — recorded, deferred, not a defect. |

A1/A3/A4 are the intentional deltas; A2 is framing-only (ciphertext identical);
A5 (CFB/OFB/CTR, AES-192, DES/TDES) is scoped-out; A6 is **split** — vault-select
logic in scope this increment, vault crypto E2E separated (§2.6). No row changes
the AES *transform* output on any in-scope, reachable input — that is
NIST-identical (§2.4).

### 2.4 AES correctness & interface authorities (separate by deliberate choice)

As with HMAC (§2.1), do not conflate the authorities:

- **Algorithm-correctness authority = published NIST KATs only.** AES-128/256
  ECB and CBC validated against **NIST AESAVS / CAVP** known-answer vectors
  (FIPS-197 / SP 800-38A). The pinned driver is the *behavioral/framing* model
  (register/cmd sequence, ctx layout, lifecycle) — **not** the cipher-vector
  oracle. The engine's transform is standard AES; parity-vs-driver on the
  ciphertext bytes and NIST-correctness coincide, but the gate is the NIST KAT.
- **Interface authority = the openprot cipher trait**
  ([hal/blocking/src/cipher.rs](../../../../../hal/blocking/src/cipher.rs)).
  **Verified (read directly):** `SymmetricCipher` + `CipherInit<M>` /
  `CipherOp<M>` constrain only the *shape* — `init(&key, &nonce, mode) -> ctx`,
  then one-shot `encrypt(PlainText) -> CipherText` / `decrypt(CipherText) ->
  PlainText`; `CipherMode`/`BlockCipherMode` are algorithm-agnostic markers; no
  AES/NIST semantics are mandated. The nonce is a **separate** associated type
  from plaintext/ciphertext — this is the trait basis for delta A2. The trait's
  one-shot (non-streaming) `encrypt`/`decrypt` matches the driver's one-shot
  crypt exactly (§1.9.4) — no streaming adapter is needed.

Three independent obligations: (a) ciphertext matches NIST AESAVS/CAVP;
(b) register/ctx/lifecycle behavior matches the pinned driver per §1.9 modulo
the §2.3 deltas; (c) the openprot cipher trait interface is satisfied. AES
parity-vs-driver on the *transform* is implied by (a); the goal's parity gate
for AES is (a)+(b).

### 2.5 OPEN ISSUE — AES KAT cannot run on the QEMU model (env, not code)

Status as of 2026-05-16 (stop-and-instrument; QEMU `ast1030-evb`,
`qemu-ast10x0-i2c` @ `73c48a0`):

- **Green:** the AES core compiles and links into the QEMU image; all
  SHA-2/HMAC KATs still pass (no regression from the AES/façade/context/device
  changes).
- **Failing:** every AES KAT case — first case `ecb-128 encrypt` returns
  `Ok` with an **all-zero** `dst` (expected `3ad77bb4…`). Deterministic.

Root cause — **observed in the emulator source, not inferred**: the upstream
QEMU aspeed HACE model `hw/misc/aspeed_hace.c` implements **hash only**. It has
no crypto data registers (only `R_HASH_SRC/DIGEST/KEY_BUFF/SRC_LEN`); the
`R_CRYPT_CMD` handler (offset `0x10`, = our HACE10 start write) is literally:

```c
case R_CRYPT_CMD:
    qemu_log_mask(LOG_UNIMP, "%s: Crypt commands not implemented\n", __func__);
    if (ahc->raise_crypt_interrupt_workaround) { s->regs[R_STATUS] |= CRYPT_IRQ; ... }
```

So HACE00/04/08/0C writes fall through `default: break` (dropped) and HACE10
merely sets `CRYPT_IRQ` without performing AES — exactly the observed
"completes instantly, zero output." This **bisects decisively**: the fault is
the emulator's scope, **not** the port's register/command code (cf. §2.2,
which *was* a real layout-sensitive code fault; this is the opposite — an
environment limitation, classified per the `stop-and-instrument` matrix as
"out of this layer's scope, hand off with evidence").

Consequences / decisions:

- The AES core stays **compiled + spec-faithful but UNVERIFIED**. The NIST
  AESAVS/CAVP correctness gate (§2.4, §3 item 7) is **blocked on real
  hardware** — the physical AST1060 EVB (`--config=k_ast1060_evb`), or a QEMU
  build that models HACE crypto. No code edit can pass it under
  `--config=virt_ast10x0`; speculative edits are explicitly *not* the next
  step (the engine being validated against does not exist in QEMU).
- `hace_aes_test` must therefore be **hardware-only** — tagged so it does not
  run (and falsely fail) under the QEMU config, mirroring the repo's
  hardware-only test handling. Pending that gating decision it is a known,
  documented red on QEMU, not a regression of any previously-green case.
- A QEMU-side option (out of this goal's scope, recorded for completeness):
  extend `aspeed_hace.c` to model the crypto path, or run against a vendor
  QEMU that does. Not undertaken here.

### 2.6 OPEN ISSUE — vault-key crypto E2E needs silicon + OTP provisioning

Status as of 2026-05-16 (decision: A6 split — see §2.3).

Scope split, by decision:

- **In scope this increment (testable now, no silicon):** the vault-*select
  logic*. Everything the pinned authority specifies for the sideload path is
  pure register/command logic — a `key_id` handle ∈ {1,2} → exact MMIO write at
  `sbase+0xc` bit 0 (`SELECT_VAL_KEY_1/2`, `hace_aspeed.h:193–199`),
  `HACE_CMD_AES_KEY_FROM_OTP` (`BIT(24)`, `hace_aspeed.h:16`) OR'd into `cmd`,
  typed error on any other id (driver `-EINVAL`, `hace_aspeed.c:113–128`).
  Ported and asserted **bit-for-bit against the frozen source by compile-time
  `const` assertions** (`constants.rs`, checked on *every* build including
  firmware — a wrong value is a hard compile error), plus named `#[cfg(test)]`
  cases for the handle→typed-error mapping (§3 items 6–7) — true parity of
  100 % of what the normative reference defines for this path. **Status: done,
  compile-verified** (`bazelisk build --config=virt_ast10x0
  //target/ast10x0/peripherals:peripherals`).
- **Separated (this OPEN ISSUE):** vault *crypto end-to-end*. A vault key is
  **non-software-visible by design** (resident in OTP; the caller holds only a
  1-byte handle), so a classical KAT — known key → known ciphertext — is
  **impossible**: no software oracle exists. The most an E2E can be is a
  *differential* test on real silicon — `AES(vault=N, pt) == AES(raw=K, pt)`
  with `K` provisioned out of band — and OTP provisioning is
  one-time/destructive and board-specific.

Consequences / decisions:

- The select logic lands as normal parity work and is **verified** (mocked,
  bit-exact). The vault crypto E2E is **blocked on real hardware + a
  provisioned slot**, gated exactly like the AES NIST KAT (§2.5): no QEMU path
  (the model is hash-only and would not hold OTP key state regardless).
- A vault E2E harness, if later run, is **hardware-only + differential** (not a
  KAT) and tagged so it does not run (and falsely fail) off-hardware —
  mirroring `hace_aes_test` handling (§2.5). Pending that, it is a documented
  deferral, not a regression.
- This path is also the principled resolution of delta A3 (it removes
  software-resident key material entirely). Recorded so the A3↔A6 relationship
  is explicit; bringing E2E in later is sanctioned, not a re-litigation.

---

## 3. Implementation plan

1. **Algorithm spec generalization** (`digest.rs`, `constants.rs`, `context.rs`)
   - Add `HaceDigestSpec` impls for `Sha2_384`, `Sha2_512`: `HASH_CMD` (ACC|BE|SG|algo
     bits), `BLOCK_SIZE = 128`, IVs (`SHA384_IV`, `SHA512_IV` as `[u32;16]`),
     `digest_from_context` (48 / 64 bytes, `from_ne_bytes`).
   - Add the SHA-384/512 IV constants and digest-size constants.
   - Confirm `fill_padding` block-128 path (already present) is selected via
     `ctx.block_size`.
   - Acceptance: SHA-256/384/512 of `b"hello_world"` match the `hash_test.rs`
     expected vectors byte-for-byte.

2. **HMAC module** (new `hmac.rs` + `mod.rs` export) — **governed by §2.1.**
   **Decided implementation strategy: software RFC-2104 over the port's own HACE
   hasher**, *not* the engine's native HMAC ops. The openprot MAC trait is
   construction-agnostic (verified — [hal/blocking/src/mac.rs](../../../../../hal/blocking/src/mac.rs):
   no provided HMAC, no required hasher; only `MacOutput = Digest<N>` couples the
   output type to the digest module), so HMAC is implemented *on top of* the
   already-parity-verified `DigestInit`/`DigestOp` (§1.5). This is RFC-2104 by
   construction, trivially KAT-gated, and adds **no** engine-HMAC register code.
   The driver's `aspeed_hash_*_hmac` (§1.7) is a *behavioral cross-reference only*,
   not the implementation; `aspeed-rust`'s flat `>128` is rejected.
   - Implement `MacInit::init`: derive `K0` — if `key.len() > block_size`
     (**RFC-2104-correct**: 64 for SHA-256, 128 for SHA-384/512) reduce via one
     `DigestOp` pass `H(key)`, else use the raw key; zero-pad to `block_size`;
     form `ipad = K0 ⊕ 0x36`, `opad = K0 ⊕ 0x5c` over the full `block_size`;
     start the inner digest and absorb `ipad`.
   - `MacOp::update` → feed the inner `DigestOp`. `MacOp::finalize` →
     `inner = H(ipad ‖ msg)`, then `tag = H(opad ‖ inner)` via a fresh `DigestOp`;
     return `Digest<N>`. No `HACE_SG_EN` source-select / linear-buffer plumbing is
     needed (it was only for the engine-native HMAC path, now not taken).
   - `key`/`ipad`/`opad` may reuse `context.rs` scratch but are driven by the MAC
     module, not the engine HMAC mode.
   - Acceptance: HMAC-SHA256/384/512 match published RFC-4231/-2202 KATs; long-key
     (`> block_size`) reduces via `H(K)`; a 65–128 B SHA-256 key **is reduced**
     (not used raw — the `aspeed-rust` deviation we reject).

3. **Owned API module** (new `digest_owned.rs` + export)
   - Move-based init/update/finalize/cancel reusing the scoped state machine and
     `from_ne_bytes` packing (D4); `bufcnt = 0` on block boundary (D2).
   - Acceptance: chunked streaming across boundaries equals one-shot digest;
     `cancel` leaves a clean reusable context.

4. ~~**Register source-select** (`registers.rs`)~~ — **dropped.** The
   `method & HACE_SG_EN` linear-buffer source-select was only needed for the
   engine-native HMAC/key-hash path. With software HMAC (item 2) the port never
   clears `HACE_SG_EN`, so SG is always on and `src_addr` is unconditionally the
   SG-list pointer. No change to `program_hash_operation` is required.

5. **Parity test harness** — keyed to the authoritative Zephyr workload
   - SHA-256/384/512 KAT vectors (= pinned-Zephyr output; standard digests).
   - **Gating test**: the production path — `start → update(4096)×N → update(tail)
     → finish` — asserted byte-equal to standard SHA. (On this path the authority's
     D2 branch is dormant, so port == authority == standard SHA.)
   - **D2 delta test**: the pathological pattern (non-empty buffer then an update
     that exactly completes to a block multiple) — assert the port produces correct
     SHA and document that the authority would here produce a different (stale-
     `bufcnt`) value. This is the one place port ≠ authority, by decision; no
     consumer reaches it.
   - D1/D4 require no divergence assertions — they are conformance (§2).
   - HMAC KATs live in the HMAC component's harness (§2.1), not here.

6. **AES module** (new `aes.rs` + `mod.rs` export) — **governed by §1.9 / §2.3
   / §2.4.** Scope: AES-128/256, ECB + CBC, raw key.
   - Add a `.ram_nc`, `#[repr(C, align(64))]` crypto context mirroring
     `aspeed_crypto_ctx` (§1.9.3): `ctx[64]` (`[0..16)` IV, `[16..]` key),
     `src_sg`/`dst_sg` (`addr`, `len|BIT(31)`), `cmd`. **Reuse the
     borrow-arbitrated single-owned-device model** — `AesCipher` is obtained
     only by an exclusive `&mut HaceDevice` borrow-split, exactly like
     `HaceDigest`/`HaceHmac` (delta A1; the engine cannot do concurrent
     crypto+hash, §5.1). No `in_use` flag, no `crypto_engine_sts` busy read.
   - `cmd` composition (§1.9.2): base
     `DES_SG_CTRL|SRC_SG_CTRL|MBUS_REQ_SYNC_EN`, `AES_KEY_HW_EXP|AES_SELECT`,
     keylen→`AES128/256`, mode→`ECB/CBC`, dir→`ENCRYPT`/`DECRYPT (0)`.
   - Drive the crypto register file in the exact §1.9.1 order via the
     confined-`unsafe` `HaceRegisters` façade (extend it with the crypto regs:
     `crypto_data_src/dst`, `crypto_ctx_base`, `crypto_data_len`,
     `crypto_cmd_reg`, `crypto_int`/`crypto_engine_sts` on `hace_sts`); bounded
     completion poll on `crypto_int` reusing the `cooperative-yield-bounded-
     poll-device` strategy + `poll_budget` → `HaceError::Timeout` (parity with
     the driver's bounded 3000-ms wait, §1.9.1; same conformance basis as D1).
   - **API shape — ADR-A1 (decided, mirrors the SBC port's ADR-1).** The
     driver core is a **trait-free, slice-based** entry on the borrow-
     arbitrated op — `AesCipher::{encrypt_raw,decrypt_raw}(&[u8], &mut [u8])`
     — exactly as `SbcOp::{verify_raw,modexp}` are trait-free slice entries.
     The openprot `CipherInit<M>`/`CipherOp<M>` is a **separate, optional thin
     skin** (a `hal_impl`-style wrapper over fixed buffers), *not* the driver
     core: the trait's single fixed `PlainText`/`CipherText` associated types
     cannot express the ≥ 4 KB streaming-DMA path (§3 item 7) and goal.md §2.4
     already records the trait as shape-only. `Ecb`/`Cbc` are port-defined
     zero-size marker types (`impl CipherMode + BlockCipherMode`; the hal
     defines no concrete modes). The skin: `init(&key, &nonce, mode)` →
     context (nonce → IV); one-shot `encrypt`/`decrypt` delegating to the raw
     core. Ciphertext buffer is **plain `CT`** (no in-band IV — delta A2).
     Zeroize key/IV on drop (delta A3). Reject non-block-multiple `in_len`
     with typed `InvalidInput` before triggering (delta A4). Add
     `impl cipher::Error for HaceError` (`error.rs`), alongside the existing
     `DigestError`/`MacError` impls.
   - DMA discipline: context/`src`/`dst` are `.ram_nc` (no `cache_data_invd_all`
     needed — non-cached, mirrors the §1.3 HashContext placement and the same
     layout-sensitivity caution as §2.2).
   - **Vault-select logic (delta A6, in scope — §2.3 / §2.6).** Add the
     non-raw-key branch: a 1-byte slot handle → `SELECT_VAL_KEY_1/2` write at
     crypto `sbase+0xc` bit 0 and `HACE_CMD_AES_KEY_FROM_OTP` (`BIT(24)`) OR'd
     into `cmd`; invalid id → typed error (driver `-EINVAL`,
     `hace_aspeed.c:113–128`, `hace_aspeed.h:16,193–199`). **Logic only** — no
     OTP provisioning; crypto E2E is separated (§2.6).
   - Acceptance: AES-128 and AES-256, ECB and CBC, encrypt and decrypt produce
     the NIST AESAVS/CAVP KAT ciphertext/plaintext byte-for-byte; a second
     concurrent AES/hash op is a borrow-check error (delta A1); a
     non-block-multiple input returns `InvalidInput` (delta A4); the
     vault-select branch computes the exact `SELECT_VAL_KEY_1/2` RMW value and
     sets `AES_KEY_FROM_OTP`, and an invalid slot id returns the typed error
     before any register write — asserted by compile-time `const` assertions
     (delta A6, done/compile-verified); vault crypto E2E is out of this gate
     (§2.6).

7. **AES parity / KAT harness** (extend the QEMU `ast1030-evb` suite)
   - **Correctness gate**: NIST AESAVS/CAVP vectors for AES-128/256 ECB and CBC
     (KAT + a multi-block MMT-style case), encrypt and decrypt, asserted
     byte-equal. This is the AES parity gate (§2.4 — equals the driver's
     transform output by construction).
   - **Production-dominant pattern**: a multi-block (≥ 4 KB) CBC encrypt→decrypt
     round-trip equals the plaintext, exercising the real SG-DMA length path
     (`len|BIT(31)`), not just a single 16-byte block.
   - **Delta A2 test**: assert the port's ciphertext buffer is exactly `CT`
     (length `in_len`, no leading IV) while the same key/IV/plaintext yields
     the NIST `CT` — documents the driver's `IV ‖ CT` framing divergence.
   - **Delta A4 test**: a 17-byte (non-block-multiple) input returns typed
     `InvalidInput`, no engine programming.
   - A1/A3 need no divergence vector (structural / cleared-scratch); A1 is
     covered by a compile-fail doc-test if the harness has one, else asserted
     by construction. A5 (out of scope) gets no test.
   - **Delta A6 test (in scope — done, compile-verified):** the select logic
     is asserted bit-for-bit vs. the frozen source by **compile-time `const`
     assertions** in `constants.rs` (slot handle 1/2 → exact
     `SELECT_VAL_KEY_1/2` RMW value at `sbase+0xc`; `AES_KEY_FROM_OTP` set in
     `cmd`; raw path never sets it) — checked on every build, a wrong value is
     a hard compile error — plus named `#[cfg(test)]` cases for the
     handle→typed-error mapping. Stronger than a runtime mock; needs no test
     runner. Vault crypto E2E is hardware-only + differential and **not** in
     this harness (§2.6).

## 4. Done criteria

- SHA-256/384/512 digest: byte-identical to the **pinned Zephyr driver**
  ([zephyr-reference/](zephyr-reference/) @ `cfe94dc`) for every reachable input,
  explicitly the production-dominant `start → update(4096)×N → update(tail) →
  finish` path — verified against the frozen `hace_aspeed.c` directly.
- Owned API: emits the same canonical digest as the scoped API (= Zephyr output);
  the `aspeed-rust` owned-API `from_be_bytes` byte-swap is **not** reproduced (D4
  conformance).
- D2 is the **only** intentional delta **for the digest path** — covered by a
  positive test (port = correct SHA) plus a documented note that the authority
  diverges on the unreachable pathological pattern. D1/D4 are conformance, no
  divergence tests needed. No other behavioral difference vs. the pinned driver
  on any reachable digest input. (AES has its own intentional deltas A1/A3/A4,
  tracked separately in §2.3 with their own tests, §3 item 7.)
- HMAC: gated by published RFC-4231/-2202 KATs and the RFC-2104-correct
  `> block_size` threshold (§2.1) — not by byte-parity with the driver, by decision.
- AES (ECB/CBC, 128/256, raw key): ciphertext/plaintext byte-identical to NIST
  AESAVS/CAVP vectors for encrypt and decrypt (§2.4), including a
  production-dominant ≥ 4 KB multi-block CBC round-trip. Register/ctx/lifecycle
  behavior matches §1.9 modulo the §2.3 deltas. Intentional deltas A1
  (structural exclusivity — AES routes through the one owned `HaceDevice`,
  overlap is a compile error), A3 (key/IV zeroized on drop), A4
  (non-block-multiple input → typed `InvalidInput`) each covered per §3 item 7;
  A2 (no in-band IV framing) asserted ciphertext-identical to NIST. Delta A6
  (vault-select logic) is **in scope and done (compile-verified)**:
  handle→register/cmd mapping bit-exact vs. the frozen source by compile-time
  `const` assertions (every build), invalid id → typed error; the vault crypto
  E2E is separated and hardware-gated (§2.6), not a failing criterion here. A5
  surface is out of scope by decision (§2.3) — its absence
  is not a failing criterion.
  Not gated by byte-parity with the driver's `IV ‖ CT` framing, by decision
  (A2); gated by NIST-correctness + §1.9 behavioral parity.

---

## 5. Architecture decision — user-space driver & SW/HW selection

### 5.1 Vendor constraint (normative)

**ASPEED states the HACE engine cannot support concurrent streaming sessions in
hardware.** This is consistent with the frozen reference: a single global
ACC-mode context (`digest`/`buffer`/`digcnt`/`bufcnt`/`method` in one `.ram_nc`
region) and a single `in_use` flag that *rejects* overlap with `-EBUSY`
([zephyr-behavior.md](zephyr-behavior.md) §2, `hace_aspeed.c:670-676`). There is
**no validated way to save/restore partial hash state** to multiplex the engine
mid-stream.

Consequence: a HW streaming session is an **exclusive lock on the singleton held
from `begin` to `finish`**. The decisive hazard is not two simultaneous clients —
it is one client holding a session open *across a yield* (I/O, scheduler quantum,
another component). That pins the engine for the whole interval.

**AES is the engine's third operation, not a second engine.** The crypto path
uses a distinct register file and status bit (§1.9.1) but is the *same physical
HACE engine*: hash and AES cannot run concurrently, and the driver enforces
this with the same `in_use`/busy-bit discipline as hash
(`hace_aspeed.c:254–256,83–87`; delta A1). The port therefore makes AES the
**third operation arbitrated by the one owned `HaceDevice`** — `AesCipher`,
like `HaceDigest`/`HaceHmac`, is obtained only by an exclusive `&mut` device
borrow (`design-patterns :: borrow-arbitrated-engine-exclusivity`, the pattern
landed in the resolved `goal-tech-debt.md`). Hash⇄AES mutual exclusion is thus
a compile error, replacing the reference's two independent runtime flags — and
crypto, being one-shot and run-to-completion, never holds the engine across a
yield (it is the well-behaved case for §5.3's HW-RPC path).

### 5.2 Decisions

1. **SPDM and any I/O-interleaved or event-driven hashing → software,
   in-process. Mandatory, not a performance preference.** SPDM transcript hashing
   spans the entire protocol exchange interleaved with network round-trips;
   running it on HW would let a remote/slow SPDM peer starve PFR firmware
   measurement, DICE, and attestation behind it — an availability/DoS exposure on
   the RoT reachable externally. Software contexts are per-component and naturally
   concurrent; they *are* the concurrency story. SPDM depends only on the abstract
   `digest`/`mac` traits and never names a backend.

2. **The user-space HACE driver is a bounded request server, NOT a streaming
   session server.** It must **not** expose `begin/update/finish` across the IPC
   boundary — that re-creates the unsupported held-across-yield lock, now spanning
   processes. It exposes only whole-object, run-to-completion operations the
   driver completes internally before releasing the engine and replying:
   - `hash(algo, buffer) -> digest`
   - `hash_region(algo, flash/mem descriptor) -> digest` (driver runs the page
     loop itself; engine held briefly, released before reply)
   - optionally `hash_sg(list of complete segments) -> digest`

   Requests are serialized by a real queue inside the driver — the principled
   replacement for the reference's `in_use`/`-EBUSY` flag. No session may straddle
   a client yield.

3. **Streaming stays in-process.** The scoped/owned trait APIs (§1.5, §1.8) remain
   for in-process, trusted, bounded-loop bulk measurement only — never marshalled
   across the service boundary.

4. **HW context save/restore is forbidden.** Do not attempt to multiplex the
   engine by snapshotting the RAM context: the vendor's "no concurrent streaming"
   means engine state is not guaranteed captured there. It is not a safe basis for
   any design.

### 5.3 Backend selection policy

| Workload | Backend | Rationale |
|----------|---------|-----------|
| SPDM transcript, HKDF, session keys, signatures | **Software, in-process** | Long-lived/interleaved or secret-bearing; HW can't hold it without starving others; isolation |
| Firmware / image / manifest measurement (large, run-to-completion) | **HW via whole-object RPC** | Bounded, non-yielding, non-secret; IPC amortized over large buffers |
| Small one-shot non-secret hashes | Either; **default software** | Not worth IPC + serialization unless profiled hot |
| Bulk AES-ECB/CBC of non-secret-keyed data (large, run-to-completion) | **HW via whole-object RPC** | One-shot, non-yielding (§1.9.4); IPC amortized over large buffers; same shape as bulk measurement |
| AES with secret/session keys, or interleaved with a protocol | **Software, in-process** | Secret-bearing or long-lived/interleaved; HW path resident-keys in `.ram_nc` and serializes the shared engine |
| Fallback when HW busy/unavailable | **Software** (always present) | Correctness must never depend on the singleton; trait makes them substitutable |

Selection is a platform composition concern injected behind the trait, decided by:
capability filter (algo ∈ HACE set) → run-to-completion & size threshold → trust
class → else software (the default and correctness baseline). HACE covers
SHA-2 (+HMAC) **and AES-ECB/CBC-128/256** (this goal's crypto scope, §1.9/§2.3);
AES-192/CFB/OFB/CTR, DES/3DES, ECDSA/ECDHE/AEAD/DRBG are software regardless, so
the stack stays inherently hybrid and the accelerator is opportunistic for the
SHA-2 + AES-ECB/CBC subset only. All HACE ops (SHA, HMAC sub-hashes, AES) share
the one owned device, so backend selection never has to reason about
intra-engine concurrency — exclusivity is structural (§5.1, delta A1).
