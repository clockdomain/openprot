# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0
"""Common definitions used by all earlgrey targets.
"""

load("//target:defs.bzl", "soc_compatible_with")

# Compatible with every platform built on the Earl Grey SoC, in-tree or not.
TARGET_COMPATIBLE_WITH = soc_compatible_with(str(Label("//target/earlgrey:soc_earlgrey")))
