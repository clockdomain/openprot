// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Standalone reimplementation of caliptra-mcu-sw's
//! `builder::rom::append_rom_digest()` (`builder/src/rom.rs`). Pads the raw
//! MCU ROM binary to the platform's full `rom_size` and appends a SHA-384
//! digest of the preceding bytes to the last 48 bytes, so that ROM's own
//! `cold_boot::rom_digest_integrity()` self-check
//! (`rom/src/cold_boot.rs`) passes at boot instead of comparing against an
//! unpatched all-zero tail.
//!
//! Usage: `mcu_rom_digest <input_bin> <output_bin>`
//!
//! `rom_size` is read from `mcu_config_emulator::EMULATOR_MEMORY_MAP.rom_size`
//! (the same source `generate_rom_ld.rs`, in this same directory, uses for
//! the ROM region's `LENGTH` in the generated linker script), rather than a
//! second hardcoded copy of the constant on the Bazel side.
//!
//! # Upstream cross-reference
//!
//! Mirrors `pub fn append_rom_digest(binary: &PathBuf, rom_size: usize) ->
//! Result<()>` in `caliptra-mcu-sw/builder/src/rom.rs` (around line 61).
//! That function lives in the `mcu-builder` crate, which we cannot depend on
//! from Bazel for the same reason `generate_rom_ld.rs` (this same directory)
//! cannot: its sole dependency is `caliptra_builder`, which has unresolved
//! upstream deps. Upstream's version calls
//! `caliptra_image_crypto::RustCrypto::sha384_digest()` (a plain SHA-384)
//! then `caliptra_image_gen::from_hw_format()` on the result; tracing both
//! functions (`image/crypto/src/rustcrypto.rs`, `image/gen/src/lib.rs`)
//! shows `from_hw_format(to_hw_format(x)) == x` (each round-trips the same
//! bytes through `u32::from_be_bytes`/`to_be_bytes` on the same 4-byte
//! chunks), so the two calls together are exactly a plain SHA-384 digest —
//! this file uses `sha2::Sha384` directly instead of pulling in those two
//! extra crates, matching `caliptra_rom_packager.rs`'s existing use of
//! `sha2` for the analogous Caliptra-ROM digest.

use std::env;
use std::fs;

use anyhow::{bail, Context, Result};
use mcu_config_emulator::EMULATOR_MEMORY_MAP;
use sha2::{Digest, Sha384};

const DIGEST_SIZE: usize = 48;

fn append_rom_digest(data: &mut Vec<u8>, rom_size: usize) -> Result<()> {
    let digest_offset = rom_size.checked_sub(DIGEST_SIZE).ok_or_else(|| {
        anyhow::anyhow!("rom_size {rom_size} smaller than digest size {DIGEST_SIZE}")
    })?;
    if data.len() > digest_offset {
        bail!(
            "ROM binary is {} bytes, which does not leave room for the {}-byte \
             SHA-384 digest within the {}-byte ROM region. Reduce ROM size or \
             increase the platform's rom_size.",
            data.len(),
            DIGEST_SIZE,
            rom_size,
        );
    }
    data.resize(rom_size, 0);
    let digest: [u8; DIGEST_SIZE] = Sha384::digest(&data[0..digest_offset]).into();
    data[digest_offset..].copy_from_slice(&digest);
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        bail!(
            "usage: {} <input_bin> <output_bin>",
            args.first().map(String::as_str).unwrap_or("mcu_rom_digest")
        );
    }
    let input_path = &args[1];
    let output_path = &args[2];
    let rom_size = EMULATOR_MEMORY_MAP.rom_size as usize;

    let mut data =
        fs::read(input_path).with_context(|| format!("failed to read MCU ROM bin {input_path}"))?;
    append_rom_digest(&mut data, rom_size)?;
    fs::write(output_path, &data)
        .with_context(|| format!("failed to write MCU ROM bin {output_path}"))?;
    Ok(())
}
