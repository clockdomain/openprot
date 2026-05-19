# How aspeed-zephyr hashes flash regions — evidence

*Consumer-behavior note for the HACE port. Complements `zephyr-behavior.md`
and `goal.md` §1.3/§5. Question answered: when the deployed PFR firmware
measures a flash region, does HACE DMA from flash? Every claim is cited
`repo:file:line` (paths under `/home/ferro/work/peripherals/`).*

## Answer (one line)

**No flash→HACE DMA.** Two stages: (1) the SPI flash driver copies the
region, in 4 KB pages, into a **non-cached aligned RAM staging buffer**;
(2) HACE scatter-gather-DMAs from **that RAM buffer**, never from flash.

## Evidence

### Stage 1 — SPI flash → non-cached RAM page buffer

- Staging buffer is non-cached + 16-aligned, `PAGE_SIZE`:
  `aspeed-zephyr-project/apps/aspeed-pfr/src/pfr/pfr_util.c:47`
  `uint8_t buffer[PAGE_SIZE] NON_CACHED_BSS_ALIGN16;` (also `:155`, `:167`).
- `PAGE_SIZE == 0x1000` (4 KB):
  `aspeed-zephyr-project/apps/aspeed-pfr/src/cerberus_pfr/cerberus_pfr_definitions.h:64`
  (and `intel_pfr/intel_pfr_definitions.h:105`). Matches the
  production-dominant 4 KB-aligned streaming path in `goal.md` §5.
- Region-hash entry: `get_hash()` →
  `flash_hash_contents(flash, start_address, length, …)`
  `pfr_util.c:184` / `:194` (Cerberus chunked "read block → hash update →
  repeat" helper; its body is in the vendored Cerberus lib, not in this
  tree — see *Confidence* below).
- Flash read path is the SPI flash driver, not HACE:
  `pfr_util.c:49` `pfr_spi_read()` → `bmc_pch_flash_read` (`:54`) /
  `rot_flash_read` (`:56`) →
  `aspeed-zephyr-project/lib/hrot_hal/flash/flash_aspeed.c:279/283/288`
  `flash_read(flash_dev, address, data, data_length)` (Zephyr flash API →
  Aspeed SPI controller). The bytes land in the RAM buffer above.

### Stage 2 — RAM buffer → HACE via the engine's scatter-gather DMA

- The buffer pointer is handed to the HACE hash session as `pkt.in_buf`:
  `aspeed-zephyr-project/lib/hrot_hal/crypto/hash_aspeed.c:93-95`
  `hashParams.pkt.in_buf = (uint8_t *)data; … hash_update(&ctx, &pkt);`
  (and the one-shot form `:34-44`,
  `hash_engine_sha_calculate`). `data` is the Stage-1 RAM buffer.
- `hash_update` enters the pinned Zephyr `aspeed_hace` driver
  (`zephyr/drivers/crypto/hace_aspeed.c`), which builds the hash SG
  descriptor list pointing at that buffer and the HACE engine DMAs from it.
  The hash SG/DMA model (SG list + `digest`/`buffer` as the **non-cached**
  DMA targets, `program_hash_operation(sg_addr, digest_addr, len, method)`)
  is the pinned spec already recorded in this port: `goal.md` §1.3
  (memory model) and §1.5 (engine pass). HACE's SG `addr` is just a pointer
  — here it points at the RAM staging buffer, not a flash window.

## Why the buffer is `NON_CACHED_BSS_ALIGN16`

It is a **HACE DMA target**: the engine reads it by bus-master DMA, so it
must be non-cached and aligned — the same discipline the port applies to
`HashContext` (`.ram_nc`, `#[repr(C, align(64))]`; `goal.md` §1.3/§5.1).
This is corroborating evidence for the port's non-cached-context invariant.

## What this does NOT establish (honest scope)

- **The SPI side.** Whether `flash_read` itself uses SPI DMA, a FIFO, or
  memory-mapped/XIP reads is the *SPI controller's* concern — a different
  engine, not traced here. The claim is only that flash is staged into RAM
  *before* HACE touches it.
- **`flash_hash_contents` internals.** Its chunk loop lives in the vendored
  Cerberus library (not present in this checkout). The two-stage shape is
  established from the call sites and the staging buffer; the per-iteration
  chunk size inside Cerberus was not read line-by-line.
- The `src_sg.addr = (uint32_t)in_buf` lines at `hace_aspeed.c:130-132` are
  the **AES/cipher** path (`aspeed_aes_crypt`, `:105`), *not* the hash path
  — cited here only to disclaim: hash SG wiring is `aspeed_hash_*` in the
  same file; this note relies on the port's already-pinned §1.3 hash model
  rather than re-deriving it.

## Consequence for the port

Confirms `goal.md` §5: HACE measurement is **bounded, run-to-completion,
RAM-staged 4 KB streaming** — the engine never holds a session open across a
flash/SPI yield, and the input it DMAs is always a non-cached RAM page, not
flash MMIO. Reinforces the §5.2 decision (whole-object RPC, no held-across-
yield session) and the non-cached-buffer invariant.
