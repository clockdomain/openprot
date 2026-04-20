// Licensed under the Apache-2.0 license

//! Virtual AST1060-EVB boot entry point for QEMU.
//!
//! Initializes the ARM Cortex-M and hands off to the Pigweed kernel.
//! Also runs the I2C global + pinmux setup that `services/i2c/server`'s
//! `init_bus(n)` declares as a precondition — without it, the master
//! side of the Aspeed controllers never produces valid ACK/NACK results
//! even in QEMU.

#![no_std]
#![no_main]

use arch_arm_cortex_m::Arch;

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub extern "C" fn pw_assert_HandleFailure() -> ! {
    use kernel::Arch as _;
    Arch::panic()
}

// ── Interrupt Handler Stubs ──
// Required by the ast1060-pac's __INTERRUPTS vector table; same list as
// `//target/ast1060-evb:entry`.
macro_rules! default_handler {
    ($($name:ident),*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name() {
                loop {}
            }
        )*
    };
}

default_handler!(
    fmc, gpio, hace,
    i2c, i2c1, i2c2, i2c3, i2c4, i2c5, i2c6, i2c7, i2c8, i2c9, i2c10, i2c11, i2c12, i2c13,
    i2cfilter,
    i3c, i3c1, i3c2, i3c3,
    scu, sgpiom,
    spi, spi1, spipf1, spipf2, spipf3,
    timer1, timer2, timer3, timer4, timer5, timer6, timer7,
    uart, uartdma, wdt
);

/// Initialize the I2C subsystem (SCU globals + pin mux for I2C1/I2C2).
/// Mirrors `ast1060-evb/entry.rs::i2c_init()`.
fn i2c_init() {
    aspeed_ddk::i2c_core::init_i2c_global();
    aspeed_ddk::pinctrl::Pinctrl::apply_pinctrl_group(aspeed_ddk::pinctrl::PINCTRL_I2C1);
    aspeed_ddk::pinctrl::Pinctrl::apply_pinctrl_group(aspeed_ddk::pinctrl::PINCTRL_I2C2);
}

#[cortex_m_rt::entry]
fn main() -> ! {
    kernel::static_init_state!(static mut INIT_STATE: InitKernelState<Arch>);

    i2c_init();

    #[allow(static_mut_refs)]
    kernel::main(Arch, unsafe { &mut INIT_STATE });
}
