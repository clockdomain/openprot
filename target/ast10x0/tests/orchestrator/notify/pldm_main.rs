// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! PLDM side of the notify-channel QEMU test.
//!
//! Drives [`NotifyChannel`] from a loop shaped like `run_terminus`: service
//! the orchestrator's channel once, then spend time on "MCTP work". That
//! interleaving is exactly what the host tests cannot reach — whether the
//! supervisor gets answered *between* Update Agent commands rather than only
//! when the firmware device happens to be idle.
//!
//! The script is driven by a count of answered requests rather than by wall
//! time, so it cannot race the other process:
//!
//! | After request | What PLDM does                                       |
//! |---------------|------------------------------------------------------|
//! | 2             | latches `UpdateRequested` and nudges the peer signal  |
//! | 5             | goes silent for good — the bounded-deadline scenario  |

#![no_main]
#![no_std]

use app_notify_pldm::handle;
use notify_api::Pending;
use notify_server_runtime::NotifyChannel;
use userspace::entry;
use userspace::syscall::{self, Signals};
use userspace::time::{Clock, Duration, SystemClock};

/// Latch the simulated Update Agent event once this many requests have been
/// answered — straight after the orchestrator's first, empty poll, so the
/// latch lands while it is still on its way to `object_wait`. That is the
/// level-triggered race: the signal must already be asserted when it parks.
const LATCH_AFTER_REQUESTS: u32 = 2;

/// Stop answering after this many requests. The orchestrator's next
/// round-trip has to end at its own bounded deadline.
const SILENT_AFTER_REQUESTS: u32 = 5;

/// Stand-in for the terminus loop's MCTP responder poll: the window in which
/// PLDM is busy and not looking at the notify channel.
const MCTP_WORK: Duration = Duration::from_millis(2);

#[entry]
fn entry() {
    let mut channel = NotifyChannel::new(handle::NOTIFY);
    let mut answered: u32 = 0;

    pw_log::info!("PLDM: notify channel up");

    loop {
        // Stand-in for the terminus loop's MCTP responder poll: park for up to
        // MCTP_WORK, waking early if the supervisor has something for us. This
        // has to yield, or the loop starves the other process and the test
        // measures scheduling noise instead of the seam.
        //
        // Not `time::sleep_until`: on this target it returns early and does
        // not delay at all — see the note in the sgpiom IRQ test.
        let _ = syscall::object_wait(
            handle::NOTIFY,
            Signals::READABLE,
            SystemClock::now() + MCTP_WORK,
        );

        // Once silent, stay silent. A firmware device parked on a long MCTP
        // poll is indistinguishable from this at the supervisor's end, which
        // is the point.
        if answered < SILENT_AFTER_REQUESTS && channel.service_once() {
            answered += 1;
            // Only the interesting transitions are logged. A console write
            // under QEMU costs more than a round-trip, so logging every
            // request would pace the test rather than observe it.
            if answered == LATCH_AFTER_REQUESTS {
                channel.latch(Pending::UpdateRequested);
                pw_log::info!("PLDM: latched UpdateRequested, peer nudged");
            }
            if answered == SILENT_AFTER_REQUESTS {
                pw_log::info!("PLDM: going silent");
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    #[expect(clippy::empty_loop)]
    loop {}
}
