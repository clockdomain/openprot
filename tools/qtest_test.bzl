# Licensed under the Apache-2.0 license

"""Bazel rule for running a QEMU qtest scenario against an openprot image.

The `qtest_scenario` rule produces an sh_test that invokes a prebuilt
qtest binary (`aspeed_i2c_server-test`) against a prebuilt QEMU
(`qemu-system-arm`). Both are expected to come from the
clockdomain/qemu@ast10x0-i2c branch (which carries the
`aspeed-qtest-ctrl` sysbus device, the `i2c-test-master`, and the qtest
binary itself).

Bazel does not fetch or build QEMU. Contributors clone the fork, run
`./configure --target-list=arm-softmmu --enable-debug && ninja`, and
point Bazel at the result via env vars — typically in `.bazelrc`:

    test --test_env=QTEST_QEMU_BINARY=/path/to/qemu/build-arm/qemu-system-arm
    test --test_env=I2C_SERVER_QTEST_BIN=/path/to/qemu/build-arm/tests/qtest/aspeed_i2c_server-test

At runtime the rule:

  1. Locates the firmware ELF (the system_image target) as a runfile.
  2. Reads `QTEST_QEMU_BINARY` and `I2C_SERVER_QTEST_BIN` from the test
     environment; fails fast with a descriptive error if either is unset
     or missing.
  3. Exports `QTEST_QEMU_BINARY` (consumed by the qtest binary itself to
     spawn QEMU) and `FIRMWARE_ELF` (consumed by the qtest binary to
     pick the -kernel argument).
  4. Invokes `$I2C_SERVER_QTEST_BIN -p /arm/ast1060/i2c_server/scenario_<id>`.
"""

load(
    "@pigweed//pw_kernel/tooling:system_image.bzl",
    "SystemImageInfo",
)

def _firmware_platform_transition_impl(settings, attr):
    # Resolve the firmware image under its kernel target platform so a
    # host-side sh_test can still depend on it. The caller supplies the
    # platform label via the `firmware_platform` attribute.
    _ = settings  # unused
    return {"//command_line_option:platforms": str(attr.firmware_platform)}

_firmware_platform_transition = transition(
    implementation = _firmware_platform_transition_impl,
    inputs = [],
    outputs = ["//command_line_option:platforms"],
)

def _pad2(n):
    s = str(n)
    if len(s) < 2:
        return "0" + s
    return s

def _qtest_scenario_impl(ctx):
    # `image` uses `cfg = firmware_platform_transition`, so attr.image is a
    # single-element list. QEMU's `-kernel` on Cortex-M expects an ELF,
    # not the raw .bin artifact (which is only useful for physical flash
    # upload via uart_boot_image).
    firmware = ctx.attr.image[0][SystemImageInfo].elf

    script = ctx.actions.declare_file(ctx.label.name + ".sh")

    content = """#!/bin/bash
set -euo pipefail

if [[ -z "${{QTEST_QEMU_BINARY:-}}" ]]; then
    echo "ERROR: QTEST_QEMU_BINARY is not set." >&2
    echo "Build clockdomain/qemu@ast10x0-i2c and add to your .bazelrc:" >&2
    echo "  test --test_env=QTEST_QEMU_BINARY=/path/to/qemu-system-arm" >&2
    exit 1
fi
if [[ ! -x "${{QTEST_QEMU_BINARY}}" ]]; then
    echo "ERROR: QTEST_QEMU_BINARY='${{QTEST_QEMU_BINARY}}' is not executable." >&2
    exit 1
fi

if [[ -z "${{I2C_SERVER_QTEST_BIN:-}}" ]]; then
    echo "ERROR: I2C_SERVER_QTEST_BIN is not set." >&2
    echo "Build clockdomain/qemu@ast10x0-i2c tests/qtest/aspeed_i2c_server-test" >&2
    echo "and add to your .bazelrc:" >&2
    echo "  test --test_env=I2C_SERVER_QTEST_BIN=/path/to/aspeed_i2c_server-test" >&2
    exit 1
fi
if [[ ! -x "${{I2C_SERVER_QTEST_BIN}}" ]]; then
    echo "ERROR: I2C_SERVER_QTEST_BIN='${{I2C_SERVER_QTEST_BIN}}' is not executable." >&2
    exit 1
fi

# Resolve firmware relative to the runfiles tree.
SCRIPT_DIR="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" && pwd)"
RUNFILES="${{SCRIPT_DIR}}/{script_name}.runfiles/_main"
if [[ -f "${{RUNFILES}}/{firmware_path}" ]]; then
    FIRMWARE="${{RUNFILES}}/{firmware_path}"
elif [[ -f "{firmware_path}" ]]; then
    FIRMWARE="{firmware_path}"
else
    echo "ERROR: Could not locate firmware at {firmware_path}." >&2
    exit 1
fi

export FIRMWARE_ELF="${{FIRMWARE}}"
export QTEST_QEMU_BINARY

exec "${{I2C_SERVER_QTEST_BIN}}" -p "/arm/ast1060/i2c_server/scenario_{scenario_id}"
""".format(
        script_name = ctx.label.name + ".sh",
        firmware_path = firmware.short_path,
        scenario_id = _pad2(ctx.attr.scenario_id),
    )

    ctx.actions.write(
        output = script,
        content = content,
        is_executable = True,
    )

    runfiles = ctx.runfiles(files = [firmware])

    return [
        DefaultInfo(
            executable = script,
            runfiles = runfiles,
        ),
    ]

qtest_scenario_test = rule(
    implementation = _qtest_scenario_impl,
    test = True,
    attrs = {
        "image": attr.label(
            mandatory = True,
            cfg = _firmware_platform_transition,
            providers = [SystemImageInfo],
            doc = "system_image target carrying the firmware ELF.",
        ),
        "firmware_platform": attr.label(
            mandatory = True,
            doc = "Bazel platform used to build the firmware image.",
        ),
        "scenario_id": attr.int(
            mandatory = True,
            doc = "Numeric scenario id written into R_SCENARIO_ID.",
        ),
    },
    doc = """Run a single qtest scenario against a prebuilt QEMU + qtest binary.

The system_image label is materialised as a runfile; the qtest binary
(`aspeed_i2c_server-test`) and QEMU (`qemu-system-arm`) are injected
via the test environment (`I2C_SERVER_QTEST_BIN`, `QTEST_QEMU_BINARY`).
""",
)

def qtest_scenario(name, image, scenario_id, firmware_platform, **kwargs):
    """Macro wrapper around qtest_scenario_test.

    Args:
      name: test target name.
      image: system_image (or plain file) target holding the firmware ELF.
      scenario_id: integer scenario id written to R_SCENARIO_ID.
      firmware_platform: label of the platform that `image` was built for
        (e.g. `//target/virt-ast1060-evb`). The rule transitions the image
        dep under this platform so the host-side sh_test remains compatible
        with the host toolchain.
      **kwargs: forwarded to qtest_scenario_test (e.g. `tags`).
    """
    qtest_scenario_test(
        name = name,
        image = image,
        firmware_platform = firmware_platform,
        scenario_id = scenario_id,
        **kwargs
    )
