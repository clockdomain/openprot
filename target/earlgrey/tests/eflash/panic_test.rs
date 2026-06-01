// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]
use panic_test_codegen::handle;
use pw_status::Error;
use userspace::entry;

use earlgrey_util::EarlgreyFlashAddress;
use hal_flash::{Flash, FlashAddress};
use services_flash_client::FlashIpcClient;
use util_error::ErrorCode;
use util_ipc::IpcChannel;

fn panic_test() -> Result<(), ErrorCode> {
    let mut flash = FlashIpcClient::new(IpcChannel::new(handle::FLASH_SERVICE))?;

    let (total_size, page_size, _erasable_sizes_bitmap) = flash.geometry();
    pw_log::info!("Flash size: {}", total_size.get() as usize);
    pw_log::info!("Flash page size: {}", page_size.get() as usize);

    // Test that basic read operations don't panic
    let mut buf = [0u8; 32];
    flash.read(FlashAddress::data(0), &mut buf)?;
    pw_log::info!("Read completed without panic");

    // Test erase operations don't panic
    flash.erase(FlashAddress::data(0x90000), page_size)?;
    pw_log::info!("Erase completed without panic");

    Ok(())
}

#[entry]
fn entry() -> Result<(), Error> {
    pw_log::info!("🔄 RUNNING PANIC TEST");
    let ret = panic_test();

    let ret = match ret {
        Ok(()) => {
            pw_log::info!("✅ PASS");
            Ok(())
        }
        Err(e) => {
            pw_log::error!("❌ FAIL: {:08x}", u32::from(e) as u32);
            Err(Error::Unknown)
        }
    };

    ret
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    pw_log::error!("FAIL: panic in {}", module_path!() as &str);
    loop {}
}
