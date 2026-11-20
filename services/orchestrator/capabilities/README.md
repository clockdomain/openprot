<!-- Licensed under the Apache-2.0 license -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# orchestrator-capabilities

Device-facing capability traits for the Boot Orchestrator: `BootControl`
(actuation — hold/release reset), `BootMonitor` (observation — read boot
liveness) and `FlashGuard` (the rules a SPI filter enforces on a device's
flash, and who owns the bus to it; see
`docs/src/design/orchestrator/orchestrator-platform.md`). A dependency-free leaf;
adapters implement them elsewhere.
