# ecdsa / evb — HARDWARE-ONLY NIST KAT (goal.md §4.B)

Runs on the **AST1060 EVB** (physical board) only — the user executes it.

**Status: IMPLEMENTED.** pw_kernel `system_image` firmware that drives the
pinned NIST CAVP P-384/SHA-384 SigVer vectors through the real ECC engine
and checks every accept/reject verdict. Builds clean
(`--config=k_ast1060_evb`). Not run here: the QEMU SBC has no ECC engine
(goal.md ADR-4) so it would time out on every vector — hence `hardware`-
tagged + qemu-incompatible (excluded from `--config=virt_ast10x0` and CI).

## Vectors (correctness authority — goal.md §2.3.2)

NIST CAVP **CAVS 11.0 FIPS 186-3 ECDSA** `SigVer.rsp`, section
`[P-384,SHA-384]`, **15 records** (3 valid + 12 invalid: Message/R/S/Q
changed). Independent of the parity reference (Zephyr) — this is the
*correctness* authority, not parity.

- `nist-reference/SigVer_P384_SHA384.rsp.txt` — verbatim vendored section.
- `nist-reference/PINNED.txt` — source URL, retrieval date, sha256 of the
  full `SigVer.rsp` **and** of the vendored section.
- `vectors.rs` — GENERATED from the above: `Qx/Qy/R/S` verbatim NIST;
  `m = SHA-384(hex-decode(Msg))` computed at vendoring time (NIST gives the
  raw message, our engine consumes the digest); `Result P⇒true, F⇒false`.

Running this on hardware also behaviorally discharges P5-OPEN-A (the SRAM
base is actually consumed correctly) and the poll-budget tuning obligation
(goal.md §2.1 residual).

## Run on hardware

```bash
AST1060_EVB_PI_HOST=<pi-host-or-ip> \
  bazel test --config=k_ast1060_evb \
    --nocache_test_results --test_output=streamed \
    --test_timeout=300 --curses=no --noshow_progress \
    //target/ast10x0/tests/peripherals/sbc/ecdsa/evb:ecdsa_evb_kat_test
```

Expected: `vec[i] valid|invalid (<note>): PASS` for all 15, then
`=== all 15 NIST vectors PASS ===` and `TEST_RESULT:PASS`. Any
`engine timeout` line ⇒ the engine wedged (or wrong SRAM base) — that is a
real failure, not a QEMU artifact.

## Refresh the vectors

Re-fetch `SigVer.rsp`, re-verify both sha256 in `nist-reference/PINNED.txt`,
re-run the generator (the `python3` step recorded in the commit) to
regenerate `vectors.rs`. Keep `Qx/Qy/R/S` verbatim; never hand-edit.
