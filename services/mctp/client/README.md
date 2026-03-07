# openprot-mctp-client

IPC client for the MCTP server, used by userspace applications under Pigweed (`pw_kernel`).

## Overview

This crate provides `IpcMctpClient`, which implements the `MctpClient` trait from `openprot-mctp-api`. It communicates with the MCTP server process over a Pigweed IPC channel using the binary wire protocol defined in `openprot-mctp-api`.

Applications (such as the MCTP echo task) use this crate to interact with the MCTP server without needing to know about transport details.

## Key Types

- `IpcMctpClient` — implements `MctpClient` via Pigweed IPC; uses `RefCell` for interior mutability so trait methods taking `&self` can mutate internal IPC buffers

## Status

The `send_recv` method is currently stubbed, pending Phase 6 integration with `syscall::channel_transact`.

## Dependencies

- `openprot-mctp-api` — `MctpClient` trait and wire protocol
