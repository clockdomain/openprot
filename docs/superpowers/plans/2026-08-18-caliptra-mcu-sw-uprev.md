# Caliptra-mcu-sw Uprev Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the pinned caliptra-mcu-sw dependency from `b7e45fc1` (429 commits behind) to upstream HEAD `a8b5eb8cf8ef98279988237e58f3ed9ade0072f5`, with all derived pins, crate renames, Bazel overlays, and in-repo consumers updated, and the emulator test suite green.

**Architecture:** This repo consumes caliptra sources three ways, all keyed off `third_party/caliptra/versions.bzl`: (1) `@caliptra_mcu_sw` / `@caliptra_sw` git_repository overlays declared in `third_party/caliptra/extensions.bzl` and compiled by hand-written rules in `third_party/caliptra/caliptra-mcu-sw/BUILD.bazel` and `third_party/caliptra/caliptra-sw/BUILD.bazel`; (2) two crate_universe workspaces (`third_party/caliptra/crates_io/{embedded,host}/Cargo.toml`) that pin individual crates by git rev; (3) `target/veer` Bazel targets (emulator binary, caliptra_runner tooling, emulator-tagged tests) that consume both. The `bazel run //third_party/caliptra:uprev` tool dual-writes versions.bzl and both Cargo.tomls.

**Tech Stack:** Bazel (bzlmod + crate_universe), Rust (riscv32imc firmware + host emulator), Python (uprev tool).

**Spec:** No separate spec doc. The requirement is the user request "uprev caliptra-mcu-sw to the latest upstream commit"; the recon findings baked into this plan (SHAs, rename map, cfi trap) serve as the spec and were verified against upstream on 2026-08-18.

## Global Constraints

- New caliptra_mcu_sw pin: `a8b5eb8cf8ef98279988237e58f3ed9ade0072f5` (upstream HEAD as of 2026-08-18 — bump by explicit SHA, not `latest`, so execution is reproducible).
- Derived pins from that commit's Cargo.lock: caliptra_sw = `85981e1bc28662a9a9fa5040cbb38ac46e548217` (130 commits ahead of current), caliptra-cfi `-git` crates stay at `a98e499d279e81ae85881991b1e9eee354151189` (unchanged), caliptra-dpe = `cf3224c41b64789aa556e4f0c78e7395c6528a43` (diverged history vs current pin — expect API churn).
- `ureg` bazel-module git_override in MODULE.bazel (chipsalliance/caliptra-ureg @ `412ca401`) is independently tracked — do NOT touch it unless a build error forces it; if touched, record why in the commit message.
- versions.bzl layout contract: one key per line, quoted strings — never reformat it.
- All commits: `git commit` with a scope prefix matching repo history (e.g. `third_party/caliptra: ...`), ending with the Claude Code co-author trailer.
- Work on a feature branch (e.g. `uprev-caliptra-mcu-sw`), never directly on `main`.
- Canonical validation commands (same as CI): `./pw presubmit` and `./pw ci`; emulator suite alone: `bazel test --keep_going --build_tag_filters=+emulator,-disabled --test_tag_filters=+emulator,-disabled --test_output=streamed //target/veer/...`.

## Key recon findings (verified 2026-08-18)

1. **Upstream renamed every MCU workspace crate** with a `caliptra-mcu-` prefix (e.g. `romtime` → `caliptra-mcu-romtime`), and upstream source code refers to deps by the new names. Our crates_io dep keys must be renamed (not aliased), because crate_universe derives both the Bazel label and the rustc `--extern` name from the dep key, and the overlay rules compile upstream sources that `use` the new names. Full map in Task 3.
2. **`ureg` (from caliptra-sw) was renamed `caliptra-ureg`.** The plain `ureg` package no longer exists at the new revs.
3. **cfi derivation trap:** `uprev.py` derives the caliptra-cfi pin from the *first* `caliptra-cfi?rev=` match in upstream's Cargo.lock. In the old lock the only caliptra-cfi-repo entries were the `-git` crates (`a98e499d`), so first-match worked. In the new lock, `caliptra-cfi-lib`/`caliptra-cfi-derive` (non-git) also come from the caliptra-cfi repo at `72c75dc7` and sort *before* the `-git` entries — first-match would wrongly rewrite our `-git` pins from `a98e499d` to `72c75dc7`. Upstream's actual `-git` pin is unchanged at `a98e499d`. Task 1 fixes the tool before running it.
4. **All 15 files exported by `third_party/caliptra/caliptra-mcu-sw/overlay.BUILD` still exist** at the new SHA (verified via HTTP HEAD), and the `caliptra_sw_rustcrypto.patch` context (`image/crypto/Cargo.toml` `[features]` block, `default = ["openssl"]`) is unchanged at caliptra-sw `85981e1b`, so the patch should still apply.
5. **Upstream restructured `builder/`** (removed `builder/src/apps.rs`; added `features.rs`, `offline_signing.rs`, `attestation_manifest.rs`, `utils.rs`) and the emulator app gained new deps (`caliptra-hw-model-types`, `caliptra-mcu-core-mailbox-server`, etc.) — the overlay rule dep lists in `third_party/caliptra/caliptra-mcu-sw/BUILD.bazel` will need additions (Task 4).
6. In-repo Rust touching renamed crates directly is minimal: `target/veer/registers/registers.rs` uses `caliptra_mcu_registers_generated::` — which is the overlay rule's explicit `crate_name`, and happily matches the new upstream lib name. Main churn risk is `target/veer/caliptra_emulator_main.rs` and `target/veer/tooling/caliptra_runner.py` against 429 commits of emulator/builder CLI+API drift.

---

### Task 1: Fix uprev.py cfi derivation (package-block-scoped rev extraction)

**Files:**
- Modify: `third_party/caliptra/uprev.py` (add helper near `extract_rev_from_cargo_lock`, ~line 109; use it in `cmd_verify` ~line 412 and `_do_bump_transaction` ~line 558)
- Test: `third_party/caliptra/uprev_test.py`

**Interfaces:**
- Produces: `extract_rev_for_package_from_cargo_lock(lock_contents: str, package: str) -> str | None` — later tasks rely on `bump` deriving the cfi pin from the `caliptra-cfi-lib-git` package block, keeping it at `a98e499d...`.

- [ ] **Step 1: Write the failing test**

Append to `third_party/caliptra/uprev_test.py` (import the new name in the existing `from third_party.caliptra.uprev import (...)` block):

```python
class TestExtractRevForPackageFromCargoLock(unittest.TestCase):
    # Mirrors the real hazard: non-git cfi crates from the same repo URL
    # sort before the -git crates in Cargo.lock.
    LOCK = (
        '[[package]]\n'
        'name = "caliptra-cfi-derive"\n'
        'version = "1.0.0"\n'
        'source = "git+https://github.com/chipsalliance/caliptra-cfi?rev='
        + "7" * 40 + '#' + "7" * 40 + '"\n'
        '\n'
        '[[package]]\n'
        'name = "caliptra-cfi-lib-git"\n'
        'version = "1.0.0"\n'
        'source = "git+https://github.com/chipsalliance/caliptra-cfi.git?rev='
        + "a" * 40 + '#' + "a" * 40 + '"\n'
    )

    def test_scoped_to_named_package_block(self):
        sha = extract_rev_for_package_from_cargo_lock(self.LOCK, "caliptra-cfi-lib-git")
        self.assertEqual(sha, "a" * 40)

    def test_first_match_would_have_been_wrong(self):
        # Documents why the URL-first-match helper is not used for cfi.
        self.assertEqual(
            extract_rev_from_cargo_lock(self.LOCK, "caliptra-cfi"), "7" * 40
        )

    def test_missing_package_returns_none(self):
        self.assertIsNone(
            extract_rev_for_package_from_cargo_lock(self.LOCK, "no-such-package")
        )
```

- [ ] **Step 2: Run test to verify it fails**

Run: `python3 third_party/caliptra/uprev_test.py`
Expected: FAIL with `ImportError: cannot import name 'extract_rev_for_package_from_cargo_lock'`

- [ ] **Step 3: Implement the helper and switch both call sites**

Add to `uprev.py` after `extract_rev_from_cargo_lock`:

```python
def extract_rev_for_package_from_cargo_lock(
    lock_contents: str, package: str
) -> str | None:
    """Extract the git rev from the [[package]] block with the given name.

    Unlike extract_rev_from_cargo_lock (first match by repo URL anywhere in
    the file), this is scoped to one package's block, which matters when
    several packages come from the same repo URL at different revs (e.g.
    caliptra-cfi-lib vs caliptra-cfi-lib-git).
    """
    pattern = re.compile(
        rf'^name = "{re.escape(package)}"\n'
        rf'(?:(?!\[\[package\]\]).)*?'
        rf'source = "git\+[^"]*\?rev=([0-9a-f]{{40}})',
        re.MULTILINE | re.DOTALL,
    )
    m = pattern.search(lock_contents)
    return m.group(1) if m else None
```

In `cmd_verify`, replace:
```python
    expected_cfi = extract_rev_from_cargo_lock(lock_contents, "caliptra-cfi")
```
with:
```python
    expected_cfi = extract_rev_for_package_from_cargo_lock(
        lock_contents, "caliptra-cfi-lib-git"
    )
```

In `_do_bump_transaction`, replace:
```python
    new_cfi = extract_rev_from_cargo_lock(lock_contents, "caliptra-cfi")
```
with:
```python
    new_cfi = extract_rev_for_package_from_cargo_lock(
        lock_contents, "caliptra-cfi-lib-git"
    )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 third_party/caliptra/uprev_test.py` and `bazel test //third_party/caliptra:uprev_test`
Expected: PASS (all existing tests too — the URL-first-match helper is unchanged and still used for caliptra-sw).

- [ ] **Step 5: Commit**

```bash
git add third_party/caliptra/uprev.py third_party/caliptra/uprev_test.py
git commit -m "third_party/caliptra: Scope uprev cfi derivation to the -git package block"
```

---

### Task 2: Mechanical pin bump via the uprev tool

**Files:**
- Modify (by tool): `third_party/caliptra/versions.bzl`, `third_party/caliptra/crates_io/embedded/Cargo.toml`, `third_party/caliptra/crates_io/host/Cargo.toml`

**Interfaces:**
- Consumes: Task 1's fixed cfi derivation.
- Produces: versions.bzl with `caliptra_mcu_sw = a8b5eb8c...`, `caliptra_sw = 85981e1b...`, `caliptra_cfi = a98e499d...` (unchanged); all `rev =` fields in both Cargo.tomls rewritten to match.

- [ ] **Step 1: Run the bump**

Run: `bazel run //third_party/caliptra:uprev -- bump a8b5eb8cf8ef98279988237e58f3ed9ade0072f5`
Expected: summary showing `caliptra_mcu_sw b7e45fc1... -> a8b5eb8c...`, `caliptra_sw 2fe38a09... -> 85981e1b...`, `caliptra_cfi: already at a98e499d... (no change)`.
Note: no local `../caliptra-mcu-sw` checkout exists, so the tool fetches Cargo.lock from raw.githubusercontent.com — network required.

- [ ] **Step 2: Verify exactly what changed**

Run: `git diff --stat && git diff third_party/caliptra/versions.bzl`
Expected: only the three files above changed; versions.bzl shows the two SHA changes and nothing else (cfi line untouched, release_tag stays `""`).

- [ ] **Step 3: Commit**

```bash
git add third_party/caliptra/versions.bzl third_party/caliptra/crates_io
git commit -m "third_party/caliptra: Bump caliptra-mcu-sw to a8b5eb8c (429 commits)"
```

(The tree does not build yet — that's expected and why Tasks 3–5 exist. Committing the mechanical step separately keeps the hand-edits reviewable.)

---

### Task 3: Rename crates_io dep keys to upstream's new crate names + dpe bump + repin

**Files:**
- Modify: `third_party/caliptra/crates_io/embedded/Cargo.toml`
- Modify: `third_party/caliptra/crates_io/host/Cargo.toml`
- Possibly modify: `third_party/caliptra/crates_io/{embedded,host}/Cargo.lock` (regenerated by repin)

**Interfaces:**
- Produces: crate_universe labels named after the new dep keys, e.g. `@rust_caliptra_crates//:caliptra-mcu-romtime`, `@rust_caliptra_crates_host//:caliptra-mcu-registers-generated`, `@rust_caliptra_crates//:caliptra-ureg`. Task 4 rewrites all label references to these names.

- [ ] **Step 1: Rename dep keys in embedded/Cargo.toml**

Rename the key only (left-hand side); `git`/`rev`/features stay as Task 2 wrote them. Verified against upstream's Cargo.lock at the new SHA:

| old key | new key |
|---|---|
| `mcu-error` | `caliptra-mcu-error` |
| `romtime` | `caliptra-mcu-romtime` |
| `registers-generated` | `caliptra-mcu-registers-generated` |
| `mcu-config` | `caliptra-mcu-config` |
| `mcu-config-emulator` | `caliptra-mcu-config-emulator` |
| `mcu-image-header` | `caliptra-mcu-image-header` |
| `flash-image` | `caliptra-mcu-flash-image` |
| `ureg` | `caliptra-ureg` |

- [ ] **Step 2: Bump the three dpe entries in embedded/Cargo.toml**

Lines 32–34 (`caliptra-dpe-platform`, `caliptra-dpe-crypto`, `caliptra-dpe`): change `rev = "f56f66ef4ada62bd99b5670c8384dc2e97e04e94"` to `rev = "cf3224c41b64789aa556e4f0c78e7395c6528a43"` on all three (uprev `verify` enforces their lockstep). This matches what upstream's new Cargo.lock uses. dpe's history diverged from our old pin, so compile errors in dpe-consuming code later are expected, not a sign this step was wrong.

- [ ] **Step 3: Rename dep keys in host/Cargo.toml**

| old key | new key |
|---|---|
| `registers-generated` | `caliptra-mcu-registers-generated` |
| `mcu-config` | `caliptra-mcu-config` |
| `mcu-config-emulator` | `caliptra-mcu-config-emulator` |
| `mcu-config-fpga` | `caliptra-mcu-config-fpga` |
| `flash-image` | `caliptra-mcu-flash-image` |
| `mctp-vdm-common` | `caliptra-mcu-mctp-vdm-common` |
| `pldm-common` | `caliptra-mcu-pldm-common` |
| `pldm-fw-pkg` | `caliptra-mcu-pldm-fw-pkg` |
| `pldm-ua` | `caliptra-mcu-pldm-ua` |
| `mcu-firmware-bundler` | `caliptra-mcu-firmware-bundler` |
| `caliptra-util-host-mailbox-test-config` | `caliptra-mcu-core-util-host-mailbox-test-config` |

- [ ] **Step 4: Add new upstream deps the overlay targets will need**

Add to host/Cargo.toml (caliptra-sw group, same rev `85981e1b...`):

```toml
caliptra-hw-model-types = { git = "https://github.com/chipsalliance/caliptra-sw", rev = "85981e1bc28662a9a9fa5040cbb38ac46e548217", default-features = false }
```

and to the mcu group (rev `a8b5eb8c...`):

```toml
caliptra-mcu-core-mailbox-server = { git = "https://github.com/chipsalliance/caliptra-mcu-sw", rev = "a8b5eb8cf8ef98279988237e58f3ed9ade0072f5", default-features = false }
caliptra-mcu-testing-common = { git = "https://github.com/chipsalliance/caliptra-mcu-sw", rev = "a8b5eb8cf8ef98279988237e58f3ed9ade0072f5", default-features = false }
```

These come from upstream `emulator/app/Cargo.toml` at the new SHA (its `[dependencies]` are the ground truth — re-read it and add anything else the repin/build demands the same way).

- [ ] **Step 5: Repin and iterate until crate resolution succeeds**

Run: `CARGO_BAZEL_REPIN=1 bazel build //target/veer/tooling:caliptra_runner`
Expected on first run: possible resolution errors (missing crate name, missing feature). For each, consult the crate's Cargo.toml in upstream at the new SHA (`https://raw.githubusercontent.com/chipsalliance/caliptra-mcu-sw/a8b5eb8cf8ef98279988237e58f3ed9ade0072f5/<path>/Cargo.toml`) and adjust the dep entry. Compile errors (not resolution errors) are fine at this stage — they belong to Tasks 4–5. Gate for this task: crate_universe resolution/repin completes.

- [ ] **Step 6: Commit**

```bash
git add third_party/caliptra/crates_io
git commit -m "third_party/caliptra: Rename crates_io deps to upstream's caliptra-mcu-* names"
```

---

### Task 4: Update Bazel labels and overlay rule dep lists

**Files:**
- Modify: `third_party/caliptra/BUILD.bazel` (the `crate_<name>` alias loop near line 30)
- Modify: `third_party/caliptra/caliptra-mcu-sw/BUILD.bazel` (all rule dep lists)
- Modify: `third_party/caliptra/caliptra-sw/BUILD.bazel` (ureg label, any new deps)
- Possibly modify: `third_party/caliptra/caliptra-mcu-sw/overlay.BUILD` (only if a build error shows a needed source file isn't exported — all currently exported files verified present upstream)
- Modify: `target/veer/BUILD.bazel` and any other file found by the grep in Step 1

**Interfaces:**
- Consumes: Task 3's new crate_universe label names.
- Produces: `bazel build`-able overlay targets with unchanged target names (`emulator_lib`, `mcu_testing_common`, `caliptra_emu_cpu`, `firmware_registers_generated` with `crate_name = "caliptra_mcu_registers_generated"`, etc.) so Task 5 consumers keep their labels.

- [ ] **Step 1: Find every stale label**

Run:
```bash
grep -rn -E '@rust_caliptra_crates(_host)?//:(ureg|mcu-error|romtime|registers-generated|mcu-config[a-z-]*|mcu-image-header|flash-image|mctp-vdm-common|pldm-(common|fw-pkg|ua)|mcu-firmware-bundler|caliptra-util-host-mailbox-test-config)\b' \
  --include='*.bazel' --include='*.bzl' --include='BUILD*' .
```
Rewrite each hit to the new name from the Task 3 tables (e.g. `@rust_caliptra_crates_host//:registers-generated` → `@rust_caliptra_crates_host//:caliptra-mcu-registers-generated`). Also update any crate-name strings in the `crate_<name>` alias loop in `third_party/caliptra/BUILD.bazel`.

- [ ] **Step 2: Reconcile each overlay rule's deps with upstream's manifests**

For every `rust_library`/`rust_binary` in `third_party/caliptra/caliptra-mcu-sw/BUILD.bazel` (at minimum: `emulator_registers_generated`, `firmware_registers_generated`, `emulator_bmc`, `emulator_caliptra`, `emulator_consts`, `emulator_periph`, `emulator_mcu_mbox`, `caliptra_mailbox_server`, `emulator_lib`, plus the rom/testing/builder targets further down): fetch the corresponding upstream `Cargo.toml` at the new SHA and make the Bazel `deps` list match its `[dependencies]` — add new entries (e.g. `emulator_lib` now needs `caliptra-hw-model-types` and the renamed mailbox-server/testing-common crates), drop removed ones. Where an upstream dep is another overlay target (workspace-internal crate), depend on that overlay target, not crates_io.

- [ ] **Step 3: Build the third_party package until green**

Run: `bazel build //third_party/caliptra/...`
Expected: PASS after iterating on Step 2. Missing-module errors ("unresolved import") in overlay-compiled upstream code mean a dep or an exported source file is missing — fix in the BUILD/overlay.BUILD, never by patching upstream source. If the `caliptra_sw_rustcrypto.patch` fails to apply, regenerate it against `image/crypto/Cargo.toml` at `85981e1b` (context was verified unchanged, so this is unlikely).

- [ ] **Step 4: Commit**

```bash
git add third_party/caliptra target/veer/BUILD.bazel
git commit -m "third_party/caliptra: Update overlay BUILD rules for upstream crate renames"
```

---

### Task 5: Fix in-repo consumers (target/veer)

**Files:**
- Modify: `target/veer/caliptra_emulator_main.rs` (drives `emulator_lib` — 429 commits of API drift land here)
- Modify: `target/veer/tooling/caliptra_runner.py` and `target/veer/tooling/caliptra_runner.bzl` (emulator/builder CLI flags may have changed; upstream `builder/src` was restructured)
- Check (likely no change): `target/veer/registers/registers.rs` — its `caliptra_mcu_registers_generated::` paths match the overlay `crate_name`, which now equals the upstream lib name

**Interfaces:**
- Consumes: Task 4's overlay targets (`//third_party/caliptra/caliptra-mcu-sw:emulator_lib`, `:mcu_testing_common`, `//third_party/caliptra/caliptra-sw:caliptra_emu_cpu`).
- Produces: a runnable `caliptra_emulator` binary and `caliptra_runner` tooling used by every emulator-tagged test in Task 6.

- [ ] **Step 1: Build the consumers, fix compile errors**

Run: `bazel build //target/veer/tooling:caliptra_runner && bazel build --build_tag_filters=+emulator,-disabled //target/veer/...`
For each error in `caliptra_emulator_main.rs`, read the new signature at its definition in the fetched `@caliptra_mcu_sw` sources (`bazel info output_base`, then `external/*caliptra_mcu_sw/emulator/app/src/`) and adapt the call site — preserve current behavior (same peripherals, same ROM/runtime images, same exit conditions), don't chase new upstream features.

- [ ] **Step 2: Smoke-run the emulator tooling**

Run: `bazel run //target/veer/tooling:caliptra_runner -- --help` (or the invocation `caliptra_runner.bzl` generates — read the `.bzl` to get the exact form).
Expected: help text / clean start, no Python tracebacks. If upstream renamed emulator CLI flags, fix `caliptra_runner.py` to match `emulator/app/src/main.rs`'s clap definitions at the new SHA.

- [ ] **Step 3: Commit**

```bash
git add target/veer
git commit -m "target/veer: Adapt emulator main and runner tooling to new caliptra-mcu-sw API"
```

---

### Task 6: Emulator test suite green

**Files:**
- Check: `third_party/caliptra/caliptra-sw/{rom.ld,link.x,fmc_memory.x,runtime_memory.x}` — local copies of upstream linker/memory scripts; diff against caliptra-sw at `85981e1b` and sync if upstream moved sections or grew regions
- Possibly modify: `target/veer/tests/**`, `target/veer/entry.rs`, `target/veer/config.rs` (only if a test failure traces to a real behavioral change upstream, e.g. interrupt delivery or console changes)

**Interfaces:**
- Consumes: everything above.
- Produces: the CI gate this uprev is judged by.

- [ ] **Step 1: Diff the vendored linker scripts against upstream**

For each of the four files, find its upstream counterpart in caliptra-sw at `85981e1b` (search the repo for the filename; e.g. `rom/rom.ld`) and diff. Port upstream's changes while keeping any local edits (each local edit should be explainable — if one isn't, flag it in the PR description rather than silently dropping it).

- [ ] **Step 2: Run the emulator suite**

Run: `bazel test --keep_going --build_tag_filters=+emulator,-disabled --test_tag_filters=+emulator,-disabled --test_output=streamed //target/veer/...`
Expected: PASS. On failure: REQUIRED SUB-SKILL `superpowers:systematic-debugging` — no speculative edits. Likely suspects given the delta: MEIVT/interrupt behavior (the recently added `target/veer/tests/interrupts` test), mailbox protocol changes, boot-flow/ROM handoff changes.

- [ ] **Step 3: Commit any fixes**

```bash
git add -A
git commit -m "target/veer: Fix emulator tests for caliptra-mcu-sw a8b5eb8c behavior changes"
```

---

### Task 7: Full verification and PR

- [ ] **Step 1: Tool-level consistency check**

Run: `bazel run //third_party/caliptra:uprev -- verify`
Expected: all `OK:` lines, ending `==> All derived SHAs are consistent.` — including `OK: caliptra_cfi = a98e499d...` (proves Task 1's fix) and the dpe lockstep line showing `cf3224c4...`.

- [ ] **Step 2: Run what CI runs**

Run: `./pw presubmit && ./pw ci`
Expected: PASS. (`ci` includes the `caliptra_emulator_tests` workflow from workflows.json; `presubmit` covers formatting/license checks.)

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin uprev-caliptra-mcu-sw
gh pr create --title "Uprev caliptra-mcu-sw to a8b5eb8c" --body "..."
```
PR body must include: old→new SHAs for mcu-sw/sw/dpe, the crate-rename summary, the cfi derivation fix and why, any behavioral test fixes from Task 6, and the note that ureg's git_override was left untouched (or why it wasn't).
REQUIRED SUB-SKILL before claiming done: `superpowers:verification-before-completion`.

---

## Fallback strategy

If Tasks 4–6 turn into an unbounded fix cascade (a sign 429 commits is too big a single step), fall back to bisecting the uprev: pick an intermediate upstream SHA (e.g. the commit just before the great crate rename, findable via `git log --follow -- romtime/Cargo.toml` in a scratch clone of caliptra-mcu-sw), land that as its own PR with this same task structure, then repeat toward HEAD. The uprev tool supports any SHA via `bump`.
