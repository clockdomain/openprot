# openprot-mctp-api

Platform-independent MCTP types and traits crate.

## Overview

This crate defines the core API contract between MCTP clients and the MCTP server. It provides traits for client operations, listener management, and request/response channels, as well as the binary IPC wire protocol used for inter-process communication.

## Key Types

- `Handle` — opaque handle for listeners, requests, or response channels
- `RecvMetadata` — metadata from a successful receive (msg_type, tag, remote_eid, payload_size, etc.)
- `MctpError` / `ResponseCode` — error types (Success, InternalError, NoSpace, AddrInUse, TimedOut, BadArgument, ServerRestarted)

## Traits

- `MctpClient` — main client interface (req, listener, get/set EID, recv, send, drop_handle)
- `MctpListener` — receiving incoming MCTP messages of a specific type
- `MctpReqChannel` — request/response channels
- `MctpRespChannel` — response channels

## Wire Protocol

The `wire` module implements binary request/response encoding for IPC communication between userspace processes and the MCTP server.

## Dependencies

- `zerocopy` — zero-copy serialization
- `heapless` — `no_std` collections

This crate is `no_std` compatible.
