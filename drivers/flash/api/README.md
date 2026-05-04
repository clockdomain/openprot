# flash_api

Shared types and backend trait for the OpenPRoT flash driver. This
crate is what the flash client (the userspace task that sends
requests) and the flash server (the platform task that handles them)
both depend on.

Bazel target: `//drivers/flash/api:flash_api`

## What's in here

- the byte layout of flash requests and responses on the IPC channel
- the list of operations (Read, Write, Erase, GetGeometry, …) and
  their numeric codes
- the data types used for discovery (`FlashGeometry`, `FlashRegion`)
- the error code table
- the `FlashBackend` trait that platform code implements

No I/O, no syscalls, no platform code. Just types and a trait.

## Layer position

```
Application task
      │
      ▼
FlashClient  ─────────►  flash_api  ◄───────── FlashServer
                       (shared types,
                        backend trait)
                              │
                              ▼
                       PlatformFlashBackend
                              │
                              ▼
                          SMC / FMC
```

## Glossary

A few terms are used throughout this crate, the client, and the server:

**Backend** — the platform code that actually talks to the flash
chip's controller. It implements the `FlashBackend` trait. There is
one backend per physical controller (e.g. `Ast10x0FlashBackend` for
the AST10x0 SMC/FMC). The wire protocol shuttles requests *to* the
backend; the backend is what makes a `Read` or an `Erase` actually do
anything.

**Geometry** — what shape a flash chip has, returned by
`FlashGeometry`: total capacity, write-page size, which erase sizes
the part supports (4 KiB sector, 32 KiB block, 64 KiB block, …),
smallest required alignment, addressing mode (3- or 4-byte), and a
few capability bits. One record per chip, set by the backend at
compile time and reported to clients via `GetGeometry`. Lets a tool
that needs to run on multiple boards stop hard-coding the chip type.

**Region** — a named sub-range of a flash chip, returned by
`FlashRegion`: a base offset, a length, a logical handle
(`route_key`), and a few flag bits. A 64 MiB BMC flash usually
exposes itself as one whole-chip region; an OpenPRoT-internal flash
typically exposes four (active firmware / recovery / runtime state /
AFM). Clients ask for them via `GetRegions`.

**Route key** — a `u32` that names a flash target — either the whole
device this channel is bound to, or a region within it. Stored inside
`FlashRegion` so a region can be referred to on its own when handed
off to another service.

**Capability flag** — a bit in `GeometryFlags` (per chip) or
`RegionAttrs` (per region) that says "this can do X." For example,
`HASH_ELIGIBLE` means a server-side hash consumer can read directly
from this device without sending the bytes through the client.

## Wire protocol

### Frame layout

A request is a `FlashRequestHeader` (16 bytes, little-endian, packed)
followed by an operation-specific payload up to `MAX_PAYLOAD_SIZE`
(256 bytes). A response is a `FlashResponseHeader` (8 bytes,
little-endian, packed) followed by an operation-specific payload up
to `MAX_PAYLOAD_SIZE`.

```rust
#[repr(C, packed)]
pub struct FlashRequestHeader {
    pub op_code: u8,
    pub flags: u8,
    pub payload_len: u16,
    pub address: u32,
    pub length: u32,
    pub reserved: u32,
}                                  // = 16 bytes

#[repr(C, packed)]
pub struct FlashResponseHeader {
    pub status: u8,                // 0 = Success; otherwise FlashError
    pub reserved: u8,
    pub payload_len: u16,
    pub value: u32,                // op-specific (capacity, byte count, ...)
}                                  // = 8 bytes
```

Both headers come with `new` / `success` / `error` builder functions
and accessor methods (`address_value()`, `length_value()`,
`value_word()`, `payload_length()`, …) that handle the little-endian
conversion, so client and server code doesn't touch raw bytes
directly.

### Operations

| Op | Value | Request | Response |
|---|---|---|---|
| `Exists` | 0x01 | header only | `value` = 0 or 1 |
| `GetCapacity` | 0x02 | header only | `value` = total bytes |
| `Read` | 0x03 | header (`address`, `length`) | `value` = byte count, payload = bytes read |
| `Write` | 0x04 | header (`address`, `length`, `payload_len`) + payload | `value` = byte count |
| `Erase` | 0x05 | header (`address`, `length`) | empty |
| `GetGeometry` | 0x06 | header only | payload = `FlashGeometry` (24 B) |
| `GetRegions` | 0x07 | header (`length` = max records) | `value` = count, payload = N × `FlashRegion` (16 B) |

`MAX_PAYLOAD_SIZE` is a fixed protocol constant — it is the same for
every backend, so clients use the constant directly instead of asking
the server.

## Discovery types

### `FlashGeometry` (24 B)

Returned in the `GetGeometry` response payload.

```rust
pub struct FlashGeometry {
    pub capacity: u32,
    pub page_size: u32,           // write granularity (typically 256)
    pub erase_sizes: u32,         // bitmap; bit n set => 1 << n bytes supported
    pub min_erase_align: u32,
    pub address_width: u8,        // 3 or 4
    pub flags: u8,                // GeometryFlags bits
    pub _rsv: [u8; 6],
}
```

`erase_sizes` is a bitmap so a part that supports several granules
can advertise all of them at once. For example, 4 KiB | 32 KiB | 64 KiB
is `(1<<12) | (1<<15) | (1<<16)`. The client picks the largest aligned
size for each block of bytes it wants to erase.

`GeometryFlags` (chip-level capability bits):

| Bit | Name | Meaning |
|---|---|---|
| 0 | `DMA_ELIGIBLE` | Backend can copy bytes between two flash regions in one request, without per-chunk round-trips through the client. |
| 1 | `HASH_ELIGIBLE` | A server-side hash consumer can read from this chip directly, without sending the bytes through the client. |

### `FlashRegion` (16 B)

Returned in the `GetRegions` response payload — one entry per region
the device exposes.

```rust
pub struct FlashRegion {
    pub route_key: u32,           // logical handle naming this region
    pub base: u32,
    pub length: u32,
    pub attrs: u32,               // RegionAttrs bits
}
```

A backend with no sub-regions returns a single entry with
`RegionAttrs::WHOLE_CHIP` set, spanning `[0, capacity)`.

`RegionAttrs` (per-region flags):

| Bit | Name | Meaning |
|---|---|---|
| 0 | `FILTER_PROTECTED` | Server enforces an access policy over this region (the actual mechanism is platform-specific). |
| 1 | `HASH_ELIGIBLE`    | A server-side hash consumer can read this region directly. |
| 2 | `READ_ONLY`        | Server rejects `Write` and `Erase` against this region. |
| 3 | `WHOLE_CHIP`       | Region covers the whole physical chip. |

## Backend trait

```rust
pub trait FlashBackend {
    type RouteKey: Copy;

    fn info(&self, key: Self::RouteKey) -> FlashInfo;

    fn geometry(&self, key: Self::RouteKey)
        -> Result<FlashGeometry, BackendError>;     // default derives from info()

    fn regions(&self, key: Self::RouteKey, out: &mut [FlashRegion])
        -> Result<usize, BackendError>;             // default = 1 whole-chip entry

    fn exists(&mut self, key: Self::RouteKey)
        -> Result<bool, BackendError>;              // default Ok(true)

    fn read (&mut self, key: Self::RouteKey, address: u32, out:  &mut [u8])
        -> Result<usize, BackendError>;
    fn write(&mut self, key: Self::RouteKey, address: u32, data: &[u8])
        -> Result<usize, BackendError>;
    fn erase(&mut self, key: Self::RouteKey, address: u32, length: u32)
        -> Result<(),    BackendError>;

    fn enable_interrupts (&mut self) -> Result<(), BackendError>;
    fn disable_interrupts(&mut self) -> Result<(), BackendError>;
}
```

`info`, `geometry`, and `regions` take `&self` because they only
report values the backend knows ahead of time — no exclusive access
is needed. `geometry` and `regions` ship default implementations so a
backend with one erase granule and no sub-regions doesn't have to
write boilerplate.

`RouteKey` is an associated type. A backend with one chip-select sets
it to `()`; a backend that drives a multi-chip-select controller sets
it to a chip-select index. The wire header does not carry routing
information — each `FlashClient` is tied to one chip-select via its
IPC handle, and the server picks the right backend (and route key)
based on which channel the request arrived on. When a piece of
routing data really has to travel across a service boundary (for
example the `route_key` field inside `FlashRegion`), it goes inside
the relevant struct.

## Errors

`FlashError` is the wire status code carried in
`FlashResponseHeader::status`:

| Variant | Code | Meaning |
|---|---|---|
| `Success` | 0x00 | OK |
| `InvalidOperation` | 0x01 | Unknown opcode |
| `InvalidAddress` | 0x02 | Address out of range |
| `InvalidLength` | 0x03 | Length zero, overflow, or misaligned |
| `BufferTooSmall` | 0x04 | Server-side buffer constraint |
| `Busy` | 0x05 | Backend busy |
| `Timeout` | 0x06 | Operation timed out |
| `WouldBlock` | 0x07 | Could not complete synchronously; retry after IRQ |
| `IoError` | 0x08 | Media-level failure |
| `NotPermitted` | 0x09 | Write-protected or restricted region |
| `InternalError` | 0xFF | Unclassified server fault |

`BackendError` is the trait-level error type backends return. An
`impl From<BackendError> for FlashError` gives the server a single
mapping when it encodes a response.

## Tests

Host-side unit tests cover each wire type:

- known opcode and error-code values map to the right variant; unknown
  byte values fall back to the documented "unknown" variant.
- request and response headers, `FlashGeometry`, and `FlashRegion`
  round-trip through `new()` → bytes → accessor reads with no loss.
- byte-by-byte little-endian layout matches the documented format.
- header decode rejects buffers that are too short.

```
bazel test //drivers/flash/api:flash_api_test
```

## Constraints

- `no_std` — no heap, no I/O.
- Just types plus one trait. No syscalls, no clocks, no platform code.
- Host-buildable — picked up by the CI `//...` wildcard.

## Dependencies

| Crate | Role |
|---|---|
| `bitflags` | `GeometryFlags`, `RegionAttrs` |
| `zerocopy` | Derives that let the wire structs be safely viewed as bytes and back |
