# Build Fixes for `//target/ast1060-evb/mctp:mctp`

This document explains the changes required to fix the Bazel build:

```
bazel build --config=k_ast1060_evb //target/ast1060-evb/mctp:mctp
```

---

## 1. Patch `openprot-hal-blocking` to a valid remote revision

**File:** `third_party/crates_io/crates_no_std/Cargo.toml`

**Problem:**
The `aspeed-ddk` crate (fetched from the `i2c-core` branch of `OpenPRoT/aspeed-rust`)
depends on `openprot-hal-blocking` from `https://github.com/OpenPRoT/openprot`.
However, commit `7da9c93` on that repo removed all `Cargo.toml` files — including
`hal/blocking/Cargo.toml` — so Cargo can no longer find the crate at HEAD.

This caused the error:
```
error: no matching package named `openprot-hal-blocking` found
location searched: Git repository https://github.com/OpenPRoT/openprot
```

**Fix:**
Added a `[patch]` section that redirects the `openprot-hal-blocking` dependency
to the `rusty1968/openprot` fork at revision `c6cd23a` (the last commit before
the Cargo.toml files were removed). Cargo requires patches to point to a
*different* source URL, so we cannot patch `OpenPRoT/openprot` with itself at a
different rev — hence the use of the `rusty1968` fork (which shares the same
commit history).

```toml
[patch."https://github.com/OpenPRoT/openprot"]
openprot-hal-blocking = { git = "https://github.com/rusty1968/openprot.git", rev = "c6cd23a" }
```

---

## 2. Add missing explicit dependencies for Bazel targets

**File:** `third_party/crates_io/crates_no_std/Cargo.toml`

**Problem:**
The `alias_hub.BUILD` file defines Bazel aliases that route crate names like
`heapless`, `nb`, `fugit`, etc. to `@oot_crates_no_std//:$CRATE`. Bazel's
`crate_universe` only generates top-level targets for *direct* dependencies
listed in `Cargo.toml`. These crates were only transitive dependencies of
`aspeed-ddk`, so `crate_universe` did not expose them as named targets.

This caused errors like:
```
no such target '@@rules_rust++crate+oot_crates_no_std//:heapless'
```

**Fix:**
Added the following as explicit dependencies in `Cargo.toml` so that
`crate_universe` generates the corresponding Bazel targets:

| Crate               | Version / Source                          |
|----------------------|-------------------------------------------|
| `heapless`           | `0.8.0`                                   |
| `hex-literal`        | `0.4`                                     |
| `nb`                 | `1.1.0`                                   |
| `fugit`              | `0.3.7`                                   |
| `openprot-hal-blocking` | `rusty1968/openprot.git` @ `c6cd23a`   |
| `proposed-traits`    | `rusty1968/proposed_traits.git` @ `8564131` |

After adding these, `Cargo.lock` was regenerated with `cargo generate-lockfile`.

---

## 3. Rename `mctp_stack` to `mctp_lib` in source code

**Files:** 8 files under `services/mctp/`

- `services/mctp/server/src/lib.rs`
- `services/mctp/server/src/server.rs`
- `services/mctp/server/src/main.rs`
- `services/mctp/server/tests/dispatch.rs`
- `services/mctp/server/tests/echo.rs`
- `services/mctp/transport-i2c/src/lib.rs`
- `services/mctp/transport-i2c/src/sender.rs`
- `services/mctp/transport-i2c/src/receiver.rs`

**Problem:**
The source code used `mctp_stack::` to refer to types from the `mctp-lib` crate
(e.g., `mctp_stack::Sender`, `mctp_stack::Router`, `mctp_stack::i2c::MctpI2cEncap`).
However, the crate is named `mctp-lib` in its `Cargo.toml`, which Rust normalizes
to `mctp_lib`. Bazel's `crate_universe` generated the target with crate name
`mctp_lib`, not `mctp_stack`, so the compiler could not resolve the imports.

This caused:
```
error[E0432]: unresolved import `mctp_stack`
  = help: use of unresolved module or unlinked crate `mctp_stack`
```

**Fix:**
Replaced all occurrences of `mctp_stack` with `mctp_lib` across the 8 affected
source files. The crate was likely named `mctp-stack` or aliased as `mctp_stack`
in a previous build system configuration; the Bazel build uses the canonical
crate name derived from the package name.

---

## 4. Increase vector table size in system configuration

**File:** `target/ast1060-evb/mctp/system.json5`

**Problem:**
The vector table region was sized at 1184 bytes (0x4A0), which was calculated
for the interrupt vectors plus kernel annotations. With 3 apps (I2C server,
MCTP server, MCTP echo client), each contributing thread and stack annotations,
the `.pw_kernel.annotations.stack` section grew to span `[0x474, 0x4C3]` —
36 bytes past the 0x4A0 boundary. This caused the linker to fail:

```
ld.lld: error: unable to move location counter (0x4c4) backward to 0x4a0
        for section '.VECTOR_TABLE.unused_space'
ld.lld: error: section '.pw_kernel.annotations.stack' will not fit in region
        'VECTOR_TABLE': overflowed by 36 bytes
```

**Fix:**
Increased `vector_table_size_bytes` from 1184 (0x4A0) to 1280 (0x500) and
adjusted `flash_start_address` from 0x4A0 to 0x500 accordingly. The kernel
flash size was recalculated as `0x20000 - 0x500 = 130816` bytes. This provides
96 bytes of headroom for the annotations section.

```json5
arch: {
    vector_table_size_bytes: 1280,  // was 1184
},
kernel: {
    flash_start_address: 0x00000500,  // was 0x000004A0
    flash_size_bytes: 130816,         // was 129888
},
```

---

## Running Unit Tests

The MCTP crates have unit tests that can be run individually:

```
bazel test //services/mctp/server:mctp_server_test
bazel test //services/mctp/api:mctp_api_test
```

**Note:** Using the wildcard `bazel test //services/mctp/...` will fail because
the wildcard also picks up kernel-target binaries (`mctp_server`, `mctp_echo`)
that depend on `pw_kernel/userspace`. The `userspace` crate imports
`kernel_config`, which is a crate generated by the `target_codegen` rule during
a full system image build (e.g., `//target/ast1060-evb/mctp:mctp`). It is not
available when building individual targets outside that context.

To run all MCTP unit tests without hitting this, target the test rules
explicitly:

```
bazel test //services/mctp/server:mctp_server_test //services/mctp/api:mctp_api_test
```
