# AST10x0 ECDSA tests — split layout

Two folders, deliberately separated so Bazel never runs the hardware-only
vectors under QEMU/CI:

| Folder | Runs on | Bazel tag | Picked up by `--config=virt_ast10x0`? |
|--------|---------|-----------|----------------------------------------|
| [`qemu/`](qemu/) | QEMU + hardware | *(none)* | **Yes** — automatable now |
| [`evb/`](evb/) | **Hardware only** (AST1060 EVB) | `["hardware"]` | **No** — excluded |

## Why the split

The AST1060 QEMU SBC model has **no ECC engine** (goal.md ADR-4:
`qemu-ast10x0-i2c/hw/misc/aspeed_sbc.c` is an OTP/secure-boot stub — the
trigger does nothing, `secure014` bit-20/21 never set, the param window is
out of the model's register range). So on QEMU `verify_raw` can only ever
deterministically time out; the accept/reject **verdict is unreachable**.

Therefore the NIST CAVP P-384/SHA-384 known-answer vectors (verdict parity +
correctness, goal.md §4.B) can only be exercised on real silicon and are
**user-executed later**.

## Tagging mechanism (do not change without reading this)

`.bazelrc` line 86:

```
test:virt_ast10x0 --test_tag_filters=-integration,-do_not_build,-do_not_run_test,-kernel_doc_test,-hardware
```

The trailing `-hardware` means any test target tagged `["hardware"]` is
**excluded from `bazel test --config=virt_ast10x0`** (and from CI), while
still building. `evb/` uses that tag plus the same
`//target/ast10x0:qemu_enabled → @platforms//:incompatible` select that
`tests/peripherals/i2c/i2c_init/BUILD.bazel` uses — belt and suspenders so a
QEMU run cannot pick it up.

`qemu/` is **not** tagged `hardware` — it is the automatable subset
(goal.md §4.A): interface/structural rejects, the `#[repr(C)]` operand-order
pin, and the D3 bounded-timeout positive test (on QEMU the engine is absent,
so the timeout firing *is* the assertion).

## Status

- **`qemu/` — IMPLEMENTED & PASSING.** Real pw_kernel `system_image_test`;
  green under `--config=virt_ast10x0` (operand-order pin, structural reject,
  D3 bounded-timeout).
- **`evb/` — IMPLEMENTED, hardware-only.** pw_kernel firmware driving the
  pinned NIST CAVP P-384/SHA-384 SigVer vectors (15) through the real ECC
  engine; builds clean (`--config=k_ast1060_evb`), `hardware`-tagged +
  qemu-incompatible. The **user runs it on an AST1060 EVB** (no board here).
