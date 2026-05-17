// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! HACE command and algorithm constants shared across hash and digest modules.

pub const HACE_SHA_BE_EN: u32 = 1 << 3;
pub const HACE_CMD_ACC_MODE: u32 = 1 << 8;
pub const HACE_SG_EN: u32 = 1 << 18;
pub const HACE_SG_LAST: u32 = 1 << 31;
pub const HACE_ALGO_SHA256: u32 = (1 << 4) | (1 << 6);
pub const HACE_ALGO_SHA512: u32 = (1 << 5) | (1 << 6);
pub const HACE_ALGO_SHA384: u32 = (1 << 5) | (1 << 6) | (1 << 10);
pub const SHA256_HASH_CMD: u32 = HACE_CMD_ACC_MODE | HACE_SHA_BE_EN | HACE_SG_EN | HACE_ALGO_SHA256;
pub const SHA384_HASH_CMD: u32 = HACE_CMD_ACC_MODE | HACE_SHA_BE_EN | HACE_SG_EN | HACE_ALGO_SHA384;
pub const SHA512_HASH_CMD: u32 = HACE_CMD_ACC_MODE | HACE_SHA_BE_EN | HACE_SG_EN | HACE_ALGO_SHA512;

// ----- AES (crypto sub-engine) command bits -----------------------------
//
// Verbatim from the pinned authority `zephyr-reference/hace_aspeed.h` (the
// `HACE_CMD_*` macros; goal.md §1.9.2). `HACE_SG_LAST` (1<<31) above doubles
// as the crypto SG single/last terminator OR'd into `src/dst` SG length words
// (`hace_aspeed.c:132-133`).

/// `HACE_CMD_MBUS_REQ_SYNC_EN` (`hace_aspeed.h:17`).
pub const HACE_CMD_MBUS_REQ_SYNC_EN: u32 = 1 << 20;
/// `HACE_CMD_DES_SG_CTRL` (`hace_aspeed.h:18`).
pub const HACE_CMD_DES_SG_CTRL: u32 = 1 << 19;
/// `HACE_CMD_SRC_SG_CTRL` (`hace_aspeed.h:19`).
pub const HACE_CMD_SRC_SG_CTRL: u32 = 1 << 18;
/// `HACE_CMD_AES_KEY_HW_EXP` — hardware key expansion (`hace_aspeed.h:25`).
pub const HACE_CMD_AES_KEY_HW_EXP: u32 = 1 << 13;
/// `HACE_CMD_AES_SELECT == 0` (`hace_aspeed.h:22`).
pub const HACE_CMD_AES_SELECT: u32 = 0;
/// `HACE_CMD_ENCRYPT` (`hace_aspeed.h:28`); decrypt is the absence of this bit.
pub const HACE_CMD_ENCRYPT: u32 = 1 << 7;
/// `HACE_CMD_ECB == 0` (`hace_aspeed.h:29`).
pub const HACE_CMD_ECB: u32 = 0;
/// `HACE_CMD_CBC` (`hace_aspeed.h:30`).
pub const HACE_CMD_CBC: u32 = 0x1 << 4;
/// `HACE_CMD_AES128 == 0` (`hace_aspeed.h:34`).
pub const HACE_CMD_AES128: u32 = 0;
/// `HACE_CMD_AES256` (`hace_aspeed.h:36`).
pub const HACE_CMD_AES256: u32 = 0x2 << 2;

/// Fixed AES session base, `aspeed_crypto_session_setup` (`hace_aspeed.c:264`,
/// `:269`): SG control + MBUS sync + HW key expansion + AES select.
pub const AES_CMD_BASE: u32 = HACE_CMD_DES_SG_CTRL
    | HACE_CMD_SRC_SG_CTRL
    | HACE_CMD_MBUS_REQ_SYNC_EN
    | HACE_CMD_AES_KEY_HW_EXP
    | HACE_CMD_AES_SELECT;

// ----- AES OTP/secret-vault sideload-key select (delta A6) --------------
//
// Verbatim port of the `aspeed_aes_crypt` non-`CAP_RAW_KEY` branch and the
// `SELECT_VAL_KEY_1/2` macros (`zephyr-reference/hace_aspeed.c:113-128`,
// `hace_aspeed.h:16,193-199`; goal.md §2.3 delta A6 / §2.6). This is the
// *select logic only* — pure, silicon-free, and the in-scope half of A6. The
// vault crypto end-to-end is separated and hardware-gated (goal.md §2.6).

/// `HACE_CMD_AES_KEY_FROM_OTP` — sources the AES key from OTP/the secret
/// vault instead of the software context (`hace_aspeed.h:16`, `BIT(24)`).
pub const HACE_CMD_AES_KEY_FROM_OTP: u32 = 1 << 24;

/// Byte offset of the vault-key-select register from the crypto engine
/// secure base (`sbase`): `SELECT_VAL_KEY_1/2` operate on `sbase + 0xc`
/// (`hace_aspeed.h:194,198`). Resolving `sbase` to a real MMIO address is
/// board/provisioning state — the hardware-gated seam, goal.md §2.6.
pub const VAULT_KEY_SELECT_OFFSET: usize = 0xc;

/// Which provisioned vault slot a key handle selects. The driver accepts a
/// 1-byte handle: `1` → slot 1, `2` → slot 2, anything else → `-EINVAL`
/// (`hace_aspeed.c:116-126`).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VaultKeySlot {
    Slot1,
    Slot2,
}

/// Decode the 1-byte vault key handle. `None` is the authority's `-EINVAL`
/// (verbatim ladder: `if (key_id == 1) … else if (key_id == 2) … else …`,
/// `hace_aspeed.c:116-126`). The caller maps `None` to the typed error,
/// exactly where the C returns `-EINVAL`.
#[inline]
pub const fn decode_vault_key_id(key_id: u8) -> Option<VaultKeySlot> {
    match key_id {
        1 => Some(VaultKeySlot::Slot1),
        2 => Some(VaultKeySlot::Slot2),
        _ => None,
    }
}

/// Read-modify-write applied to the `sbase + 0xc` vault-select register.
///
/// Verbatim port of the two macros — **including their deliberate
/// asymmetry**, reproduced bit-for-bit, *not* "corrected" (parity discipline;
/// goal.md §2.6):
///
/// - `SELECT_VAL_KEY_1`: `reg &= ~BIT(0)` — clear bit 0, preserve the rest
///   (`hace_aspeed.h:194`).
/// - `SELECT_VAL_KEY_2`: `reg &= BIT(0)` — keep *only* bit 0, clear all other
///   bits (`hace_aspeed.h:198`). (Asymmetric vs. KEY_1 by the authority's own
///   definition; preserved as a behavioral fact, not a bug to fix.)
#[inline]
pub const fn vault_select_rmw(slot: VaultKeySlot, cur: u32) -> u32 {
    match slot {
        VaultKeySlot::Slot1 => cur & !(1 << 0),
        VaultKeySlot::Slot2 => cur & (1 << 0),
    }
}

/// `data->cmd |= HACE_CMD_AES_KEY_FROM_OTP` (`hace_aspeed.c:127`): the
/// command-word change the vault path makes on top of the session base cmd.
#[inline]
pub const fn aes_key_from_otp(base_cmd: u32) -> u32 {
    base_cmd | HACE_CMD_AES_KEY_FROM_OTP
}

// Compile-time bit-exact parity vs. the frozen source — enforced on *every*
// build (firmware included), not just under a test runner. These are the A6
// select-logic acceptance assertions (goal.md §3 item 7).
const _: () = {
    // Handle decode ladder (hace_aspeed.c:116-126).
    assert!(matches!(decode_vault_key_id(1), Some(VaultKeySlot::Slot1)));
    assert!(matches!(decode_vault_key_id(2), Some(VaultKeySlot::Slot2)));
    assert!(decode_vault_key_id(0).is_none());
    assert!(decode_vault_key_id(3).is_none());
    assert!(decode_vault_key_id(255).is_none());
    // SELECT_VAL_KEY_1: clear bit0, preserve the rest (hace_aspeed.h:194).
    assert!(vault_select_rmw(VaultKeySlot::Slot1, 0xFFFF_FFFF) == 0xFFFF_FFFE);
    assert!(vault_select_rmw(VaultKeySlot::Slot1, 0x0000_0001) == 0x0000_0000);
    assert!(vault_select_rmw(VaultKeySlot::Slot1, 0xA5A5_A5A4) == 0xA5A5_A5A4);
    // SELECT_VAL_KEY_2: keep only bit0, clear everything else (hace_aspeed.h:198).
    assert!(vault_select_rmw(VaultKeySlot::Slot2, 0xFFFF_FFFF) == 0x0000_0001);
    assert!(vault_select_rmw(VaultKeySlot::Slot2, 0xA5A5_A5A4) == 0x0000_0000);
    assert!(vault_select_rmw(VaultKeySlot::Slot2, 0x1234_5679) == 0x0000_0001);
    // cmd |= AES_KEY_FROM_OTP sets exactly bit 24, nothing else.
    assert!(aes_key_from_otp(0) == (1 << 24));
    assert!(aes_key_from_otp(AES_CMD_BASE) == AES_CMD_BASE | (1 << 24));
    assert!(AES_CMD_BASE & HACE_CMD_AES_KEY_FROM_OTP == 0); // raw path never sets it
};

pub const DEFAULT_POLL_BUDGET: u32 = 1_000_000;

/// Suggested wait window, in nanoseconds, passed to the cooperative `yield_fn`
/// between completion polls. Mirrors the reference HACE driver's 1 µs poll
/// interval (`reg_read_poll_timeout(..., 1, 3000)`). Advisory only: the
/// injected strategy decides whether/how to honor it (`spin_loop` ignores it;
/// an async/RTOS strategy may sleep for it).
pub const POLL_YIELD_NS: u32 = 1_000;
