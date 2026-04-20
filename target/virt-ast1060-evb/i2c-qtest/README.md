# I2C Server qtest suite (virt-ast1060-evb)

End-to-end validation of `services/i2c/server` under the Pigweed kernel, driven
by qtest from outside QEMU. The firmware boots in QEMU, a scenario dispatcher
polls an `aspeed-qtest-ctrl` scratchpad device, and a host-side qtest binary
writes scenario IDs + reads back status/result bytes to validate behaviour.

See [`plan-i2c-server-qtest.md`](../../../../plan-i2c-server-qtest.md) for the
design rationale and the full scenario matrix.

---

## Prerequisites

### 1. Patched QEMU

Tests require QEMU from the `clockdomain/qemu` fork, branch `ast10x0-i2c`.
That branch carries:

- The I²C slave-mode DMA-TX patches for `hw/i2c/aspeed`.
- Populated I²C slaves on `-M ast1060-evb` (tmp105 on bus 1, pca9552 on
  bus 2).
- `i2c-test-master` sysbus device on bus 3 at `0x7e7c_0000`.
- `aspeed-qtest-ctrl` scratchpad at `0x7e7d_0000` (the firmware-side IPC
  rendezvous for this suite).
- `tests/qtest/aspeed_i2c_server-test` — the host qtest binary this harness
  invokes.

Clone and build once on your workstation:

```bash
git clone https://github.com/clockdomain/qemu.git
cd qemu
git checkout ast10x0-i2c

# System deps (Debian/Ubuntu). See plan.md §1 for the full list.
sudo apt install -y git build-essential ninja-build meson pkg-config \
    python3-venv python3-tomli python3-setuptools python3-wheel \
    libglib2.0-dev libpixman-1-dev libfdt-dev zlib1g-dev flex bison

mkdir build-arm && cd build-arm
../configure --target-list=arm-softmmu --enable-debug
ninja
```

After this, the two artefacts the harness needs are:

```
<qemu>/build-arm/qemu-system-arm
<qemu>/build-arm/tests/qtest/aspeed_i2c_server-test
```

### 2. Env vars for Bazel

Bazel does **not** fetch or build QEMU. Point the sh_test wrappers at your
checkout by adding to `~/.bazelrc` (or the project `.bazelrc`):

```
test --test_env=QTEST_QEMU_BINARY=/absolute/path/to/qemu/build-arm/qemu-system-arm
test --test_env=I2C_SERVER_QTEST_BIN=/absolute/path/to/qemu/build-arm/tests/qtest/aspeed_i2c_server-test
```

Or pass them per-invocation on the `bazel test` command line. Each scenario
fails fast with a descriptive error if either variable is unset or points at a
non-executable path.

---

## Building

From the `openprot/` workspace root:

```bash
# Full system image (firmware ELF + .bin). QEMU boots the ELF via -kernel.
bazelisk build //target/virt-ast1060-evb/i2c-qtest:i2c_qtest

# Outputs:
#   bazel-bin/target/virt-ast1060-evb/i2c-qtest/i2c_qtest.elf
#   bazel-bin/target/virt-ast1060-evb/i2c-qtest/i2c_qtest.bin
```

Building the test wrappers does not require QEMU to be built yet — only
running them does:

```bash
bazelisk build //target/virt-ast1060-evb/i2c-qtest:scenario_1
bazelisk build //target/virt-ast1060-evb/i2c-qtest:all      # all 8 scenarios
```

---

## Running the tests

```bash
# One scenario
bazelisk test //target/virt-ast1060-evb/i2c-qtest:scenario_5

# All scenarios
bazelisk test //target/virt-ast1060-evb/i2c-qtest:scenario_1 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_5 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_6 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_7 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_8 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_9 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_10 \
               //target/virt-ast1060-evb/i2c-qtest:scenario_11
```

Each wrapper:

1. Resolves the firmware ELF as a Bazel runfile, exports it as
   `FIRMWARE_ELF`.
2. Exports `QTEST_QEMU_BINARY` so the qtest binary can spawn QEMU.
3. `exec`s `$I2C_SERVER_QTEST_BIN -p /arm/ast1060/i2c_server/scenario_<NN>`.

The qtest binary itself launches QEMU with `-M ast1060-evb -kernel
$FIRMWARE_ELF`, drives the scratchpad over the qtest protocol, and asserts
on the returned status + result bytes.

---

## Scenario matrix

| Test            | What it exercises                                         | Bus / device     | Expected `R_STATUS` |
|-----------------|-----------------------------------------------------------|------------------|---------------------|
| `scenario_1`    | `configure_target_address` + `I2cAddress::new()` validation | bus 3            | `PASS` + `R_RESULT[0] = 0x0F` |
| `scenario_5`    | master write→read (tmp105 temperature register)           | bus 1, tmp105    | `PASS` + 2-byte reading |
| `scenario_6`    | NACK path on vacant address                               | bus 1, 0x10      | `PASS` (any Err counts as NACK) |
| `scenario_7`    | register-read loop (5 reads)                              | bus 1, tmp105    | `PASS` + 5 bytes |
| `scenario_8`    | write+write-read round-trip (pca9552 LS0)                 | bus 2, pca9552   | `PASS` + LS0 readback |
| `scenario_9`    | slave-mode RX (master writes N bytes to firmware)         | bus 3, 0x55      | `ARMED` → `PASS` + received bytes |
| `scenario_10`   | slave-mode TX (firmware pre-loads, master reads)          | bus 3, 0x55      | `ARMED` → `PASS` + `[len]` |
| `scenario_11`   | slave address rebind                                      | bus 3, 0x55→0x56 | `PASS` + `[0x55, 0x56]` |

Scenarios 2–4 (`init_bus` at Std/Fast/FastPlus speeds) are not implemented —
the `services/i2c/api` API does not expose `init_bus` today, bus speed is
set statically in `system.json5`. See the plan for the deferred work.

### Control-register layout (`aspeed-qtest-ctrl` @ `0x7e7d_0000`)

The firmware writes and qtest reads these:

| Offset | Name            | Notes                                                   |
|--------|-----------------|---------------------------------------------------------|
| `0x00` | `R_SCENARIO_ID` | qtest writes to dispatch; firmware zeroes on completion |
| `0x04` | `R_READY`       | Firmware sets `1` after IPC client init                 |
| `0x08` | `R_STATUS`      | `0`=running, `1`=pass, `2`=fail, `3`=armed              |
| `0x0C` | `R_RESULT_LEN`  | Number of valid bytes in `R_RESULT`                     |
| `0x10` | `R_RESULT[0..32]` | Scenario-specific bytes                               |

`ARMED` is a two-phase signal used by slave scenarios (9, 10): the firmware
has configured the controller and is blocked on `slave_wait_event`; qtest may
now drive `i2c-test-master` to generate the bus event.

---

## Manual debugging

To boot the image directly in QEMU without the qtest harness (useful when
bringing up new scenarios):

```bash
$QTEST_QEMU_BINARY -M ast1060-evb -cpu cortex-m4 -bios none -nographic \
    -semihosting -kernel bazel-bin/target/virt-ast1060-evb/i2c-qtest/i2c_qtest.elf \
    -monitor stdio
```

The firmware logs via semihosting (visible on stdout). Once you see
`i2c_qtest_client ready`, `R_READY == 1` at `0x7e7d_0004`. From the HMP prompt
you can poke the scratchpad directly, e.g. dispatch scenario 5:

```
(qemu) xp/1wx 0x7e7d0004     # confirm R_READY == 1
(qemu) writel 0x7e7d0000 5   # write R_SCENARIO_ID = 5
(qemu) xp/1wx 0x7e7d0008     # poll R_STATUS
(qemu) xp/1wx 0x7e7d000c     # read R_RESULT_LEN
(qemu) xp/8bx 0x7e7d0010     # read R_RESULT bytes
```

---

## Known gaps

- **I2C IRQ wiring.** [`system.json5`](system.json5) declares only
  `I2C2_IRQ` (number 112), matching the existing [`target/ast1060-evb/i2c`](../../ast1060-evb/i2c/system.json5)
  configuration. Master-mode scenarios (5–8) run fine via polling, but slave
  scenarios 9–11 on bus 3 may need an `I2C3_IRQ` declaration depending on
  how `services/i2c/server` arms its wait group. Expect iteration when those
  scenarios first run live.
- **Scenario 1 address set.** The plan wanted `{0x42, 0x7F, 0x80, 0x00}`;
  the implementation uses `{0x42, 0x77, 0x80, 0x78}` because
  [`I2cAddress::new()`](../../../services/i2c/api/src/address.rs) rejects
  both `0x7F` and `0x00` as reserved. The scenario still exercises the same
  two edges (valid / out-of-range / reserved).
- **QEMU-side pieces unverified from this workspace.** The harness assumes
  `aspeed-qtest-ctrl`, the bus-3 `i2c-test-master`, and
  `aspeed_i2c_server-test` are present on the `ast10x0-i2c` branch per the
  plan. If your fork lacks any of them, the first `bazel test` will fail
  with a clear error; rebuild from the right SHA.
