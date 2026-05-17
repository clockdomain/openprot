# ecdsa / qemu — automatable subset (goal.md §4.A)

Runs under `bazel test --config=virt_ast10x0` (NOT tagged `hardware`).
The ECC engine is absent on QEMU (parent README / ADR-4), so these test
only what does not need a verdict:

1. **Interface/structural reject** — the port (not the trait) rejects
   malformed `r/s`/point; the HAL trait only guarantees non-zero
   (goal.md §2.3.3). Pure logic.
2. **Operand-order pin (mandatory)** — assert `#[repr(C)]` field order of
   `P384PublicKey`/`P384Signature` ↔ the SRAM operand offsets in
   `registers.rs::start_verify` (goal.md §1.2 step 5), so a future HAL
   struct refactor cannot silently break parity via `as_bytes()`.
3. **D3 bounded-timeout (positive test)** — drive `EcdsaOp::verify_raw`;
   QEMU never sets `secure014` bit-20, so it MUST return
   `EcdsaError::Timeout` within the poll budget. Asserting that *is* the
   test of the lone intentional delta (goal.md §2.1 / D3).

## Status: IMPLEMENTED & PASSING

`target.rs` is a pw_kernel `system_image` firmware (mirrors
`tests/peripherals/i2c/i2c_init/`); tagged `qemu_only` +
`TARGET_COMPATIBLE_WITH` (runs under `virt_ast10x0`, excluded on the EVB —
`-qemu_only` in the `k_ast1060_evb` filter). Never add a verdict assertion
here (impossible on QEMU; that lives in `../evb/`).

## Run

```bash
bazel test --config=virt_ast10x0 \
  --nocache_test_results --test_output=streamed --test_timeout=180 \
  --curses=no --noshow_progress \
  //target/ast10x0/tests/peripherals/sbc/ecdsa/qemu:ecdsa_qemu_test
```

Expected: `operand-order pin: PASS`, `structural reject: PASS`,
`D3 bounded-timeout: PASS`, then `TEST_RESULT:PASS`.
