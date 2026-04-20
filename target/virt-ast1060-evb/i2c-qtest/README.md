# I2C Server qtest suite (virt-ast1060-evb)

End-to-end validation of `services/i2c/server` under the Pigweed kernel,
driven by qtest from outside QEMU. The firmware boots in QEMU, a scenario
dispatcher polls an `aspeed-qtest-ctrl` scratchpad device, and a host-side
qtest binary writes scenario IDs + reads back status/result bytes to
validate behaviour.

See [`plan-i2c-server-qtest.md`](../../../../plan-i2c-server-qtest.md) for
the original design rationale and the full scenario matrix.

---

## Current status

| Scenario | What it exercises                                 | Status     |
|----------|---------------------------------------------------|------------|
| `scenario_1`  | `configure_target_address` + address validation | **PASS**   |
| `scenario_5`  | master write-read against tmp105 path           | fail (1)   |
| `scenario_6`  | NACK on vacant address                          | fail (1)   |
| `scenario_7`  | register-read loop                              | fail (1)   |
| `scenario_8`  | pca9552 write + write-read                      | fail (1)   |
| `scenario_9`  | slave-mode RX via `i2c-test-master`             | fail (2)   |
| `scenario_10` | slave-mode TX via `i2c-test-master`             | fail (2)   |
| `scenario_11` | slave-address rebind                            | **PASS**   |

Failure buckets:

1. **AST I2C master-emulation gap.** The clockdomain/qemu `ast10x0-i2c`
   branch ships the slave-mode DMA-TX patches, but the master-mode flow
   that `services/i2c/backend-aspeed` drives doesn't complete against
   QEMU's `hw/i2c/aspeed_i2c.c` — the backend polls for status bits QEMU
   doesn't set, so scenarios 5/7/8 hang for ~30s and scenario 6 sees a
   spurious ACK for a vacant address.
2. **Single-bus `i2c_server` init.** `services/i2c/server/src/main.rs`
   only calls `backend.init_bus(2)`. The firmware-side slave in scenarios
   9/10 therefore lives on bus 2, but QEMU's `i2c-test-master` is
   hardwired to bus 3, so master→slave never connects and
   `slave_wait_event` returns Err.

Neither failure is a harness bug — scenarios 1 and 11 prove the full
QEMU + `-accel tcg` + scratchpad + IPC + backend loop works end-to-end.

---

## Prerequisites

### 1. Patched QEMU

Tests require QEMU from the `clockdomain/qemu` fork, branch `ast10x0-i2c`.
That branch carries:

- The I²C slave-mode DMA-TX patches for `hw/i2c/aspeed`.
- Populated I²C slaves on `-M ast1060-evb` (tmp105 on bus 1, pca9552 on
  bus 2, pca9554 on bus 3, etc.).
- `i2c-test-master` sysbus device on bus 3 at `0x7e7c_0000`.
- `aspeed-qtest-ctrl` scratchpad at `0x7e7d_0000`.
- `tests/qtest/aspeed_i2c_server-test` — the host qtest binary this
  harness invokes, including the scenario driver functions added
  alongside this harness (`/arm/ast1060/i2c_server/scenario_*`).

Clone and build once on your workstation:

```bash
git clone https://github.com/clockdomain/qemu.git
cd qemu
git checkout ast10x0-i2c

# System deps (Debian/Ubuntu).
sudo apt install -y git build-essential ninja-build meson pkg-config \
    python3-venv python3-tomli python3-setuptools python3-wheel \
    libglib2.0-dev libpixman-1-dev libfdt-dev zlib1g-dev flex bison

mkdir build-arm && cd build-arm
../configure --target-list=arm-softmmu --enable-debug
ninja qemu-system-arm tests/qtest/aspeed_i2c_server-test
```

The two artefacts the harness needs:

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

Or pass them per-invocation on the `bazel test` command line. Each
scenario fails fast with a descriptive error if either variable is unset
or points at a non-executable path.

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
# Single scenario
bazelisk test //target/virt-ast1060-evb/i2c-qtest:scenario_1

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

The qtest binary itself launches QEMU with `-accel tcg -M ast1060-evb
-cpu cortex-m4 -nographic -semihosting-config enable=on,target=native
-kernel $FIRMWARE_ELF`, drives the scratchpad over the qtest protocol,
and asserts on the returned status + result bytes.

**`-accel tcg` is load-bearing.** Without it, `qtest_init()` falls back
to `-accel qtest`, which freezes the vCPU between RPC calls so the
firmware never progresses. With TCG enabled the guest runs instructions
normally while qtest still mediates MMIO.

---

## Scenario matrix

| #   | IPC call on server                                  | Bus / target       | Expected result                  |
|-----|-----------------------------------------------------|--------------------|----------------------------------|
| 1   | 4× `configure_target_address` / `I2cAddress::new()` | bus 2              | `R_RESULT[0] == 0x0F`            |
| 5   | `write_read` (2-byte)                               | bus 2, 0x4d        | 2-byte readback                  |
| 6   | `write(empty)` probe to vacant address              | bus 2, 0x10        | Err (NACK) → PASS                |
| 7   | 5× `write_read` register-read loop                  | bus 2, 0x4d        | 5-byte readback                  |
| 8   | `write` + `write_read` to pca9552 LS0               | bus 2, 0x60        | `R_RESULT[0] == 0xAA`            |
| 9   | slave RX: `configure` + `enable_receive` + wait     | bus 2, 0x55        | received bytes == master pattern |
| 10  | slave TX: + `slave_set_response` + wait             | bus 2, 0x55        | `R_RESULT[0] == 8` (TX length)   |
| 11  | Two `configure_target_address` at different addrs   | bus 2, 0x55 → 0x56 | `R_RESULT == [0x55, 0x56]`       |

Scenarios 2–4 from the plan (`init_bus` at Std/Fast/FastPlus speeds) are
not implemented — the `services/i2c/api` API does not expose `init_bus`
today; bus speed is set statically in `system.json5`.

All scenarios target bus 2 rather than the buses named in the plan
(scenario 5/7 wanted tmp105 on bus 1; 9/10/11 wanted bus 3) because
`i2c_server` only initializes bus 2. Scenarios that exercise real hardware
currently fail for that reason — see "Current status" above.

### Control-register layout (`aspeed-qtest-ctrl` @ `0x7e7d_0000`)

| Offset | Name              | Notes                                                    |
|--------|-------------------|----------------------------------------------------------|
| `0x00` | `R_SCENARIO_ID`   | qtest writes to dispatch; firmware zeroes on completion  |
| `0x04` | `R_READY`         | firmware sets `1` after IPC client init                  |
| `0x08` | `R_STATUS`        | `0`=running, `1`=pass, `2`=fail, `3`=armed               |
| `0x0C` | `R_RESULT_LEN`    | number of valid bytes in `R_RESULT`                      |
| `0x10` | `R_RESULT[0..32]` | scenario-specific bytes                                  |

The firmware writes the result region **byte-by-byte** over 8-bit MMIO
stores. The `aspeed-qtest-ctrl` device stores `value & 0xff` on every
write, so word-wide stores truncate to the first byte — a footgun worth
remembering if you hack on the dispatcher.

`ARMED` is a two-phase signal used by slave scenarios (9, 10): the
firmware has configured the controller and is blocked on
`slave_wait_event`; qtest may now drive `i2c-test-master` to generate
the bus event.

---

## Manual debugging

To boot the image directly in QEMU without the qtest harness (useful
when bringing up new scenarios):

```bash
$QTEST_QEMU_BINARY -accel tcg -M ast1060-evb -cpu cortex-m4 -nographic \
    -semihosting-config enable=on,target=native \
    -kernel bazel-bin/target/virt-ast1060-evb/i2c-qtest/i2c_qtest.elf \
    -monitor stdio
```

The firmware logs tokenized messages via semihosting (visible on stdout;
decode with `pw_tokenizer` against the build's token database). Once
`R_READY == 1` at `0x7e7d_0004`, you can poke the scratchpad directly.
From the HMP prompt (separate `-monitor` channel if you've split it
out):

```
(qemu) xp/1wx 0x7e7d0004     # confirm R_READY == 1
(qemu) writel 0x7e7d0000 5   # write R_SCENARIO_ID = 5
(qemu) xp/1wx 0x7e7d0008     # poll R_STATUS
(qemu) xp/1wx 0x7e7d000c     # read R_RESULT_LEN
(qemu) xp/8bx 0x7e7d0010     # read R_RESULT bytes
```

Trace Aspeed I2C register activity as the firmware boots and runs a
scenario:

```bash
$QTEST_QEMU_BINARY -accel tcg -M ast1060-evb -cpu cortex-m4 -nographic \
    -semihosting-config enable=on,target=native \
    -kernel bazel-bin/target/virt-ast1060-evb/i2c-qtest/i2c_qtest.elf \
    -d trace:aspeed_i2c_\* 2> /tmp/aspeed-i2c.trace
```

---

## To unblock the failing scenarios

Neither blocker is in the harness; both are in components the harness
now surfaces:

1. **AST I2C master completion in QEMU (fixes 5/6/7/8).** The
   `backend-aspeed` master path polls status bits `hw/i2c/aspeed_i2c.c`
   never sets on the `ast10x0-i2c` branch. Either patch the QEMU
   emulation to complete master-mode transactions the way
   `backend-aspeed` expects, or change the backend's master sequence to
   a mode the emulation already supports.
2. **Multi-bus init in `i2c_server` (fixes 9/10 — also needs item 1).**
   Change `services/i2c/server/src/main.rs` so `init_bus` is called for
   every bus declared in the app's `system.json5` instead of hardcoding
   bus 2. Then add the `I2C3` pinmux group to
   `target/virt-ast1060-evb/entry.rs::i2c_init()` and move scenarios
   9/10/11 back to bus 3 (where `i2c-test-master` lives).

Both are follow-ups outside the harness itself.
