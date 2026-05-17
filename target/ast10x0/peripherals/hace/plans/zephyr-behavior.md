# HACE Behavior — `aspeed-zephyr-project` Reference (AST1060)

Reverse-engineered companion to [goal.md](goal.md). Where `goal.md` documents the
`aspeed-rust` reference, this document records the **`aspeed-zephyr-project`** path:
the Zephyr crypto-hash stack the AST1060 firmware actually ships, so the openprot
port can be checked against *both* references.

Sources reverse-engineered:

- `aspeed-zephyr-project/lib/hrot_hal/crypto/hash_aspeed.{c,h}` — HAL state machine.
- `aspeed-zephyr-project/lib/hrot_wrapper/crypto/hash_wrapper.{c,h}` — Cerberus
  `hash_engine` v-table binding.
- The Zephyr `crypto/hash.h` API contract (`hash_begin_session`, `hash_update`,
  `hash_compute`, `hash_free_session`, `crypto_query_hwcaps`).
- The `aspeed_hace` driver itself — **now vendored**, frozen verbatim at
  [zephyr-reference/](zephyr-reference/) (`hace_aspeed.c`, `hace_aspeed.h`,
  `hash_aspeed_priv.h`) @ `cfe94dc`. All register/algorithm claims below are read
  **directly from that frozen source**, not inferred from `aspeed-rust` (which is
  informative only and, where it differs, buggy).

Scope: AST1060 only, little-endian Cortex-M. The thin `aspeed-zephyr-project` HAL
path exercises **digest only** (it never calls the driver's HMAC ops), but the
driver itself **does implement HMAC** (`aspeed_hash_setkey` /
`aspeed_hash_digest_hmac`); see [goal.md](goal.md) §1.7 / §2.1.

---

## 1. Stack layering

```
Cerberus hash_engine v-table          hash_wrapper.c   (start_sha256/384, update,
  |                                                     finish, cancel, calculate_*)
  v
HAL state machine                     hash_aspeed.c    (single static `hashParams`)
  |  hash_begin_session / hash_update / hash_compute / hash_free_session
  v
Zephyr crypto hash API                crypto/hash.h    (struct hash_ctx / hash_pkt)
  |  ctx->hash_hndlr(ctx, pkt, finish)
  v
aspeed_hace driver (upstream Zephyr)  drivers/crypto   (DMA + HACE registers)
  |  hace1c/20/24/28/2c/30
  v
HACE hardware
```

The driver is bound by name: `HASH_DRV_NAME = DEVICE_DT_NAME(DT_INST(0, aspeed_hace))`
under `CONFIG_CRYPTO_ASPEED`. `device_get_binding(HASH_DRV_NAME)` is re-resolved on
every HAL entry point (not cached).

---

## 2. HAL state machine (`hash_aspeed.c`)

One file-scope instance drives everything:

```c
static struct hash_params hashParams;   // { struct hash_ctx ctx; struct hash_pkt pkt;
                                         //   uint8_t sessionReady; }
```

**Consequence: exactly one hash operation in flight, process-wide. Not reentrant,
not thread-safe.** This matches the port's single global `HashContext` in `.ram_nc`.

### 2.1 `hash_engine_start(algo)` — begin streaming session

1. `memset(&hashParams, 0, sizeof hashParams)` — wipes `ctx`, `pkt`, and
   `sessionReady`. Any prior session handle is dropped *without* `free_session`.
2. Resolve `dev`.
3. `hashParams.ctx.flags = crypto_query_hwcaps(dev)` — capability flags fed back
   into the session (selects sync/async + algo caps).
4. `hash_begin_session(dev, &ctx, algo)` → driver allocates session state, records
   `algo`, sets `ctx.hash_hndlr`.
5. On success: `sessionReady = 1`.

### 2.2 `hash_engine_update(data, len)` — accumulate

Sets `pkt.in_buf = data`, `pkt.in_len = len`, calls `hash_update(&ctx, &pkt)` which
dispatches `ctx->hash_hndlr(ctx, pkt, /*finish=*/false)`. Multiple calls stream;
the underlying driver keeps a running block buffer + 128-bit length counter
(§4). `pkt.out_buf` is irrelevant on this path.

### 2.3 `hash_engine_finish(hash, hash_length)` — finalize

1. `pkt.out_buf = hash`.
2. `sessionReady = 0` (set **before** the work — see Quirk Q2).
3. `hash_compute(&ctx, &pkt)` → `ctx->hash_hndlr(ctx, pkt, /*finish=*/true)`:
   pad, final DMA pass, copy `digest_size` bytes into `pkt.out_buf`.
4. `hash_free_session(dev, &ctx)`.

> `hash_length` is **accepted but never enforced** by the HAL. The driver writes
> exactly the algorithm's digest size into `out_buf`. The caller's buffer must be
> ≥ digest size; a too-small `hash_length` is silently ignored, not validated.

### 2.4 `hash_engine_cancel(void)` — abort

`sessionReady = 0`; `hash_free_session(dev, &ctx)`. No padding, no final pass, no
digest produced. Driver-side cleanup zeroes the context and idles the engine.

### 2.5 `hash_engine_sha_calculate(algo, data, len, hash, hash_length)` — one-shot

```
pkt = { in_buf=data, in_len=len, out_buf=hash }
ret = sessionReady ? 0 : hash_begin_session(dev, &ctx, algo)   // (!) see Q1
if !ret: ret = hash_update(&ctx, &pkt)
if !ret: ret = hash_compute(&ctx, &pkt)
hash_free_session(dev, &ctx)                                    // unconditional
return ret
```

A complete digest in one call: begin (if needed) → update → compute → free.

---

## 3. HAL-level quirks / latent defects (Zephyr path only)

These are properties of `hash_aspeed.c`, *not* of the HACE engine. They matter
because they shape what byte-parity actually has to match and which call orders
are safe.

| ID | Behavior | Why it matters |
|----|----------|----------------|
| **Q1** | `hash_engine_sha_calculate` **skips `hash_begin_session` when `sessionReady` is set, and ignores its own `algo` argument**. It then runs update/compute on the *previously started* session and unconditionally frees it. | A `start(SHA256)` followed by `sha_calculate(SHA384, …)` computes **SHA-256**, not SHA-384. The HAL's own test (`hash_test_calculate_sha`) calls `start` then `sha_calculate` with the *same* algo, so it never trips this — but it is order-sensitive. |
| **Q2** | `sha_calculate` and `finish` free the session but `sha_calculate` does **not clear `sessionReady`**; `finish` clears it *before* doing the work. After `start` + `sha_calculate`, `sessionReady` stays `1` while the session is already freed. | A second `sha_calculate` with no intervening `start` skips `begin_session` (Q1) and operates on a freed/stale `ctx` → use-after-free / wrong result. Safe usage: every `sha_calculate` must be preceded by `start`, or never preceded by `start`. |
| **Q3** | `hash_engine_start` `memset`s `hashParams` without freeing a live prior session. | A `start` after a `start` (no `finish`/`cancel`) leaks the driver session handle. The port's scoped/owned API has no equivalent leak. |
| **Q4** | `hash_length` / `out_len` is never propagated or checked anywhere in the HAL. | Output length is implied entirely by the session algorithm; the port likewise derives it from the algorithm spec. No behavioral divergence, but no defensive check either. |
| **Q5** | `hash_test.c` indexes `SHA_DISPLAY_MSG[]` (`{SHA1,SHA256,SHA384,SHA512}`) by the raw `enum hash_algo` value. | Cosmetic only (log string mislabel); does not affect the digest. Noted so it is not mistaken for a parity signal. |

None of Q1–Q5 change the digest bytes on the **intended** call sequence
(`start → update* → finish`, or a lone `sha_calculate`). They constrain *legal
call order*, not output, so they do not relax the byte-parity requirement in
`goal.md`; they are reference-side caveats, not port obligations.

---

## 4. Underlying `aspeed_hace` driver behavior (register + algorithm)

Behavior the HAL's `begin_session` / `hash_hndlr(finish=false)` /
`hash_hndlr(finish=true)` / `free_session` actually drive. Identical to the
`aspeed-rust` reference (`goal.md` §1) — restated here as the Zephyr ground truth.

### 4.1 Hardware contract — six-register pass, fixed order

| Reg | Write |
|-----|-------|
| `hace1c` | `hash_intflag = 1` — clear completion latch |
| `hace20` | source: SG-list ptr if `method & HACE_SG_EN`, else linear `buffer` ptr |
| `hace24` | digest in/out address (`ctx.digest`) |
| `hace28` | **same** digest address as `hace24` |
| `hace2c` | byte length of this pass |
| `hace30` | command word (`method`) — this write starts the engine |

Completion = `hace1c.hash_intflag` re-asserts (driver busy-waits). Idle/cleanup =
`hace30 = 0`. The `hace20` source-select branch is real in the driver even though
the digest path always has `HACE_SG_EN` set (it exists for the SG-disabled key/HMAC
path that this HAL never exercises).

### 4.2 Command word

`method = HACE_CMD_ACC_MODE(1<<8) | HACE_SHA_BE_EN(1<<3) | HACE_SG_EN(1<<18) | algo_bits`

- **ACC_MODE always on**: `digest` is live chaining state — the engine reads it in,
  updates it, writes it back. This is what makes `hash_engine_update` streaming work
  across calls.
- **SHA_BE_EN**: digest emitted big-endian.
- **SG_EN**: source is the 2-entry scatter-gather descriptor list; `HACE_SG_LAST
  (1<<31)` OR'd into the final descriptor length terminates it.
- Algorithm bits (`HACE_ALGO_*`): SHA1 `1<<5`; SHA224 `1<<6`;
  SHA256 `(1<<4)|(1<<6)`; SHA512 `(1<<5)|(1<<6)`; SHA384 `(1<<5)|(1<<6)|(1<<10)`;
  SHA512_224 adds `1<<11`; SHA512_256 = `(1<<5)|(1<<6)|(1<<11)`.

**Reachable through this project's HAL/wrapper:** SHA-256 and SHA-384 via
`hash_wrapper` (`start_sha256`, `start_sha384`, `calculate_sha256`,
`calculate_sha384`); SHA-256/384/512 via the `hash_aspeed.c` self-test
(`HASH_ENABLE_SHA384` / `HASH_ENABLE_SHA512` gated). SHA-1/224 and the SHA-512/t
variants are defined in the engine but unwired here → out of parity scope, same
as `goal.md`.

### 4.3 Memory / context model

Single `aspeed_hash_ctx`-equivalent: 2-entry SG list, `digest[64]`, `buffer`
(block-staging, ≥ 2·128 B), `block_size`, `digcnt[2]` (128-bit bit/byte counter),
`bufcnt`. SG descriptors, `buffer`, and `digest` are DMA targets and must be in
DMA-capable, coherent memory (the port's `.ram_nc`, 64-byte aligned). One op at a
time (enforced upstream by the single static `hashParams`, §2).

**Non-cacheable placement is per-symbol, not partitioned (source-verified,
`hace_aspeed.c:40`).** The whole driver state is one symbol —
`static struct aspeed_hace_drv_state hace_drv_state NON_CACHED_BSS_ALIGN16;` —
so the SG list, `digest`, and `buffer` inherit non-cacheable placement *as a unit*
from that single attribute. Zephyr does **not** partition the AST10x0 non-cacheable
region across devices: there is no carve-out, quota, or allocator. The
`NON_CACHED_BSS_ALIGN16` attribute (defined in the Aspeed SoC layer, *outside* the
pinned subset) expands to a `.nocache.bss` section attribute; the linker aggregates
**every** such symbol from **all** drivers into one contiguous `_nocache_ram`
block, and one MPU region marks that block non-cacheable at boot. Inter-device
isolation is a property of each driver owning distinct static symbols, not of the
region being subdivided. **Port implication:** the port's single `.ram_nc`
`HashContext`/`CryptoContext` static is the faithful analogue — one driver-owned
symbol in the shared non-cached section, *not* a sub-allocation of a managed
non-cacheable arena. No coordination obligation the port must reproduce.

Coherency for **caller-supplied** buffers is handled separately, by explicit cache
maintenance, *not* by placement: the SG entries point at `pkt->in_buf`
(`hace_aspeed.c:484`), which is not in `.nocache`, and the driver issues
`cache_data_invd_range(data->digest, 64)` (`:448`) / `cache_data_invd_all()` on the
caller path. The port's design note that `.ram_nc` context/`src`/`dst` need no
`cache_data_invd_all` (goal.md §1.9.1 / line 549) is consistent with this: the
non-cached placement covers driver-owned DMA memory; caller buffers are the only
ones needing maintenance, and the port keeps its DMA buffers driver-owned.

### 4.4 IV / endianness (AST1060, little-endian)

- `begin_session` loads the algorithm IV into `digest`: each IV `u32` written in
  native (little-endian) byte order, length `iv_len*4` bytes — 32 B for SHA-256,
  64 B for SHA-384/512.
- Digest readback is the standard digest byte string (engine emits big-endian via
  `SHA_BE_EN`); `digest_size` bytes are copied verbatim to `pkt.out_buf`. This is
  the canonical observable output the port must reproduce.

### 4.5 Digest state machine

- **begin_session** (HAL `start` / first `sha_calculate`): set `method`; load IV
  into `digest`; set `block_size` (64 for SHA-256, 128 for SHA-384/512);
  `bufcnt = 0`; `digcnt = {0,0}`. `buffer`/`digest` content not otherwise cleared
  (scoped semantics).
- **update** (`hash_hndlr finish=false`):
  - `digcnt[0] += in_len` (carry into `digcnt[1]`).
  - If `bufcnt + in_len < block_size`: append to `buffer`, return — no engine pass.
  - Else: build SG list of whole blocks only — `sg[0]` = buffered partial,
    `sg[1]` = block-aligned span of the new input, last entry tagged
    `HACE_SG_LAST`; one DMA pass over the block-aligned `total_len`; copy the
    `in_len % block_size` tail back to `buffer[0..]`, `bufcnt = remaining`.
- **finalize** (`hash_hndlr finish=true`, HAL `finish` / `sha_calculate`):
  `fill_padding(0)`; one `HACE_SG_LAST` descriptor over the padded buffer; final
  pass; copy `digest_size` bytes to `out_buf`; cleanup (zero `bufcnt`, `digcnt`,
  `buffer`, `digest`; `hace30 = 0`).
- **free_session / cancel**: same cleanup; engine idled.

### 4.6 Padding (`fill_padding`)

Standard Merkle–Damgård: append `0x80`; zero-fill to the 56-byte (block 64) /
112-byte (block 128) boundary; then the message **bit-length**, big-endian, as an
8-byte field (block 64) or 16-byte field (block 128, `high = (digcnt[1]<<3) |
(digcnt[0]>>61)`, `low = digcnt[0]<<3`).

---

## 5. Deltas vs. the port — source-verified against the frozen driver

Subject is the authoritative model: the frozen [zephyr-reference/](zephyr-reference/)
`hace_aspeed.c` @ `cfe94dc`, read directly. Final classification (`goal.md` §2):
**D2 is the only intentional delta**; **D1 and D4 are conformance** (the authority
already does what the port does — the prior "vs. `aspeed-rust`" framing measured the
wrong reference); **HMAC** exists in the driver but is gated separately by decision.

| Area | Pinned driver (verbatim, line) | Port (openprot) | Verdict |
|------|--------------------------------|-----------------|---------|
| Busy-wait | **Bounded** 3 s timeout, error returned (`hace_aspeed.c:380–393`, `aspeed_hash_wait_completion(dev,3000)`) | Bounded `poll_budget` → `HaceError::Timeout` | **D1 = conformance.** Authority bounds the wait too; same observable failure (error, no partial output) |
| Exact-block-boundary `bufcnt` | **Has the stale-`bufcnt` bug**: `bufcnt` updated only when `remainder != 0` (`hace_aspeed.c:489–492`) | Always `bufcnt = remaining` (correct SHA) | **D2 = the one intentional delta (keep the fix).** Unreachable: PFR streams 4 KB pages from `bufcnt==0`; DICE uses sub-block updates — see traces below |
| Digest packing | Raw `memcpy` of `SHA_BE_EN` engine output → canonical BE digest (`hace_aspeed.c:513,583,614`) | `from_ne_bytes` → same canonical digest | **D4 = conformance.** Port == authority; `aspeed-rust` *owned* `from_be_bytes` is the bug to avoid |
| HMAC | **Implemented** (`aspeed_hash_setkey`/`aspeed_hash_digest_hmac`); threshold `key_len > block_size` (RFC-2104-correct) | Separate KAT authority, `> block_size` threshold | In driver, but **gated separately by decision** (`goal.md` §2.1); not byte-matched here |
| API shape | C session over one static `hashParams` (Quirks Q1–Q5) | Rust scoped + owned APIs; no `sessionReady`/order quirks | Port improvement, not a gap |
| Output-length check | None (`hash_length` ignored, Q4) | Length from algorithm spec | Port improvement |

**D2 reachability traces (against the frozen source):**

- `hash_device_firmware` / `flash_hash_contents`: `update(4096)` repeatedly from a
  fresh session. 4096 % 64 = 4096 % 128 = 0, and `bufcnt` is `0` entering every
  full page → `remainder == 0`, "don't touch `bufcnt`" correctly leaves it `0`.
  Bug **dormant**; output = standard SHA. (This is why field firmware verifies.)
- DICE (`update(48)`, `update(48)`, SHA-384 block 128): `48+48 < 128` → both hit
  the sub-block early-return; the boundary branch is never reached. Bug **dormant**.
- Trigger needs `bufcnt != 0` *then* an update that exactly fills to a block
  multiple — **no `aspeed-zephyr-project` consumer produces that**.

**Bottom line:** on every reachable input, port output = pinned-driver output =
standard SHA (D1/D4 conformance, D2 dormant). The port and the authority differ on
exactly one unreachable pathological pattern, where the port is correct and the
authority is not — the single accepted delta. No open unknowns: the D2 branch was
read directly from the frozen `hace_aspeed.c:489–492`.
