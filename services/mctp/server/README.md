# openprot-mctp-server

Platform-independent MCTP server core, ported from the Hubris `mctp-server` crate.

## Overview

This crate implements the central MCTP server logic: listener and request handle allocation, inbound message routing, outbound message fragmentation/sending, and timeout management for pending receive calls. It is generic over transport bindings via the `mctp_stack::Sender` trait.

## Key Types

- `Server<S, N>` — the MCTP server, generic over `Sender` (transport) and `N` (config)
- `ServerConfig` — configuration constants (MAX_REQUESTS: 8, MAX_LISTENERS: 8, MAX_OUTSTANDING: 16, MAX_PAYLOAD: 1023)
- `RecvResult` — result from a receive operation

## Modules

- `dispatch` — IPC request dispatcher; decodes wire-protocol requests and calls the corresponding `Server` methods

## Architecture

The server wraps the `mctp-lib` `Router` as its packet processing engine. Inbound packets are fed via `Server::inbound()`, and `Server::update()` drives pending-recv fulfillment. The `dispatch` module bridges IPC wire-protocol messages to server operations.

## Dependencies

- `openprot-mctp-api` — API traits and wire protocol
- `mctp-stack` (`mctp-lib`) — `Router`, `Sender`, fragmentation, serial/I2C handlers
- `mctp` — core MCTP types (`Eid`, `MsgType`, `Tag`)
- `heapless` — `no_std` collections
