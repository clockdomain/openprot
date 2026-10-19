#!/usr/bin/env python3
# Licensed under the Apache-2.0 license
# SPDX-License-Identifier: Apache-2.0
"""Keeps examples/out_of_tree_target/MODULE.bazel in sync with MODULE.bazel.

Bazel ignores `git_override`, `archive_override` and `override_repo` outside
the root module, so a downstream consumer of openprot has to repeat them. The
example module is that consumer, and this check fails when a pinned commit,
archive integrity or URL in its copy drifts from the root module's. `--fix`
rewrites the example's pins from the root.

Usage:
    presubmit/check_downstream_module.py [--fix] [ROOT_MODULE EXAMPLE_MODULE]
"""

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_ROOT_MODULE = ROOT / "MODULE.bazel"
DEFAULT_EXAMPLE_MODULE = ROOT / "examples" / "out_of_tree_target" / "MODULE.bazel"

# Each override block is `<kind>_override(\n    module_name = "<name>",\n ... )`.
_OVERRIDE_RE = re.compile(
    r'^(?P<kind>git|archive)_override\(\s*\n\s*module_name = "(?P<name>\w+)",\n(?P<body>(?:.*\n)*?)\)',
    re.MULTILINE,
)
# Attributes whose values must match between the two files.
_PINNED_ATTRS = ("commit", "integrity", "url", "remote")


def pinned_overrides(text):
    """Returns {module_name: {attr: value}} for every git/archive override."""
    pins = {}
    for m in _OVERRIDE_RE.finditer(text):
        attrs = {}
        for attr in _PINNED_ATTRS:
            am = re.search(r'^\s*%s = "([^"]*)",' % attr, m.group("body"), re.MULTILINE)
            if am:
                attrs[attr] = am.group(1)
        pins[m.group("name")] = attrs
    return pins


def apply_pins(text, root_pins):
    """Rewrites the pinned attributes in `text` from `root_pins`."""

    def rewrite(m):
        body = m.group("body")
        for attr, value in root_pins.get(m.group("name"), {}).items():
            body = re.sub(
                r'^(\s*%s = ")[^"]*(",)' % attr,
                lambda am: am.group(1) + value + am.group(2),
                body,
                flags=re.MULTILINE,
            )
        return m.group(0)[: m.start("body") - m.start(0)] + body + ")"

    return _OVERRIDE_RE.sub(rewrite, text)


def compare(root_module, example_module):
    """Returns the list of drift errors between the two MODULE.bazel files."""
    root_pins = pinned_overrides(root_module.read_text())
    example_pins = pinned_overrides(example_module.read_text())

    errors = []
    for name, attrs in root_pins.items():
        if name not in example_pins:
            errors.append(
                f"{name}: overridden in {root_module} but not in {example_module}"
            )
            continue
        for attr, value in attrs.items():
            got = example_pins[name].get(attr)
            if got != value:
                errors.append(f"{name}.{attr}: root has {value!r}, example has {got!r}")
    for name in example_pins:
        if name not in root_pins:
            errors.append(
                f"{name}: overridden in {example_module} but not in {root_module}"
            )
    return errors


def downstream_module_check(ctx):
    """pw_presubmit check: runs when either MODULE.bazel is in the change."""
    from pw_presubmit.presubmit import PresubmitFailure

    if not any(path.name == "MODULE.bazel" for path in ctx.paths):
        return
    errors = compare(
        ctx.root / "MODULE.bazel", ctx.root / DEFAULT_EXAMPLE_MODULE.relative_to(ROOT)
    )
    if errors:
        raise PresubmitFailure(
            "\n".join(errors)
            + "\nrun `presubmit/check_downstream_module.py --fix` to copy the root module's pins"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--fix",
        action="store_true",
        help="rewrite the example's pins from the root module",
    )
    parser.add_argument(
        "root_module", nargs="?", default=DEFAULT_ROOT_MODULE, type=pathlib.Path
    )
    parser.add_argument(
        "example_module", nargs="?", default=DEFAULT_EXAMPLE_MODULE, type=pathlib.Path
    )
    args = parser.parse_args()

    errors = compare(args.root_module, args.example_module)

    if errors and args.fix:
        root_pins = pinned_overrides(args.root_module.read_text())
        args.example_module.write_text(
            apply_pins(args.example_module.read_text(), root_pins)
        )
        print(f"rewrote {args.example_module}")
        return 0
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    if errors:
        print(
            f"run `{sys.argv[0]} --fix` to copy the root module's pins", file=sys.stderr
        )
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
