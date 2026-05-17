// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! ECDSA device-layer constants (poll budget, advisory windows, SRAM base).

/// Default completion-poll budget (poll iterations, not wall-clock).
/// Tuning obligation: too low → spurious timeouts; too high → slow wedge
/// detection. Adequacy vs. real verify latency is discharged only by the
/// Phase-6 KAT/parity run (goal.md §2.1 residual), not assumed here.
pub const DEFAULT_POLL_BUDGET: u32 = 1_000_000;

/// Advisory wait window (ns) passed to the cooperative `yield_fn` between
/// completion polls. Tracks the **normative authority's 10 µs** poll interval
/// (`zephyr-reference/ecdsa_aspeed.c:115`, `k_usleep(10)`). Advisory only: the
/// injected strategy may honor or ignore it. (The earlier 5 µs was the
/// rejected `aspeed-rust` value — goal.md D3/§0.3.)
pub const POLL_YIELD_NS: u32 = 10_000;

/// Settle window (ns) after the ECC-engine reset, before parameter load.
/// Authority: `k_usleep(1000)` = 1 ms (`ecdsa_aspeed.c:59`, delta D2).
pub const RESET_SETTLE_NS: u32 = 1_000_000;

/// Trigger-assert hold (ns) before de-asserting the trigger register.
/// Authority: `k_usleep(5000)` = 5 ms (`ecdsa_aspeed.c:111`, delta D2).
pub const TRIGGER_HOLD_NS: u32 = 5_000_000;

/// ECDSA engine scratch-RAM base address.
///
/// P5-OPEN-A (goal.md §3) — decided value, single fix point. The normative
/// Zephyr driver reads this from device-tree at runtime (no constant to
/// copy); this is the *informative* `aspeed-rust` `SBC_SRAM_BASE`. **Not
/// proven** — validated by the Phase-6 KAT run (a wrong base → garbage
/// operands → loud deterministic verdict mismatch). If a SoC/PAC source ever
/// states otherwise, that source wins and only this line changes.
pub const SBC_SRAM_BASE: usize = 0x7900_0000;
