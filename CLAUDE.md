# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

AGENTS.md is the repo's tracked agent guidance and its rule applies here too:
if `docs/src/`, `workflows.json` or a README states something, point at it
rather than restating it. What follows is only what those sources leave
implicit or scatter across several files.

## Build & test

Bazel-only. There is **no Cargo workspace**; `cargo build`/`cargo test` do not
work. Everything goes through `bazelisk` (`bazel` below) or the Pigweed workflow
launcher `./pw`, which runs the groups in `workflows.json`.

```bash
./pw presubmit      # CI gate: format check, MODULE.bazel.lock check, license/include-guard/rust_features checks, clippy
./pw format         # apply rustfmt / black / buildifier / clang-format
./pw ci             # host tests + AST10x0 QEMU + Earlgrey QEMU + Caliptra emulator + FPGA build-only + zero-test audit
./pw default        # wildcard build

bazel test //services/orchestrator/sm:orchestrator_sm_test                 # one target
bazel test //services/pldm:firmware_update_host_test --test_output=all
bazel test //... --test_tag_filters=-hardware,-disabled,-verilator,-emulator,-qemu,-fpga -- -//third_party/caliptra/...
bazel build //docs                                                          # mdbook
```

Wildcard builds **must** exclude `//third_party/caliptra/...` (its
crate_universe workspaces collide with the host one) and filter the tags above.
`workflows.json` is authoritative for which filters each group applies;
`docs/src/usage.md` explains the tags.

Emulated and hardware test runs select a platform with `--config`:

| config | runs on |
|---|---|
| `virt_ast10x0` | AST10x0 under QEMU (`bazel test --config=virt_ast10x0 //target/ast10x0/...`) |
| `k_ast1060_evb` | AST1060 EVB over a lab host (`AST1060_EVB_PI_HOST`), serialized |
| `k_earlgrey` | Earlgrey images (QEMU tests are tagged `qemu`, verilator tests `verilator`) |
| `k_veer` / `fpga` | Caliptra MCU emulator / FPGA (`fpga` tests tagged `fpga`) |

Clippy is a `rules_rust` aspect, not a target:

```bash
bazel build --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect \
  --output_groups=clippy_checks -- //... -//third_party/caliptra/...
```

Presubmit traps that are easy to hit:

- Cargo features are set with `crate_features`, never the Bazel `features`
  attribute; `presubmit/rust_features.py` rejects the latter because it is
  silently ignored and compiles `#[cfg(feature)]` code out.
- `./pw ci` fails a `rust_test` that ran zero tests (`presubmit/zero_tests.py`),
  so a test file whose cases are all `cfg`-stripped is a CI failure, not a pass.
- `target/app_no_panics_test.bzl` fails a `pw_kernel` system image if any
  userspace app links `core::panicking::*`. Panic-freedom is checked by the
  linker, not just by review.
- Crate dependencies come from three pinned `crate_universe` hubs, not per-crate
  manifests: add to `third_party/crates_io/Cargo.toml` (+ `Cargo.lock`) and
  depend on `@rust_crates//:<name>`. Caliptra crates live in
  `third_party/caliptra/crates_io/{embedded,host}` (`@rust_caliptra_crates`,
  `@rust_caliptra_crates_host`).

## Code constraints

`.github/copilot-instructions.md` is the enforced review checklist;
`docs/src/coding-style.md` summarizes it. The short form, because every change
is judged against it: `no_std`, panic-free (no `unwrap`/`expect`/`panic!`/`[]`
indexing), no heap (`heapless` and fixed arrays), checked arithmetic, MMIO only
through HAL traits with volatile access, `// SAFETY:` on every `unsafe`,
`subtle` for secret comparison and `zeroize` for secret lifetime. Untrusted IPC
input is validated in the server, never in the driver. Every source and build
file carries the Apache-2.0 / SPDX header pair (`presubmit/license.py`).

## Architecture

`docs/src/architecture.md` is the long form. The split that explains where code
goes is contracts vs. implementations, so protocol logic is host-testable
without a PAC or kernel:

- `hal/{blocking,async,nb}/` are hardware traits by execution model. Most
  blocking traits are modules of one crate (`openprot_hal_blocking`); the flash
  HAL is its own two-crate directory, `hal/blocking/flash/`
  (`FlashDriver` start-poll-complete contract + `Flash`/`BlockingFlash`
  facade).
- `platform/traits/` and `platform/impls/{linux,tock,hubris,baremetal,rustcrypto}/`
  are OS abstraction.
- `services/{flash,i2c,i3c,mctp,orchestrator,pldm,spdm,telemetry}/` are
  OS-agnostic. Each follows the seam `api/` · `client/` + `client-ipc/` ·
  `server/` + `server-runtime/`, so the codec and dispatch under host test is
  the code that ships. Each service and driver directory has a `README.md`
  documenting its seams and invariants; read it before changing the crate.
- `target/{ast10x0,earlgrey,veer}/` hold silicon backends, peripherals, linker
  scripts, `defs.bzl` image helpers, and the emulator/hardware tests, so host
  wildcard builds never pull in target toolchains. `drivers/usart/` is the
  pattern for a target-agnostic driver contract with its backend under
  `target/ast10x0/backend/`.
- `util/` holds the shared plumbing the services lean on: `util/ipc` (channel
  roles over `pw_kernel`), `util/service` (the deferred-reply server loop),
  `util/region` (MPU region ownership tokens generated from `system.json5`),
  `util/error` (per-module error codes over `pw_status`).

`veer` runs on Pigweed's `pw_kernel`: process topology is declared in
`system.json5` and assembled by `system_image()`; IPC returns
`Result<T, pw_status::Error>`. See `docs/src/design/pw-kernel-ipc.md`,
`docs/src/design/thread-priorities.md` (servers strictly above their clients),
and `target/veer/ipc/`.

### Orchestrator

`services/orchestrator/` is the eRoT boot-sequence controller and the most
opinionated subsystem. Read `docs/src/design/orchestrator/` before touching it.
The invariants that are not obvious from any one file:

- `sm/` is a **pure reducer**: it consumes `Event`s and emits `Effect`s and
  performs no I/O. Handlers only call `ctx.emit(Effect::…)`; all outside
  information arrives in event payloads. Internal follow-ups are emitted as
  `Effect::Emit(event)` so they appear in the effect trace rather than as
  hidden state changes.
- `capabilities/` holds the narrow device contracts effects execute against
  (`BootControl`, `BootWatch`, `Updatable`, `SvnFloor`, `LockdownLatch`, …).
  It is a dependency-free leaf whose `Error` types are bounded only by
  `core::error::Error`, so mocks and IPC proxies can implement them without a
  HAL. Adapters live in `adapters/{hal,pldm,walk}`.
- `config/` is the board device-table *schema*; values live in
  `target/<board>/devices.rs` and are validated at compile time. The core
  decides *what*, never *how*: porting to a new board is a new table plus HAL,
  not a policy change.

## Bazel conventions

- `rust_library` with `crate_name = "openprot_<thing>"` (older crates use
  `hal_<thing>`), `edition = "2024"`.
- Host tests: `rust_test(crate = ":<lib>")` for unit tests; a separate
  `rust_test` with `crate_root` for integration tests under `tests/`.
- Target-only crates set `target_compatible_with = TARGET_COMPATIBLE_WITH` from
  the target's `defs.bzl` so host wildcard builds skip them, and use that file's
  image helpers (`veer_system_image`, `veer_unittest_image`, …) instead of
  hand-rolled `rust_binary` rules.

## Contributing

Large changes (architecture, boot process, APIs, security posture) need an
approved RFC filed as a GitHub issue titled `[RFC] …` before PRs land
(`docs/src/development-process.md`). The main ruleset requires linear history,
so rebase on `main` before pushing.
