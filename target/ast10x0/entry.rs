// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Entry point for the ASPEED AST10x0 target.
//!
//! # What lives here vs. elsewhere
//!
//! This file is the **platform** layer. It runs once on cortex-m boot,
//! before the Pigweed kernel takes over. Anything that must touch
//! board/SoC-global state (SCU clocks, pin mux, debug aids) belongs
//! here. Per-peripheral *operating* state (controller reset, FIFOs,
//! IER bits) lives in the per-task backend's constructor — userspace
//! tasks can't reach the SCU because it's not in their MPU mappings.
//!
//! # Boot order
//!
//! ```text
//! reset vector  ──►  cortex_m_rt::pre_init  ──►  cortex_m_rt::entry  ──►  kernel::main
//!                    │ optional, runs       │ this fn               │
//!                    │ before .bss/.data    │ RAM is initialized    │
//!                    │ are initialized      │ by now                │
//! ```
//!
//! # When extending this file
//!
//! For each peripheral a new system image for this target wants to
//! drive, add the corresponding SCU init **before** `kernel::main`:
//!
//! - I2C bus N: `Pinctrl::apply_pinctrl_group(PINCTRL_I2CN)`. The
//!   global `init_i2c_global()` is one-shot and idempotent — call it
//!   once even if you add multiple buses.
//! - Future UART other than UART5 (debug): add `PINCTRL_UART<n>` once
//!   the constants land in `pinctrl::`.
//! - HACE / crypto: add an `init_hace_global()` analog when that
//!   peripheral driver lands.
//!
//! Pin mux only the buses your board actually uses. Each
//! `apply_pinctrl_group` writes SCU multi-function registers that
//! steal pins away from GPIO/I3C/etc.; muxing buses you don't need
//! can stomp on signals the board uses for something else.
//!
//! # Optional pre-init hook
//!
//! `cortex_m_rt::pre_init` runs even earlier — before `.bss`/`.data`
//! are initialized. Useful for JTAG debug halts or anything that
//! must happen with no Rust state available. Add behind a feature
//! gate so the production build stays lean. Example:
//!
//! ```ignore
//! #[cfg(feature = "jtag-halt")]
//! #[cortex_m_rt::pre_init]
//! unsafe fn pre_kernel_init() {
//!     // Mux JTAG pins via SCU41C, busy-loop on a HALT word the
//!     // debugger clears.
//! }
//! ```

#![no_std]
#![no_main]

use arch_arm_cortex_m::Arch;
use ast10x0_peripherals::i2c::init_i2c_global;
use ast10x0_peripherals::pinctrl::{Pinctrl, PINCTRL_I2C0};

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn pw_assert_HandleFailure() -> ! {
    use kernel::Arch as _;
    Arch::panic()
}

#[cortex_m_rt::entry]
fn main() -> ! {
    kernel::static_init_state!(static mut INIT_STATE: InitKernelState<Arch>);

    // ── Pre-kernel platform init ─────────────────────────────────────
    //
    // Configures SCU globals + pin mux for every peripheral any of this
    // target's system images drives. Userspace tasks can't reach the
    // SCU (it's outside their MPU mappings), so this is the only place
    // the writes can happen.
    //
    // Add an entry per bus a new system image needs (see file-level
    // docs).

    // I2C global: SCU reset, I2CG0C config, I2CG10 clock dividers.
    // Idempotent; safe to keep even if no I2C image is in this build.
    init_i2c_global();

    // I2C bus 0 — the bus the `tests/i2c` system image owns.
    Pinctrl::apply_pinctrl_group(PINCTRL_I2C0);

    // ────────────────────────────────────────────────────────────────

    #[allow(static_mut_refs)]
    kernel::main(Arch, unsafe { &mut INIT_STATE });
}
