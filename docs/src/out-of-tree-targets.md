# Out-of-tree targets

OpenPRoT hosts reference targets only. The directories under `target/` are
SoC support plus one reference board each, kept in-tree so the project can
test the kernel, services and drivers on real silicon and in emulators. A new
board or SoC belongs in its own repository that consumes openprot as a Bazel
module. This page describes that contract. The worked example is
[`examples/out_of_tree_target/`](https://github.com/OpenPRoT/openprot/tree/main/examples/out_of_tree_target),
which CI builds standalone on every change.

## The two constraints

A pw_kernel platform in openprot carries two constraint values:

- **Target** (`@pigweed//pw_kernel/target:target`): the board or product.
  Every platform has its own, and the pw_kernel tooling keys on it.
- **SoC** (`@openprot//target:soc`): the chip. Each reference SoC declares
  one value, such as `@openprot//target/ast10x0:soc_ast10x0`, and gates its
  support crates on that value. Entry code, the console backend, the kernel
  config, peripherals, I2C and USART backends and the linker script template
  all build for any platform that lists the SoC value, in whichever
  repository that platform lives.

Shared drivers and services are gated on neither. They use
`PW_KERNEL_TARGET_COMPATIBLE_WITH` from `@openprot//target:defs.bzl`, which
admits every platform that declares a pw_kernel target and rejects the host.

So there are two shapes of out-of-tree target:

1. **A new board on a reference SoC.** Declare a platform that lists the
   in-tree SoC value, point its kernel config and console flags at the in-tree
   crates or your own, and write the system config and target binary. This is
   what the example does.
2. **A new SoC.** Also call `openprot_soc()` in your own package, implement
   the SoC crates (entry, console backend, `kernel_config`, linker script
   template, peripherals) and gate them with `soc_compatible_with()`. Services
   reach hardware through the traits in `services/*/api` and `hal/`, so a
   backend crate implementing those traits is all a service needs.

## Declaring the platform

```python
load("@openprot//target:defs.bzl", "openprot_target")

openprot_target(
    name = "my_board",
    soc = "@openprot//target/ast10x0:soc_ast10x0",
    constraint_values = [
        "@pigweed//pw_build/constraints/arm:cortex-m4",
        "@platforms//cpu:armv7e-m",
    ],
    flags = {
        "@pigweed//pw_kernel/config:kernel_config": ":config",
        "@pigweed//pw_kernel/subsys/console:console_backend": "@openprot//target/ast10x0:console",
    },
)
```

`openprot_target()` creates the constraint value `target_my_board` and the
`platform()` `my_board`, adds `@platforms//os:none` and Pigweed's `no_std`
constraint, and merges `flags` over Pigweed's `KERNEL_DEVICE_COMMON_FLAGS`.
The flag keys resolve from the root module, so write them with their
repository name.

For a new SoC:

```python
load("@openprot//target:defs.bzl", "openprot_soc", "soc_compatible_with")

openprot_soc(name = "my_soc")  # declares :soc_my_soc

MY_SOC_COMPATIBLE_WITH = soc_compatible_with(str(Label("//soc:soc_my_soc")))
```

Put the `soc_compatible_with()` call in a `.bzl` file and anchor the label
with `Label()`, as the in-tree `target/<soc>/defs.bzl` files do, so it keeps
resolving if another module loads your definitions.

## Building the image

The image is assembled by Pigweed's `system_image()` exactly as in-tree. The
three pieces are the `system.json5` memory and app layout, a
`target_linker_script()` rendered from the SoC's template, and a
`rust_binary` that links the SoC entry crate and implements
`TargetInterface`. See `examples/out_of_tree_target/BUILD.bazel`; the only
difference from `target/ast10x0/erot/BUILD.bazel` is the `@openprot//`
prefix on the labels.

Gate every target-only rule with the SoC's `TARGET_COMPATIBLE_WITH` (from
`@openprot//target/<soc>:defs.bzl`) or your own `soc_compatible_with()` so
host wildcard builds skip them.

## MODULE.bazel

Bazel only honours `git_override`, `archive_override`,
`local_path_override` and `override_repo` in the root module. openprot
needs several of them, so your module must repeat the block from
`examples/out_of_tree_target/MODULE.bazel`:

- `pigweed` and `ureg` via `git_override`, pinned to the commits openprot
  uses.
- `caliptra_deps`, the nested module at `third_party/caliptra`. With a
  `git_override` of openprot, override it from the same remote and commit
  with `strip_prefix = "third_party/caliptra"`.
- `opentitan_devbundle` via `archive_override`.
- The `override_repo` that replaces Pigweed's `rust_crates` with openprot's
  crate universe. Pigweed's own repo does not resolve the embedded target
  triples, and openprot's does.

Copy `.bazelrc` as well, or at least the `common` lines from the example's.
The example keeps its pins identical to openprot's;
`presubmit/check_downstream_module.py` fails presubmit when they drift and
`--fix` copies them over. When you consume a tagged openprot commit, pin the
same pigweed commit that commit's `MODULE.bazel` names.

## Running tests

The AST10x0 QEMU harness is public and usable from a downstream module:

```
test:qemu --platforms=//:my_board
test:qemu --@openprot//target/ast10x0:qemu=true
test:qemu --run_under="@openprot//target/ast10x0/harness:qemu_runner \
  --cpu cortex-m4 --machine ast1030-evb --image "
```

A `system_image_test()` whose target prints `TEST_RESULT:PASS` or
`TEST_RESULT:FAIL` on the console then passes or fails under
`bazel test --config=qemu`. The physical board runner at
`@openprot//target/ast10x0/harness:test_runner` works the same way.

## Checklist

- [ ] `openprot_target()` with the SoC value, kernel config and console flags.
- [ ] For a new SoC: `openprot_soc()`, the SoC crates, and a
      `soc_compatible_with()` select in a `.bzl` file.
- [ ] `system.json5`, linker script, and a target binary implementing
      `TargetInterface`.
- [ ] `MODULE.bazel` with the override block, and `.bazelrc` flags.
- [ ] Target-only rules gated with the SoC's compatibility select.
