# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0
"""Definitions shared by every OpenPRoT pw_kernel target, in-tree or not.

Everything here is anchored with `Label()` so it resolves the same way when
loaded as `//target:defs.bzl` from this repository and as
`@openprot//target:defs.bzl` from a downstream module. See
docs/src/out-of-tree-targets.md for the downstream workflow.
"""

load("@pigweed//pw_build:merge_flags.bzl", "flags_from_dict")
load("@pigweed//pw_kernel:flags.bzl", "KERNEL_DEVICE_COMMON_FLAGS")

INCOMPATIBLE = [str(Label("@platforms//:incompatible"))]

_PW_KERNEL_TARGET_SETTING = str(Label("@pigweed//pw_kernel/target:target"))
_PW_KERNEL_TARGET_NONE = str(Label("@pigweed//pw_kernel/target:none"))
_SOC_SETTING = str(Label("//target:soc"))

# `target_compatible_with` value for crates that run on any pw_kernel target
# (kernel-tagged libraries, userspace service runtimes, IPC clients) but never
# on the host. A platform that does not declare a pw_kernel target value gets
# pw_kernel's default value `none`, so this is precise: it admits every
# platform declared with `openprot_target()` below, whichever repository it
# lives in, and rejects host and non-kernel platforms.
PW_KERNEL_TARGET_COMPATIBLE_WITH = select({
    _PW_KERNEL_TARGET_NONE: INCOMPATIBLE,
    "//conditions:default": [],
})

def soc_compatible_with(soc):
    """`target_compatible_with` value for SoC support crates.

    Args:
      soc: Label of a `constraint_value` declared with `openprot_soc()`. Pass
        a `str(Label(...))` from a `.bzl` file so the label is anchored to the
        declaring repository.

    Returns:
      A `select()` that is compatible only with platforms carrying `soc`.
    """
    return select({
        soc: [],
        "//conditions:default": INCOMPATIBLE,
    })

def openprot_soc(name, visibility = ["//visibility:public"]):
    """Declares the `//target:soc` constraint value `soc_<name>`.

    Call this once in the package that hosts an SoC's support crates and gate
    those crates with `soc_compatible_with()` on the resulting value.

    Args:
      name: SoC name, e.g. `ast10x0`. The constraint value is `soc_<name>`.
      visibility: Visibility of the constraint value. Public by default so
        downstream boards can list it in their `platform()`.
    """
    native.constraint_value(
        name = "soc_" + name,
        constraint_setting = _SOC_SETTING,
        visibility = visibility,
    )

def openprot_target(
        name,
        soc,
        constraint_values,
        flags = {},
        visibility = ["//visibility:public"],
        **kwargs):
    """Declares a pw_kernel target: a constraint value and a `platform()`.

    Creates `constraint_value` `target_<name>` on
    `@pigweed//pw_kernel/target:target` and a `platform()` named `name` that
    carries it, the SoC value, `@platforms//os:none`, Pigweed's `no_std`
    constraint and `constraint_values`. The platform's flags start from
    Pigweed's `KERNEL_DEVICE_COMMON_FLAGS`; `flags` adds to or overrides them
    and must at least set the kernel config and console backend:

        openprot_target(
            name = "my_board",
            soc = "@openprot//target/ast10x0:soc_ast10x0",
            constraint_values = [
                "@pigweed//pw_build/constraints/arm:cortex-m4",
                "@platforms//cpu:armv7e-m",
            ],
            flags = {
                "@pigweed//pw_kernel/config:kernel_config": ":config",
                "@pigweed//pw_kernel/subsys/console:console_backend": ":console",
            },
        )

    Flag keys are command-line flag labels and resolve from the root module,
    so spell them with their repository (`@pigweed//...`). Flag values that
    are labels are resolved relative to the calling package.

    Args:
      name: Platform name; the constraint value is `target_<name>`.
      soc: Label of the SoC constraint value declared by `openprot_soc()`.
      constraint_values: CPU and architecture constraint values for the core.
      flags: Dict of flag label to value merged over `KERNEL_DEVICE_COMMON_FLAGS`.
      visibility: Visibility of both the constraint value and the platform.
      **kwargs: Forwarded to `platform()`.
    """
    native.constraint_value(
        name = "target_" + name,
        constraint_setting = _PW_KERNEL_TARGET_SETTING,
        visibility = visibility,
    )
    native.platform(
        name = name,
        constraint_values = [
            ":target_" + name,
            soc,
            str(Label("@platforms//os:none")),
            str(Label("@pigweed//pw_build/constraints/rust:no_std")),
        ] + constraint_values,
        flags = flags_from_dict(KERNEL_DEVICE_COMMON_FLAGS | flags),
        visibility = visibility,
        **kwargs
    )
