<!-- Licensed under the Apache-2.0 license -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# SPI monitor (SPIPF)

Driver for the four AST1060 SPI monitors, the hardware filters that sit between
a host (BMC or PCH) and its flash and decide which transactions get through.
The generic contract this driver will implement is
[`hal/blocking/spi_filter/`](../../../../hal/blocking/spi_filter/README.md); the
chip-independent design is in `docs/src/design/spi-filter.md`, and the
orchestrator's use of a filter is in
`docs/src/design/orchestrator/orchestrator-platform.md`.

## Module structure

```
spimonitor/
├── mod.rs          — Re-exports; module declarations
├── registers.rs    — SpiMonitorRegisters: safe accessors over ast1060_pac::spipf,
│                     one block per SpiMonitorController (SPIPF1..4)
├── commands.rs     — CommandDescriptor: the per-slot command encoding the
│                     hardware needs to parse a command; the Zephyr descriptor table
├── policy.rs       — SpiMonitorPolicy: a declarative allow-list + region set
├── profile.rs      — Built-in allow-list profiles (read-only, update window, Zephyr default)
├── controller.rs   — SpiMonitor<Uninitialized | Configured | Locked>: the typestate facade
├── traits.rs       — SpiMonitorControl: the instance-id boot interface (to be retired)
└── types.rs        — Public enums, ViolationLogEntry, errors, boot-level types
```

## Seams

- **`SpiMonitor<Mode>`** is the handle. `Uninitialized::apply_policy` programs
  a whole `SpiMonitorPolicy` and yields `Configured`; `Configured` exposes the
  runtime-mutating API (commands, regions, mode, mux, log, interrupts) and
  `lock()` yields `Locked`, which keeps only the external mux and the
  violation-log drain. That split is deliberate: after lock, the boot
  sequence still has to hand the bus over and read violations, and nothing
  else.
- **`SpiMonitorPolicy`** is the declarative input: up to 32 allowed opcodes
  and up to 16 `RegionPolicy` entries. The 16 is this struct's capacity, not
  a hardware limit (see below). `profile.rs` supplies allow-lists; regions
  are platform policy and come from the caller.
- **`ViolationLogEntry::parse`** decodes one log RAM word into a blocked
  command, a blocked read address, or a blocked write address.

## Hardware facts the API encodes

- **Command slots carry descriptors, not opcodes.** Each of the 32 slots
  holds data width, dummy cycles, address length and width, and program
  size, because the monitor must parse a command to find the address it
  checks. `add_command` therefore fails with `UnsupportedCommand` for an
  opcode `commands::descriptor` does not know. Slots 0 and 1 are fixed for
  EN4B/EX4B and slot 31 for WREAR when present, so the general capacity is 29
  or 30. A slot can be valid, valid-once, or empty, and can be locked.
- **Regions are bitmaps.** The read and write privilege tables are each 512
  words covering 16 KiB blocks up to 256 MiB; a set bit allows. There is no
  region table and no table-full condition, only coarse granularity.
  `configure_region` rounds a span outward to whole blocks, which widens an
  *allow* as well as a *deny*; the HAL contract rejects misaligned spans
  instead.
- **Violations are sticky bits plus a log.** SPIPF004 holds command, write
  and read status bits with matching interrupt enables; the log is a
  caller-owned RAM buffer (`configure_log`) of one word per entry at 16 KiB
  address granularity. The hardware has a write index but no read pointer,
  so a consumer keeps its own cursor.
- **Locks are fine-grained.** Control, interrupt control, log base, log
  control, each privilege table, each command slot, plus software-reset and
  block-FIFO lock bits in the control register. `lock()` sets all of them and
  reads every one back.
- **The external mux is in the SCU, not here.** `set_ext_mux` writes SCU0F0
  through the SCU unlock discipline. `Sel0`/`Sel1` are electrical; the board
  (`ast10x0_board`) maps them onto root-of-trust and host control.
- **Two mode knobs.** Passthrough (single or multi-bit) and monitor enable
  are independent bits; Zephyr uses passthrough while the mux changes hands.

## How ASPEED's PFR firmware drives this block

The one shipping user of this hardware is ASPEED's PFR application
(`AspeedTech-BMC/aspeed-zephyr-project`, branch `aspeed-master`;
`apps/aspeed-pfr/src/AspeedStateMachine/AspeedStateMachine.c`,
`intel_pfr/intel_pfr_spi_filtering.c`, `spi_filter/spim_util.c`,
`platform_monitor/spim_monitor.c`) over the Zephyr driver
(`AspeedTech-BMC/zephyr`, `drivers/spi/spi_monitor_aspeed.c`), which this
crate mirrors. What it does with the registers is the reference for anyone
changing this driver:

| Moment | Filter and bus actions |
|---|---|
| Zephyr device init | Allow-list loaded from devicetree; read and write privilege granted over all 256 MiB, then devicetree forbidden regions removed; monitor enabled; external mux parked on the RoT. |
| Boot hold (`BMCBootHold`, `PCHBootHold`) | Host reset asserted, mux to the RoT's own controller, flash chip reset by command. No rule table written. |
| Host release (`enter_tzero`, provisioned) | **The only place policy is written**: the PFM manifest's SPI region records set or clear read and write privilege bits, then the monitor is enabled. Release resets the flash chip, pulses the monitor's software reset bit, switches the mux to the host, deasserts the reset GPIO. |
| Unprovisioned | `bypass_spi_filter`: everything allowed, monitor disabled (passthrough). |
| Host reset GPIO edge | Full re-verify through the hold path; **rules are rewritten on every host reset.** |
| `SYSTEM_LOCKDOWN` | Both hosts held, mux on the RoT. Nothing written, nothing locked. |

Two facts matter for anyone building on this:

- **Violations are log only.** The ISR calls an application callback that
  queues a work item to walk the log RAM and print each entry. No state
  change follows a blocked command.
- **The lock bits are never set.** The driver exposes `spim_lock_common`,
  `spim_lock_rw_privilege_table` and `spim_lock_allow_command_table`, the
  same registers `SpiMonitor::lock()` writes, and the application never calls
  them; the only callers are shell commands. That is consistent with
  re-arming on every host reset, since the lock bits hold until SoC reset.

The orchestrator design departs from this on all three points (locks the
command table, re-arms on verification rather than on host reset, treats a
blocked write as evidence); the reasons are on `FilterLockPolicy` and
`ViolationPolicy` in `services/orchestrator/config` and on
`Effect::ArmFlashGuard` in `services/orchestrator/sm`.

## To settle on the EVB

- **The software reset pulse.** ASPEED pulses the control register's
  software reset bit on every release, and nothing records what it clears. If
  it only rewinds the log pointer and the block FIFO, the orchestrator skips
  it and locks the reset control bit too. If it clears privilege state, the
  pulse has to stay and the bit stays unlocked. Lock, cycle a device through
  hold and release without the pulse, and confirm the filter still passes and
  blocks correctly.
- **Passthrough and enable together.** Whether any supported configuration
  sets both, or whether a three-valued mode (disabled, passthrough,
  filtering) is complete.
- **The block style.** ASPEED's driver is said to choose between an early
  chip-select deassert and blocked clocks on a violation; this driver has no
  such control and the register has not been identified.

## Tests

QEMU and EVB tests live under `target/ast10x0/tests/spimonitor/`
(`setup_all_spim.rs` walks hold, configure, release and monitor on all four
instances). `commands.rs` carries host unit tests for the descriptor
encoding.
