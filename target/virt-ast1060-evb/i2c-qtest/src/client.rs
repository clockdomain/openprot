// Licensed under the Apache-2.0 license

//! I2C qtest scenario dispatcher.
//!
//! Runs as a userspace app alongside `i2c_server`. Polls the
//! `aspeed-qtest-ctrl` scratchpad at `0x7e7d_0000` (landed on
//! `clockdomain/qemu@ast10x0-i2c` commit 4f424a0). On a non-zero
//! scenario id written by qtest, dispatches to the matching routine,
//! writes status and result bytes back through the scratchpad, then
//! zeroes the id to signal completion.
//!
//! MMIO layout matches the plan:
//!
//! | off  | reg            | notes                                       |
//! |------|----------------|---------------------------------------------|
//! | 0x00 | R_SCENARIO_ID  | qtest writes to dispatch; fw zeroes on done |
//! | 0x04 | R_READY        | fw sets 1 after IPC client init             |
//! | 0x08 | R_STATUS       | 0=running, 1=pass, 2=fail, 3=armed          |
//! | 0x0C | R_RESULT_LEN   | bytes valid in R_RESULT                     |
//! | 0x10 | R_RESULT[0..32]| scenario-specific bytes                     |

#![no_main]
#![no_std]

use app_i2c_qtest_client::handle;
use i2c_api::{
    BusIndex, I2cAddress, I2cClient, I2cClientBlocking, I2cTargetClient, SlaveEventKind,
};
use i2c_client::IpcI2cClient;
use userspace::entry;
use userspace::syscall;

// ─── qtest-ctrl MMIO ────────────────────────────────────────────────────────

const QTEST_CTRL_BASE: usize = 0x7e7d_0000;
const R_SCENARIO_ID: usize = 0x00;
const R_READY: usize = 0x04;
const R_STATUS: usize = 0x08;
const R_RESULT_LEN: usize = 0x0c;
const R_RESULT: usize = 0x10;
const RESULT_CAP: usize = 32;

const STATUS_RUNNING: u32 = 0;
const STATUS_PASS: u32 = 1;
const STATUS_FAIL: u32 = 2;
const STATUS_ARMED: u32 = 3;

struct QtestCtrl;

impl QtestCtrl {
    #[inline]
    fn read_u32(off: usize) -> u32 {
        unsafe { ((QTEST_CTRL_BASE + off) as *const u32).read_volatile() }
    }

    #[inline]
    fn write_u32(off: usize, val: u32) {
        unsafe { ((QTEST_CTRL_BASE + off) as *mut u32).write_volatile(val) };
    }

    fn scenario(&self) -> u32 {
        Self::read_u32(R_SCENARIO_ID)
    }

    fn clear_scenario(&self) {
        Self::write_u32(R_SCENARIO_ID, 0);
    }

    fn set_ready(&self) {
        Self::write_u32(R_READY, 1);
    }

    fn set_status(&self, s: u32) {
        Self::write_u32(R_STATUS, s);
    }

    fn set_result(&self, bytes: &[u8]) {
        // The aspeed-qtest-ctrl device truncates multi-byte writes to the
        // result region (it stores `value & 0xff` per access), so we have
        // to write one byte at a time.
        let n = bytes.len().min(RESULT_CAP);
        for i in 0..n {
            unsafe {
                ((QTEST_CTRL_BASE + R_RESULT + i) as *mut u8)
                    .write_volatile(bytes[i]);
            }
        }
        Self::write_u32(R_RESULT_LEN, n as u32);
    }

    fn reset_for_scenario(&self) {
        self.set_status(STATUS_RUNNING);
        Self::write_u32(R_RESULT_LEN, 0);
    }
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn pass(ctrl: &QtestCtrl, result: &[u8]) {
    ctrl.set_result(result);
    ctrl.set_status(STATUS_PASS);
}

fn fail(ctrl: &QtestCtrl) {
    ctrl.set_status(STATUS_FAIL);
}

fn fail_with(ctrl: &QtestCtrl, tag: &[u8]) {
    ctrl.set_result(tag);
    ctrl.set_status(STATUS_FAIL);
}

// ─── scenarios ──────────────────────────────────────────────────────────────

/// Scenario 1 — configure_slave address validation.
///
/// Exercises `I2cAddress::new()` via `configure_target_address`. Records a
/// bitmask where each bit corresponds to a probed address matching its
/// expected outcome. Passes if all four bits are set.
///
/// | bit | address | expected                             |
/// |-----|---------|--------------------------------------|
/// |  0  | 0x42    | Ok (valid 7-bit)                     |
/// |  1  | 0x77    | Ok (last valid before reserved)      |
/// |  2  | 0x80    | Err (OutOfRange)                     |
/// |  3  | 0x78    | Err (Reserved, 10-bit ext range)     |
fn scenario_1_configure_slave(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    // i2c_server currently only initializes bus 2, so all scenarios use it.
    const BUS: BusIndex = BusIndex::new(2);
    let mut mask: u8 = 0;

    // 0x42 — valid
    match I2cAddress::new(0x42) {
        Ok(a) => {
            if ipc.configure_target_address(BUS, a).is_ok() {
                mask |= 1 << 0;
            }
        }
        Err(_) => {}
    }

    // 0x77 — valid, last before reserved range
    match I2cAddress::new(0x77) {
        Ok(a) => {
            if ipc.configure_target_address(BUS, a).is_ok() {
                mask |= 1 << 1;
            }
        }
        Err(_) => {}
    }

    // 0x80 — OutOfRange at client-side validation
    if I2cAddress::new(0x80).is_err() {
        mask |= 1 << 2;
    }

    // 0x78 — Reserved (10-bit ext range)
    if I2cAddress::new(0x78).is_err() {
        mask |= 1 << 3;
    }

    if mask == 0x0f {
        pass(ctrl, &[mask]);
    } else {
        fail_with(ctrl, &[mask]);
    }
}

/// Scenario 5 — master_write + master_read against tmp105 on bus 1, addr 0x4d.
/// Expects the two-byte temperature register.
fn scenario_5_master_ack(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    let addr = match I2cAddress::new(0x4d) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    let mut buf = [0u8; 2];
    match ipc.write_read(BusIndex::new(2), addr, &[0x00], &mut buf) {
        Ok(n) if n == 2 => pass(ctrl, &buf),
        Ok(_) => fail_with(ctrl, b"short"),
        Err(_) => fail_with(ctrl, b"xfer"),
    }
}

/// Scenario 6 — NACK path. Probe a vacant address on bus 1; expect NoAck.
fn scenario_6_nack(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    // 0x10 is valid per I2cAddress::new() and not populated on bus 1.
    let addr = match I2cAddress::new(0x10) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    // Any Err from a vacant-address write is treated as NACK. The only
    // success path is an unexpected ACK, which is a scenario-level fail.
    match ipc.write(BusIndex::new(2), addr, &[]) {
        Ok(()) => fail_with(ctrl, b"ack"),
        Err(_) => pass(ctrl, &[]),
    }
}

/// Scenario 7 — register-read loop against tmp105 on bus 1.
///
/// Reads 5 registers back-to-back and stores each single-byte value in
/// R_RESULT.
fn scenario_7_register_loop(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    let addr = match I2cAddress::new(0x4d) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    const REGS: [u8; 5] = [0x00, 0x01, 0x02, 0x03, 0x00];
    let mut out = [0u8; 5];
    for (i, &reg) in REGS.iter().enumerate() {
        let mut byte = [0u8; 1];
        match ipc.write_read(BusIndex::new(2), addr, &[reg], &mut byte) {
            Ok(_) => out[i] = byte[0],
            Err(_) => return fail_with(ctrl, b"read"),
        }
    }
    pass(ctrl, &out);
}

/// Scenario 8 — write-read on pca9552 on bus 2, addr 0x60.
///
/// Writes to the LS0 LED output register (0x06) and reads it back.
fn scenario_8_pca9552(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    let addr = match I2cAddress::new(0x60) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    const REG_LS0: u8 = 0x06;
    const PATTERN: u8 = 0xaa;

    if ipc
        .write(BusIndex::new(2), addr, &[REG_LS0, PATTERN])
        .is_err()
    {
        return fail_with(ctrl, b"write");
    }
    let mut back = [0u8; 1];
    match ipc.write_read(BusIndex::new(2), addr, &[REG_LS0], &mut back) {
        Ok(_) => pass(ctrl, &back),
        Err(_) => fail_with(ctrl, b"read"),
    }
}

/// Scenario 9 — slave-mode RX. Configure bus 3 as a target at 0x55, arm,
/// then wait for a DataReceived event. qtest drives i2c-test-master to
/// write N bytes; firmware publishes the received bytes in R_RESULT.
fn scenario_9_slave_rx(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    // i2c_server currently only initializes bus 2, so all scenarios use it.
    const BUS: BusIndex = BusIndex::new(2);
    let addr = match I2cAddress::new(0x55) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    if ipc.configure_target_address(BUS, addr).is_err() {
        return fail_with(ctrl, b"cfg");
    }
    if ipc.enable_receive(BUS).is_err() {
        return fail_with(ctrl, b"ena");
    }

    // Tell qtest we're armed and waiting for a master write.
    ctrl.set_status(STATUS_ARMED);

    let mut buf = [0u8; RESULT_CAP];
    match ipc.slave_wait_event(BUS, &mut buf) {
        Ok((SlaveEventKind::DataReceived, n)) => pass(ctrl, &buf[..n]),
        Ok((_, _)) => fail_with(ctrl, b"kind"),
        Err(_) => fail_with(ctrl, b"evt"),
    }
}

/// Scenario 10 — slave-mode TX. Pre-load the slave response, arm, and wait
/// for a ReadRequest event confirming the master read was served.
fn scenario_10_slave_tx(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    // i2c_server currently only initializes bus 2, so all scenarios use it.
    const BUS: BusIndex = BusIndex::new(2);
    const PATTERN: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let addr = match I2cAddress::new(0x55) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    if ipc.configure_target_address(BUS, addr).is_err() {
        return fail_with(ctrl, b"cfg");
    }
    if ipc.enable_receive(BUS).is_err() {
        return fail_with(ctrl, b"ena");
    }
    if ipc.slave_set_response(BUS, &PATTERN).is_err() {
        return fail_with(ctrl, b"rsp");
    }

    ctrl.set_status(STATUS_ARMED);

    let mut discard = [0u8; 8];
    match ipc.slave_wait_event(BUS, &mut discard) {
        Ok((SlaveEventKind::ReadRequest, _)) => pass(ctrl, &[PATTERN.len() as u8]),
        Ok((_, _)) => fail_with(ctrl, b"kind"),
        Err(_) => fail_with(ctrl, b"evt"),
    }
}

/// Scenario 11 — slave rebind. Configure at 0x55, then rebind to 0x56.
/// Passes if both IPC calls succeed — external address-probe validation is
/// performed by the qtest side.
fn scenario_11_rebind(ipc: &mut IpcI2cClient, ctrl: &QtestCtrl) {
    // i2c_server currently only initializes bus 2, so all scenarios use it.
    const BUS: BusIndex = BusIndex::new(2);
    let first = match I2cAddress::new(0x55) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    let second = match I2cAddress::new(0x56) {
        Ok(a) => a,
        Err(_) => return fail(ctrl),
    };
    if ipc.configure_target_address(BUS, first).is_err() {
        return fail_with(ctrl, b"cfg1");
    }
    if ipc.enable_receive(BUS).is_err() {
        return fail_with(ctrl, b"ena1");
    }
    // Rebind.
    if ipc.configure_target_address(BUS, second).is_err() {
        return fail_with(ctrl, b"cfg2");
    }
    if ipc.enable_receive(BUS).is_err() {
        return fail_with(ctrl, b"ena2");
    }
    pass(ctrl, &[0x55, 0x56]);
}

// ─── main loop ──────────────────────────────────────────────────────────────

fn run() -> ! {
    let mut ipc = IpcI2cClient::new(handle::I2C);
    let ctrl = QtestCtrl;

    ctrl.set_ready();
    pw_log::info!("i2c_qtest_client ready");

    loop {
        // Busy-poll the scratchpad. Not cycle-efficient but keeps the
        // dispatcher simple; qtest fires scenarios infrequently.
        let id = loop {
            let v = ctrl.scenario();
            if v != 0 {
                break v;
            }
            core::hint::spin_loop();
        };

        ctrl.reset_for_scenario();

        match id {
            1 => scenario_1_configure_slave(&mut ipc, &ctrl),
            5 => scenario_5_master_ack(&mut ipc, &ctrl),
            6 => scenario_6_nack(&mut ipc, &ctrl),
            7 => scenario_7_register_loop(&mut ipc, &ctrl),
            8 => scenario_8_pca9552(&mut ipc, &ctrl),
            9 => scenario_9_slave_rx(&mut ipc, &ctrl),
            10 => scenario_10_slave_tx(&mut ipc, &ctrl),
            11 => scenario_11_rebind(&mut ipc, &ctrl),
            _ => fail_with(&ctrl, b"unknown"),
        }

        ctrl.clear_scenario();
    }
}

#[entry]
fn entry() -> ! {
    run();
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    let _ = syscall::debug_shutdown(Err(pw_status::Error::Unknown));
    loop {}
}
