// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0
#![no_std]

use core::ops::Range;
use memory_config::MemoryRegion;

pub use kernel_config::{
    ClintTimerConfigInterface, ExceptionMode, KernelConfigInterface, RiscVKernelConfigInterface,
    VeerPicConfigInterface,
};

//const RAM_BASE: usize = 0x4000_0000;
//const RAM_SIZE: usize = 384*1024;
//
//const UART0_BASE: usize = 0x1000_1000;
//const UART0_SIZE: usize = 0x100;
//
//const TIMER_BASE: usize = 0x4010_0000;
//const TIMER_SIZE: usize = 0x200;
//
//const PLIC_SIZE: usize = 0x0000_5400;
const PIC_BASE: usize = 0x6000_0000;

pub struct KernelConfig;

impl KernelConfigInterface for KernelConfig {
    #[cfg(feature = "silicon")]
    const SYSTEM_CLOCK_HZ: u64 = 100_000_000;
    #[cfg(feature = "fpga")]
    const SYSTEM_CLOCK_HZ: u64 = 10_000_000; //FIXME
    #[cfg(feature = "emulator")]
    const SYSTEM_CLOCK_HZ: u64 = 1_000_000;
}

impl RiscVKernelConfigInterface for KernelConfig {
    type Timer = TimerConfig;

    const MTIME_HZ: u64 = KernelConfig::SYSTEM_CLOCK_HZ;
    const PMP_ENTRIES: usize = 16;
    const PMP_USERSPACE_ENTRIES: Range<usize> = Range {
        start: 0usize,
        end: 16usize,
    };
    const PMP_GRANULARITY: usize = 0;
    const KERNEL_MEMORY_REGIONS: &'static [MemoryRegion] = &[];

    fn get_exception_mode() -> ExceptionMode {
        ExceptionMode::Vectored(unsafe { MTVEC_TABLE.as_ptr() as usize })
    }
}

pub struct VeerPicConfig;

impl VeerPicConfigInterface for VeerPicConfig {
    const PIC_BASE_ADDRESS: usize = PIC_BASE;
    const MAX_IRQS: u32 = 256;
    // The VeeR core requires the external-interrupt redirect table to live
    // in DCCM. The start of DCCM (0x5000_0000) overlaps the ROM-dedicated RAM
    // that the boot ROM/runtime uses as scratch, so the table must sit at the
    // top of the window to avoid being clobbered. The runner configures a
    // 16KiB DCCM (0x5000_0000..0x5000_4000); the redirect table (256 IRQs *
    // 4 bytes = 1KiB) is placed in the final 1KiB, matching the reference
    // firmware which links it at ORIGIN(dccm) + LENGTH(dccm) - 1024.
    const MEIVT_BASE_ADDRESS: usize = 0x5000_3C00;
}

// Compile-time validation of the VeeR PIC constants above. This is a
// module-level `const _`, not an associated const in an `impl` block: rustc
// evaluates the former unconditionally, but the latter only where it is
// referenced, which would silently disable these checks.
const _: () = {
    // MEIVT stores MEIHAP[31:10], so the table base must be 1KiB-aligned.
    assert!(<VeerPicConfig as VeerPicConfigInterface>::MEIVT_BASE_ADDRESS & 0x3ff == 0);
    // claimid is 8 bits, so the core can address at most 256 table entries.
    assert!(<VeerPicConfig as VeerPicConfigInterface>::MAX_IRQS <= 256);
};

pub struct TimerConfig;

const TIMER_BASE: usize = 0x2100_0000;

impl ClintTimerConfigInterface for TimerConfig {
    const MTIME_REGISTER: usize = TIMER_BASE + 0xe4;
    const MTIMECMP_REGISTER: usize = TIMER_BASE + 0xec;
}

unsafe extern "C" {
    #[link_name = "_mtvec_table"]
    static MTVEC_TABLE: [u32; 32];
}
