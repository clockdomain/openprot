# Example out-of-tree target

A board on the AST10x0 SoC, defined in its own Bazel module with openprot as a
dependency. It is the template for any target that is not one of openprot's
reference targets, and CI builds it standalone so the downstream paradigm
stays honest.

```bash
cd examples/out_of_tree_target
bazelisk build --config=qemu //...
bazelisk test  --config=qemu //:example_board_test
```

What it shows:

- `MODULE.bazel`: the dependency and override block a downstream module copies.
- `BUILD.bazel`: a platform declared with `openprot_target()` that reuses the
  in-tree SoC crates by carrying `@openprot//target/ast10x0:soc_ast10x0`.
- `.bazelrc`: openprot's build-wide flags plus a `qemu` config that runs the
  image with openprot's harness.

`MODULE.bazel.lock` is generated on first build and ignored by git here, so
it tracks whatever pigweed commit openprot pins. A real downstream module
should commit its own lockfile.

The full write-up is in the OpenPRoT docs under "Out-of-tree targets"
(`docs/src/out-of-tree-targets.md`).
