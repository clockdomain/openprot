# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0
"""Canonical caliptra git pins for this openprot checkout.

Downstream integrators: fork this file to target a different
caliptra release. The uprev tool (`bazel run
//third_party/caliptra:uprev -- <subcommand>`) keeps the derived
Cargo.toml and MODULE.bazel entries in sync with whatever values
are set here.

Layout contract — ONE key per line, quoted string values, no
multi-line strings. The uprev regex-edit path depends on this
format. Do not "improve" the formatting.

Subcommands:
  verify               — cross-check that MODULE.bazel,
                         crates_io/embedded/Cargo.toml, and
                         crates_io/host/Cargo.toml all agree with
                         the values set here plus the derived
                         Cargo.lock in caliptra-mcu-sw.
  bump <new-mcu-sw-sha> — set caliptra_mcu_sw to the given SHA,
                         derive caliptra_sw + caliptra_cfi from
                         its Cargo.lock, update all files.
  latest               — resolve caliptra-mcu-sw HEAD, then bump.
  release <tag>        — resolve a caliptra-mcu-sw tag via
                         `git ls-remote --tags`, then bump, AND
                         set release_tag to the tag string.

NOTE: bzlmod forbids `load()` in MODULE.bazel, so this file is
NOT consumed by MODULE.bazel's caliptra_repos() calls directly.
uprev dual-writes — versions.bzl + Cargo.toml files + MODULE.bazel
are updated in lockstep by every bump/latest/release transaction,
and verify cross-checks them. This is documented in
plans/uprevpy.md under "bump / release / latest — dual-write path".
"""

CALIPTRA_VERSIONS = {
    "caliptra_cfi": "a98e499d279e81ae85881991b1e9eee354151189",
    "caliptra_dpe": "cf3224c41b64789aa556e4f0c78e7395c6528a43",
    "caliptra_mcu_sw": "a8b5eb8cf8ef98279988237e58f3ed9ade0072f5",
    "caliptra_sw": "85981e1bc28662a9a9fa5040cbb38ac46e548217",
    "release_tag": "",
    "ureg": "4762ed5eef323a4267720a6b6da287c88738f901",
}
