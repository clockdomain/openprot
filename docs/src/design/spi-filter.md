# SPI flash filter

A SPI flash filter is a policy enforcement point on a flash bus. It sits
between a host (BMC, PCH, application processor) and the host's flash and
decides which transactions get through. This page proposes the HAL contract
for such filters, `hal/blocking/spi_filter/`, and maps it onto the four
chips surveyed and onto the AST10x0 driver that already exists in this tree.

This is revision 6 of the design. Revision 5 was a type-checked Rust sketch
derived from data sheets only; this revision folds in what the tree already
has: the flash HAL at `hal/blocking/flash/`, the AST10x0 SPI monitor
peripheral at `target/ast10x0/peripherals/spimonitor/`, and the HAL error
conventions in `hal/blocking/src/`. Where the two disagree, the code in the
tree wins and the change is called out in [Changes from revision 5](#changes-from-revision-5).

The shared values, traits and generic functions on this page are the crate
`hal/blocking/spi_filter/` (`hal_spi_filter`), whose host test exercises the
generic functions against a fake filter. The AST1060 backend below is a
proposal and is not implemented. A new HAL contract is an API change, so the
contract needs an approved `[RFC]` issue before a backend lands; see
[Development Process](../development-process.md).

## Sources

| Source | What was read |
|---|---|
| This tree | `target/ast10x0/peripherals/spimonitor/` (register map, command table, privilege tables, lock bits, violation log), `target/ast10x0/tests/spimonitor/` (boot sequence), `hal/blocking/flash/` (`FlashAddress`, `FlashDriver`) |
| OpenTitan | lowRISC/opentitan `spi_device` registers, 2026-11-20 |
| AST1060 | ASPEED's aspeed-rust and Zephyr SPI monitor drivers, which the tree's driver mirrors |
| CEC173x | Microchip CEC173x-TFLX data sheet DS00005397. Normally configured through Soteria-G3 firmware, so it shapes the traits more than it is a backend anyone would write |
| MCX A577 | NXP `SPI_FILTER` register and field *names* from the Embassy nxp-pac docs. NXP's SVD (mcux-soc-svd, 2026-10-09) does not contain the block, so no bit-level descriptions were available |
| Prior art | Microsoft Project Cerberus `core/spi_filter/spi_filter_interface.h` (commit bdb64b08), a chip-independent C interface whose feature list matches NXP's block closely |

## What the hardware taught us

**AST1060** (four monitors, SPIPF1..4; this is the chip we have a driver for)

- Command *allow-list* of 32 slots. Each slot holds a full command
  descriptor (data width, dummy cycles, address length and width, program
  size), not just an opcode: the filter must parse the command to find the
  address it will check. Slots 0 and 1 are fixed for EN4B/EX4B and slot 31
  for WREAR when present, so general capacity is 29 or 30. A slot is valid,
  valid once, or empty, and can be locked.
- Two address *privilege bitmaps*, read and write, each 512 words covering
  16 KiB blocks up to 256 MiB. A set bit allows. There is no region table
  and no table-full condition; the trade is coarse granularity.
- Violations set three sticky status bits (command, write, read) with
  matching interrupt enables, and are logged to a caller-owned RAM buffer
  one word per entry, carrying the blocked opcode or the blocked address at
  16 KiB granularity.
- Locks are fine-grained: control, interrupt, log base, log control, each
  privilege table, each command slot, plus software-reset and block-FIFO
  lock bits in the control register.
- The external mux that decides who is wired to the flash lives in the SCU
  (SCU0F0), not in the monitor. The board decides which selector means
  "root of trust" and which means "host".
- Mode is two independent knobs: passthrough (single or multi-bit) and
  monitor enable. Zephyr uses passthrough while the mux changes hands.

**OpenTitan** (one `spi_device`)

- One deny bit per opcode, all 256 of them. Modes: disabled, flash
  emulation, passthrough.
- Can answer selected commands itself (status, JEDEC ID, SFDP, mailbox).
- Per-command `CMD_INFO` descriptors, like AST1060's slots.

**CEC173x** (one monitor per host; one or two per package)

- Passive mode observes; active mode intervenes. The reaction is
  configurable: block and continue, reset the flash, reset the host, cut
  flash power.
- Up to 16 regions with separate read, write and erase permissions. A
  prohibited opcode can also be flagged as a violation.
- Separate pre-boot and post-boot rule sets; the host requests the switch.
- Violation status and address are readable. Holds the host in reset and
  isolates the flash at power-up. The data sheet does not say whether rules
  can be locked.

**MCX A577** (one filter block, two ports) and Cerberus

- The filter must know the flash: 3- or 4-byte address mode, and the device
  extent (a maximum-address mask on NXP, a size in Cerberus).
- Two-flash model: one chip select is active and read-only, the other is the
  writable update target and is flagged "dirty" once written.
- Regions are writable *windows* on an otherwise write-protected flash, six
  per port. So region rules need a default, and a chip may have write rules
  only.
- Interrupts for blocked opcode, dirty flash and address-mode change; the
  blocked opcode is readable. Cerberus has a "misaligned address" error.
- Five "programmable opcode" registers whose meaning is unverified, so
  opcode rules are *not* claimed for this chip.

## Six questions, six capabilities

| Question | Capability |
|---|---|
| What does a rule match? | `OpcodeRules`, `RegionRules` |
| How is it enforced? | `SpiFilter` (core) and its `Mode` |
| What happens on a violation? | `ViolationResponse` |
| How do I find out? | `ViolationReport` |
| Who owns the bus, and when may policy change? | `BusIsolation`, `PolicyPhases`, `Lockable` |
| What flash is behind it? | `AddressMode`, `FlashExtent`, `DualFlash` |

| | OpenTitan | AST1060 | CEC173x | MCX A577 |
|---|---|---|---|---|
| `SpiFilter` | yes | yes | yes | yes |
| `OpcodeRules` | 256 opcodes | 32 slots, descriptor per slot | yes | unverified |
| `RegionRules` | - | read/write bitmaps, 16 KiB | read/write/erase, 16 regions | write only, 6 per port |
| `ViolationResponse` | - | not in our driver | 4 responses | - |
| `ViolationReport` | not checked | opcode or address, logged | status + address | yes |
| `BusIsolation` | - | SCU mux | isolate | - |
| `Lockable` | - | yes, fine-grained | not stated | unknown |
| `AddressMode` | yes | implicit in descriptor | not checked | yes |
| `FlashExtent` * | - | 256 MiB fixed | - | yes |
| `DualFlash` * | - | - | - | yes |
| `PolicyPhases` * | - | - | pre/post boot | - |
| `CommandIntercept` * | yes | - | - | - |

\* provisional: one chip shaped it, so expect it to change.

## The typing rule

Where chips disagree, the trait uses an associated type bounded by a
conversion from the small part every chip shares. Portable code builds the
shared value and converts; chip-aware code uses the richer native type.

| Shared | Chip type | Why chips disagree |
|---|---|---|
| `Action` | `SpiFilter::Rule` | pass once (AST1060), block and report (CEC173x) |
| `FlashOp` | `RegionRules::Op` | erase is separate on CEC173x; MCX A577 has write rules only, hence `TryFrom` |
| `Span` | `RegionRules::Region` | alignment (16 KiB on AST1060), size and address limits |
| `FlashAddress` | `FlashExtent::Extent` | NXP wants an address mask |

Where no shared part exists (`Mode`, `Response`, `Violation`, bus
`Connection`, lock `Target`) the type is unbounded and only chip-aware code
sets it.

Only what the SPI flash protocol fixes is concrete, and each such value is a
named type: `Opcode` (one byte), `AddrWidth` (three or four bytes), and the
address. The address type is **`FlashAddress` from `hal_flash_driver`**, not
a new newtype: the filter and the flash driver describe the same device and
one address type lets a verified image offset become a protected region
without conversion.

## Error convention

The blocking HAL's pattern (`gpio_port.rs`, `system_control.rs`) is a chip
error type that maps to a small `#[non_exhaustive]` kind, so generic code
can react to common failures while the backend keeps its detail. The filter
follows it.

```rust
/// Common filter failures. Backends map their own errors onto these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The command table or region table is full, or `set_all(Pass)` was
    /// asked of a chip with a bounded allow-list.
    CapacityExceeded,
    /// The chip cannot parse this command (no descriptor for it).
    UnsupportedCommand,
    /// The region is misaligned, too large, or outside what the filter sees.
    InvalidRegion,
    /// The target register or table is locked.
    Locked,
    /// A read-back after write did not match.
    VerificationFailed,
    /// The operation exists in the trait but not on this chip or in this mode.
    Unsupported,
    /// Anything else the hardware reported.
    Hardware,
}

pub trait Error: core::fmt::Debug {
    fn kind(&self) -> ErrorKind;
}

impl Error for core::convert::Infallible {
    fn kind(&self) -> ErrorKind {
        match *self {}
    }
}

pub trait ErrorType {
    type Error: Error;
}
```

One `Error` type per backend serves every capability trait, including
`Lockable`. Conversion failures (`TryFrom<Span>`, `TryFrom<FlashOp>`) stay
separate because they are decided before the hardware is touched.

## Shared values

```rust
pub use hal_flash_driver::FlashAddress;

/// A SPI flash command byte. Fixed by the protocol, so concrete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode(pub u8);

/// The two outcomes every chip has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Pass,
    Block,
}

/// The two kinds of flash access every region-capable chip distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashOp {
    Read,
    Write,
}

/// How many address bytes a flash command carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrWidth {
    Three,
    Four,
}

/// Whether the writable (inactive) flash has been written since last cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashState {
    Clean,
    Dirty,
}

/// An inclusive address range. Inclusive so that a whole 4 GiB device is
/// expressible and no start-plus-length sum can overflow. Chips convert it
/// into their own checked `Region`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    first: FlashAddress,
    last: FlashAddress,
}

impl Span {
    /// `None` if `last` precedes `first`.
    pub fn new(first: FlashAddress, last: FlashAddress) -> Option<Self> {
        if last.offset() < first.offset() {
            return None;
        }
        Some(Self { first, last })
    }

    pub fn first(&self) -> FlashAddress {
        self.first
    }

    pub fn last(&self) -> FlashAddress {
        self.last
    }
}
```

`Span` validates on construction so every `TryFrom<Span>` below can assume
`first <= last` and check only its chip's alignment and limits.

## Core and capability traits

```rust
pub trait SpiFilter: ErrorType {
    /// How the chip enforces policy (off, passthrough, passive, active...).
    /// No shared part, so only chip-aware code picks a mode.
    type Mode: Copy;
    /// What the chip can do with a matching command.
    type Rule: Copy + From<Action>;

    fn set_mode(&mut self, mode: Self::Mode) -> Result<(), Self::Error>;
    fn mode(&self) -> Result<Self::Mode, Self::Error>;
}

/// Rules that match on the command byte.
pub trait OpcodeRules: SpiFilter {
    /// How many opcodes can pass at once. `None` = all 256. A chip with
    /// reserved slots reports the table size; the usable count is lower.
    const PASS_CAPACITY: Option<usize>;
    /// Give every opcode the same outcome. `Pass` fails with
    /// `CapacityExceeded` where the allow-list is bounded.
    fn set_all(&mut self, action: Action) -> Result<(), Self::Error>;
    /// A chip that must parse the command fails with `UnsupportedCommand`
    /// for an opcode it has no descriptor for. Adding a described command
    /// is an inherent, chip-specific operation.
    fn set_rule(&mut self, opcode: Opcode, rule: Self::Rule) -> Result<(), Self::Error>;
    fn rule(&self, opcode: Opcode) -> Result<Self::Rule, Self::Error>;
}

/// Rules that match on the flash address and the kind of access.
pub trait RegionRules: SpiFilter {
    /// `TryFrom`, because a chip may have rules for only some operations.
    type Op: Copy + TryFrom<FlashOp>;
    type Region: Copy + TryFrom<Span>;
    /// `Some(n)`: a table of `n` regions. `None`: a bitmap over the
    /// address space, so any number of regions at the chip's granularity.
    const REGION_CAPACITY: Option<usize>;
    /// Outcome for addresses no region covers. Call it before adding
    /// regions: on a bitmap chip it rewrites the whole table.
    fn set_default(&mut self, op: Self::Op, action: Action) -> Result<(), Self::Error>;
    /// Regions carry only pass or block. Richer per-region behaviour
    /// (report, pass once) is not a region property on any surveyed chip.
    fn set_region_rule(
        &mut self,
        region: Self::Region,
        op: Self::Op,
        action: Action,
    ) -> Result<(), Self::Error>;
}

/// Choose what the chip does when a rule is broken.
pub trait ViolationResponse: SpiFilter {
    type Response: Copy;
    fn set_response(&mut self, response: Self::Response) -> Result<(), Self::Error>;
}

/// Read back what was blocked.
pub trait ViolationReport: SpiFilter {
    type Violation;
    /// Returns and consumes the oldest pending violation, if any. A chip
    /// whose log has no hardware read pointer keeps the cursor in the
    /// driver.
    fn take_violation(&mut self) -> Result<Option<Self::Violation>, Self::Error>;
}

/// Control who is electrically connected to the flash. What a connection
/// means (host, root of trust, nobody) is board wiring, so the type is the
/// chip's and the board gives it meaning.
pub trait BusIsolation: SpiFilter {
    type Connection: Copy;
    fn connect(&mut self, connection: Self::Connection) -> Result<(), Self::Error>;
    fn connection(&self) -> Result<Self::Connection, Self::Error>;
}

/// Freeze part of the configuration until reset. Which parts can be frozen
/// separately is the chip's business.
pub trait Lockable: ErrorType {
    type Target: Copy;
    fn lock(&mut self, target: Self::Target) -> Result<(), Self::Error>;
    fn is_locked(&self, target: Self::Target) -> Result<bool, Self::Error>;
}

/// The filter must know how many address bytes commands carry, or it cannot
/// find the address in a transaction.
pub trait AddressMode: SpiFilter {
    fn set_addr_width(&mut self, width: AddrWidth) -> Result<(), Self::Error>;
    fn addr_width(&self) -> Result<AddrWidth, Self::Error>;
}

/// Provisional (MCX A577, Cerberus). Tell the filter how large the flash is,
/// as its last valid address.
pub trait FlashExtent: SpiFilter {
    type Extent: Copy + TryFrom<FlashAddress>;
    fn set_extent(&mut self, extent: Self::Extent) -> Result<(), Self::Error>;
}

/// Provisional (MCX A577, Cerberus). Two flashes behind one filter: the
/// active one is read-only, the other is the writable update target.
pub trait DualFlash: SpiFilter {
    type ChipSelect: Copy;
    fn set_active(&mut self, chip_select: Self::ChipSelect) -> Result<(), Self::Error>;
    fn inactive_state(&self) -> Result<FlashState, Self::Error>;
    fn clear_inactive_state(&mut self) -> Result<(), Self::Error>;
}

/// Provisional (CEC173x). Separate rule sets per boot phase; the host
/// itself requests the switch to post-boot.
pub trait PolicyPhases: SpiFilter {
    type Phase: Copy;
    fn phase(&self) -> Result<Self::Phase, Self::Error>;
}

/// Provisional (OpenTitan). The chip answers selected commands itself
/// instead of forwarding them.
pub trait CommandIntercept: SpiFilter {
    type Command: Copy;
    fn set_intercept(&mut self, command: Self::Command, enabled: bool) -> Result<(), Self::Error>;
}
```

`Lockable` is defined in this crate because nothing in `hal/` provides it
today. It depends only on `ErrorType`, so it can move to a shared module when
a second peripheral needs it. The orchestrator's `LockdownLatch` is a
different thing: it latches the *platform* into a safe state, and would be
implemented on top of this (lock every target, isolate the bus), not
replaced by it.

## Generic code

These compile for every chip with the named capabilities and nothing else,
and are what boot code and the orchestrator adapters should call.

```rust
/// Allow exactly these commands.
pub fn allow_only<F: OpcodeRules>(filter: &mut F, opcodes: &[Opcode]) -> Result<(), F::Error> {
    filter.set_all(Action::Block)?;
    for &opcode in opcodes {
        filter.set_rule(opcode, Action::Pass.into())?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum RegionError<E, R, O> {
    /// The chip cannot express this address range.
    BadRegion(R),
    /// The chip has no rules for this kind of access.
    BadOp(O),
    Filter(E),
}

/// Shorthand for the error of a portable region function on filter `F`.
pub type RegionResult<F> = Result<
    (),
    RegionError<
        <F as ErrorType>::Error,
        <<F as RegionRules>::Region as TryFrom<Span>>::Error,
        <<F as RegionRules>::Op as TryFrom<FlashOp>>::Error,
    >,
>;

/// Block writes to one range.
pub fn write_protect<F: RegionRules>(filter: &mut F, span: Span) -> RegionResult<F> {
    let region = F::Region::try_from(span).map_err(RegionError::BadRegion)?;
    let write = F::Op::try_from(FlashOp::Write).map_err(RegionError::BadOp)?;
    filter
        .set_region_rule(region, write, Action::Block)
        .map_err(RegionError::Filter)
}

/// Block writes everywhere except the given ranges.
pub fn writable_only<F: RegionRules>(filter: &mut F, spans: &[Span]) -> RegionResult<F> {
    let write = F::Op::try_from(FlashOp::Write).map_err(RegionError::BadOp)?;
    filter
        .set_default(write, Action::Block)
        .map_err(RegionError::Filter)?;
    for &span in spans {
        let region = F::Region::try_from(span).map_err(RegionError::BadRegion)?;
        filter
            .set_region_rule(region, write, Action::Pass)
            .map_err(RegionError::Filter)?;
    }
    Ok(())
}

pub fn drain_violations<F: ViolationReport>(
    filter: &mut F,
    mut handle: impl FnMut(F::Violation),
) -> Result<(), F::Error> {
    while let Some(violation) = filter.take_violation()? {
        handle(violation);
    }
    Ok(())
}

#[derive(Debug)]
pub enum LockDownError<E, R, O> {
    Opcodes(E),
    Region(RegionError<E, R, O>),
    Lock(E),
}

/// Shorthand for the error of `lock_down` on filter `F`.
pub type LockDownResult<F> = Result<
    (),
    LockDownError<
        <F as ErrorType>::Error,
        <<F as RegionRules>::Region as TryFrom<Span>>::Error,
        <<F as RegionRules>::Op as TryFrom<FlashOp>>::Error,
    >,
>;

/// Restrict commands, write-protect a range, then freeze the configuration.
pub fn lock_down<F>(
    filter: &mut F,
    opcodes: &[Opcode],
    protected: Span,
    locks: &[F::Target],
) -> LockDownResult<F>
where
    F: OpcodeRules + RegionRules + Lockable,
{
    allow_only(filter, opcodes).map_err(LockDownError::Opcodes)?;
    write_protect(filter, protected).map_err(LockDownError::Region)?;
    for &target in locks {
        filter.lock(target).map_err(LockDownError::Lock)?;
    }
    Ok(())
}
```

## AST1060 backend

This is the backend that will be written first, as
`target/ast10x0/backend/spi_filter/`, wrapping the existing
`ast10x0_peripherals::spimonitor` the way `target/ast10x0/backend/flash/`
wraps the SMC. The peripheral keeps its register knowledge; the backend only
maps types.

```rust
use ast10x0_peripherals::spimonitor::SpiMonitorError;
use hal_spi_filter::{Action, ErrorKind, FlashAddress, FlashOp, Opcode, Span};

/// SPIPF000 bits 0..2. The hardware has two independent knobs (passthrough
/// width, monitor enable); the backend exposes the three combinations the
/// Zephyr sequence uses and treats the rest as `Unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Disabled,
    /// Traffic bypasses the filter. Used while the mux changes hands.
    Passthrough(PassthroughWidth),
    /// Programmed policy is enforced.
    Filtering,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassthroughWidth {
    Single,
    Multi,
}

/// A command slot can be valid, or valid for one use only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Pass,
    PassOnce,
    Block,
}

impl From<Action> for Rule {
    fn from(action: Action) -> Self {
        match action {
            Action::Pass => Rule::Pass,
            Action::Block => Rule::Block,
        }
    }
}

/// Privilege bitmap granularity and reach.
pub const BLOCK_SIZE: u32 = 16 * 1024;
pub const ADDRESS_LIMIT: u32 = 256 * 1024 * 1024;

/// A run of whole 16 KiB blocks below 256 MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    first_block: u16,
    block_count: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadRegion {
    Misaligned,
    BeyondReach,
}

impl TryFrom<Span> for Region {
    type Error = BadRegion;
    fn try_from(span: Span) -> Result<Self, BadRegion> {
        let first = span.first().offset();
        let last = span.last().offset();
        if first % BLOCK_SIZE != 0 || last % BLOCK_SIZE != BLOCK_SIZE - 1 {
            return Err(BadRegion::Misaligned);
        }
        if last >= ADDRESS_LIMIT {
            return Err(BadRegion::BeyondReach);
        }
        let first_block = first / BLOCK_SIZE;
        let block_count = (last - first) / BLOCK_SIZE + 1;
        Ok(Region {
            first_block: first_block as u16,
            block_count: block_count as u16,
        })
    }
}

/// Decoded log entry. Addresses are 16 KiB-granular, as the hardware logs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    BlockedCommand(Opcode),
    BlockedRead(FlashAddress),
    BlockedWrite(FlashAddress),
    Invalid(u32),
}

/// SCU0F0 external mux selector. `ast10x0_board` maps `Sel0`/`Sel1` onto
/// root-of-trust and host control, including any polarity inversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Sel0,
    Sel1,
}

/// One per lock bit the hardware has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Control,
    Interrupts,
    Log,
    SoftwareReset,
    BlockFifo,
    Command(Opcode),
    PrivilegeTable(FlashOp),
}

/// The backend's error. A newtype, not `SpiMonitorError` itself: the HAL
/// trait and the peripheral type are both foreign to the backend crate, so
/// Rust's orphan rule forbids implementing one for the other. The flash
/// backend maps `SmcError` the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub SpiMonitorError);

impl From<SpiMonitorError> for Error {
    fn from(e: SpiMonitorError) -> Self {
        Error(e)
    }
}

impl hal_spi_filter::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self.0 {
            SpiMonitorError::NoCommandSlot => ErrorKind::CapacityExceeded,
            SpiMonitorError::UnsupportedCommand(_) => ErrorKind::UnsupportedCommand,
            SpiMonitorError::InvalidRegion
            | SpiMonitorError::InvalidAddress
            | SpiMonitorError::InvalidLength => ErrorKind::InvalidRegion,
            SpiMonitorError::Locked => ErrorKind::Locked,
            SpiMonitorError::VerificationFailed | SpiMonitorError::LockFailed => {
                ErrorKind::VerificationFailed
            }
            _ => ErrorKind::Hardware,
        }
    }
}
```

The handle is the peripheral's existing typestate, `SpiMonitor<Configured>`
and `SpiMonitor<Locked>`, plus a read cursor for the violation log. The two
states implement different trait sets, which is the point of the typestate:

| Trait | `Configured` | `Locked` |
|---|---|---|
| `SpiFilter` | yes | `set_mode` fails `Locked`; `mode` works |
| `OpcodeRules`, `RegionRules`, `Lockable` | yes | no |
| `ViolationReport`, `BusIsolation` | yes | yes |

The consuming `lock() -> SpiMonitor<Locked>` stays inherent: it sets every
lock bit at once and is what boot code calls. `Lockable::lock(target)` is
the fine-grained form for generic code and for the policies that keep one
table open after boot.

How the generic calls land on the peripheral:

| Trait call | Peripheral call |
|---|---|
| `set_rule(op, Pass)` / `PassOnce` | `add_command(op, false)` / `add_command(op, true)`; `UnsupportedCommand` if there is no descriptor |
| `set_rule(op, Block)` | `remove_command(op)` |
| `set_all(Block)` | clear every unlocked slot; `Locked` if any slot is locked |
| `set_all(Pass)` | `CapacityExceeded` |
| `set_default(op, action)` | fill the read or write bitmap with ones or zeros |
| `set_region_rule(r, op, action)` | `configure_region(start, len, direction, Enable/Disable)` on whole blocks |
| `take_violation()` | read the next unread log word, decode with `ViolationLogEntry::parse` |
| `connect(sel)` | `set_ext_mux(sel)`, through the SCU unlock discipline |
| `lock(PrivilegeTable(Write))` | `lock_privilege_table(Write)`; `lock(Command(op))` is `lock_command(op)` |

Inherent, not traits: adding a command with a caller-supplied
`CommandDescriptor`, `configure_log(&'static mut [u32])`,
`enable_violation_interrupts`, `acknowledge_violations`,
`reset_filter_state` (pulsed before handing the bus to the host), and
`set_push_pull`.

Not modelled yet: `ViolationResponse`. Revision 5 claimed two block styles
for AST1060 from ASPEED's driver; our driver has no such control, so the
claim is withdrawn until the register is identified.

## The other chips

Only the parts that changed from revision 5 are shown.

**OpenTitan**: `Rule = Action`, `PASS_CAPACITY = None`, `Mode = {Disabled,
Flash, Passthrough}`, `CommandIntercept::Command = {Status, JedecId, Sfdp,
Mailbox}`. The hardware bit means deny, so `set_rule` writes the inverse.
`AddressMode` is backed by `ADDR_MODE`, seen in the register list only.

**CEC173x**: `Rule = {Pass, Block, BlockAndReport}` for opcodes;
`Op = {Read, Write, Erase}` with `TryFrom<FlashOp>` (infallible in
practice, but spelled `TryFrom` so all backends look alike);
`REGION_CAPACITY = Some(16)`; `Response = {BlockOnly, ResetFlash,
ResetHost, KillPower}`; `Violation { status, address }` where `status`
stays opaque until the bit layout is checked; `Connection = {Isolated,
Connected}`; `Phase = {PreBoot, PostBoot}`. No `Lockable`: the data sheet
does not say rules can be locked.

**MCX A577**: `Rule = Action`; `Op = {Write}` so `TryFrom<FlashOp>` rejects
`Read`; `REGION_CAPACITY = Some(6)` per port; `Mode = {Disabled, Dual,
Single(ChipSelect)}`; `Extent` must be an address mask (`last & (last+1) ==
0`); `Violation = {BlockedOpcode(Opcode), InactiveFlashDirty,
AddressModeChanged, F8Command}`. No `OpcodeRules` until the programmable
opcode registers are understood.

## The boot sequence through the traits

The AST10x0 tests already perform a Hold, Configure, Release, Monitor
sequence against the raw peripheral (`target/ast10x0/tests/spimonitor/`,
with `BootPhase` in `spimonitor/types.rs`). Through the traits it reads:

1. **Hold.** `connect(rot)` so the root of trust owns the flash; the host is
   held in reset by the orchestrator's `BootControl`. The root of trust
   verifies the image through the flash HAL on the SMC.
2. **Configure.** `set_mode(Passthrough)` while nothing depends on the
   filter, then `allow_only`, `writable_only` or `write_protect` with spans
   derived from the verified image layout, then `set_mode(Filtering)`.
3. **Lock.** What gets locked is a board policy, not a fixed step; see
   [Flash guarding](./orchestrator/orchestrator-platform.md#flash-guarding)
   on the orchestrator platform page.
   Locking only the command table is `Lockable::lock(target)`; locking
   everything is `lock_down` or the inherent consuming `lock()`. All of
   them read back every lock bit and fail `VerificationFailed` if one did
   not stick.
4. **Release.** Inherent `reset_filter_state`, then `connect(host)`, then
   `BootControl::release`.
5. **Monitor.** `drain_violations` from the interrupt or a poll; the
   orchestrator turns a violation into an `Event`.

Steps 1, 4 and 5 use only `BusIsolation` and `ViolationReport`, which is
exactly the trait set the `Locked` typestate keeps. That is the check that
the lock boundary is drawn in the right place.

## Crate layout and migration

- `hal/blocking/spi_filter/` is a `rust_library` named `spi_filter` with
  `crate_name = "hal_spi_filter"`, `edition = "2024"`, depending on
  `//hal/blocking/flash:driver` for `FlashAddress`. It follows the flash
  HAL's shape of a separate directory rather than a module of
  `openprot_hal_blocking`, because it has a dependency. Its `rust_test`
  holds a fake filter with a bounded allow-list, a four-entry region table,
  a violation queue and two lock targets, and runs the generic functions
  against it.
- `target/ast10x0/backend/spi_filter/` implements the traits over
  `ast10x0_peripherals::spimonitor`, with `target_compatible_with =
  TARGET_COMPATIBLE_WITH`. The QEMU tests under `target/ast10x0/tests/`
  move from raw peripheral calls to the generic functions one at a time.
- `SpiMonitorControl` in `spimonitor/traits.rs`, which takes an instance id
  on every call, is retired once the backend exists; its `BootError`,
  `BootPhase` and `BootConfig` move with the boot sequence into a generic
  helper over the traits.
- The flash HAL gains `PartialOrd, Ord` on `FlashAddress`. The design works
  without it, but span arithmetic reads better with it.
- The orchestrator adapter under `services/orchestrator/adapters/hal`, the
  `FlashGuard` capability it implements, and the lock and violation
  policies are described under
  [Flash guarding](./orchestrator/orchestrator-platform.md#flash-guarding)
  on the orchestrator platform page. How ASPEED's PFR firmware drives the
  same AST1060 block is recorded in
  `target/ast10x0/peripherals/spimonitor/README.md`.

## Open questions

- AST1060: which register selects the block style (early chip-select
  deassert versus blocked clocks) that ASPEED's driver exposes, if any.
- AST1060: whether passthrough and monitor-enable are ever both set in a
  supported configuration, or whether the three-valued `Mode` is complete.
- AST1060: whether a region whose span is not 16 KiB-aligned should be
  rejected (this design) or rounded outward (what `configure_region` does
  today, which widens an *allow* as well as a *deny*).
- AST1060: what the software reset pulse ASPEED issues on every release
  actually clears. If only the log pointer and block FIFO, the orchestrator
  can skip it and lock the reset control bit; if privilege state, the pulse
  stays and the bit stays unlocked. An EVB experiment, listed in the
  peripheral README.
- Dual flash. ASPEED's BMC can span two chips, with a region past the first
  chip's size rebased onto the second monitor. Every guard is per filter, so
  a dual-flash device is two guard entries with the spans split by the table
  or by the manifest reader. Nothing in the tree needs this yet.
- CEC173x: the violation status bit layout, and whether rules can be locked.
- MCX A577: everything at bit level; the SVD does not have the block yet.
- Whether `Lockable` belongs in a shared HAL module from the start.

## Changes from revision 5

- The address type is `hal_flash_driver::FlashAddress`, not a new
  `FlashAddr`.
- Errors follow the blocking HAL's `Error`/`ErrorKind` convention; one error
  type per backend serves every trait, including `Lockable`, which is now
  defined here rather than imported from a module that did not exist.
- `Span` validates on construction and hides its fields.
- `RegionRules::MAX_REGIONS: usize` became `REGION_CAPACITY: Option<usize>`:
  the AST1060 tables are bitmaps with no region count. The `16` in
  `policy.rs` is a policy-struct size, not a hardware limit, and the `32`
  claimed in revision 5 was wrong.
- Region rules take `Action`, not `Self::Rule`: no surveyed chip has
  pass-once or report-per-region.
- AST1060 `Mode` gained the filtering state; `Violation` carries the opcode
  or address, matching `ViolationLogEntry`; `Lock` lists every hardware
  lock bit; `Region` requires 16 KiB alignment and rejects addresses at or
  above 256 MiB; `ViolationResponse` is withdrawn pending a register.
- `SpiFilter` and `BusIsolation` gained read-back methods.
- `lock_down`'s error names the opcode step separately instead of filing it
  under `RegionError::Filter`.
- `cec173x::Op` uses `TryFrom<FlashOp>` like the other chips.
- "Five questions" became six, and "all three chips" became the table above.
- The shared values, traits and generic functions are compiled, tested and
  clippy-clean as `hal_spi_filter`; the chip sections are sketches. The
  compiler caught one thing revision 5 missed: a backend cannot implement
  the HAL `Error` trait for the peripheral's error type (orphan rule), so
  the AST1060 backend wraps `SpiMonitorError` in a newtype.
- `LockDownResult<F>` names `lock_down`'s return type, as `RegionResult<F>`
  does for the region helpers; clippy's `type_complexity` asked for it.
