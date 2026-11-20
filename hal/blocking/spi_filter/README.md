# Blocking SPI flash filter HAL

Traits for the SPI monitors that sit between a host and its flash and decide
which transactions get through. The design, hardware survey and the AST1060
mapping are in [`docs/src/design/spi-filter.md`](../../../docs/src/design/spi-filter.md);
this file is the crate's public seam.

## Seams

- **`SpiFilter`** is the core every backend implements: an error type (via
  `ErrorType`), a chip-specific `Mode`, and a `Rule` constructible from the
  shared `Action` (`Pass` or `Block`).
- **Capability traits** each answer one question and are implemented only
  where the hardware has the feature: `OpcodeRules`, `RegionRules`,
  `ViolationResponse`, `ViolationReport`, `BusIsolation`, `Lockable`,
  `AddressMode`, and the provisional `FlashExtent`, `DualFlash`,
  `PolicyPhases`, `CommandIntercept`.
- **Shared values** are only what the SPI flash protocol fixes: `Opcode`,
  `AddrWidth`, `Span` (an inclusive range, valid by construction), and
  `FlashAddress`, re-exported from `hal_flash_driver` so the filter and the
  flash driver describe the device with one address type.
- **Generic functions** are the portable entry points for boot code:
  `allow_only`, `write_protect`, `writable_only`, `drain_violations`,
  `lock_down`. Each compiles only for a filter with the capabilities it uses.

## Invariants

- Where chips disagree, the trait has an associated type bounded by a
  conversion from the shared value (`Rule: From<Action>`,
  `Op: TryFrom<FlashOp>`, `Region: TryFrom<Span>`,
  `Extent: TryFrom<FlashAddress>`). Conversion is where a backend rejects
  what its hardware cannot express, before any register is touched.
- One backend error type serves every trait, including `Lockable`, and
  reduces to `ErrorKind` so generic code can react without knowing the chip.
- `RegionRules::set_default` must be called before adding regions: on a
  bitmap chip it rewrites the whole table. Region rules carry only `Action`.
- A backend that must parse commands fails `set_rule` with
  `ErrorKind::UnsupportedCommand` for an opcode it has no descriptor for.
  Adding a described command is a chip-specific inherent operation, not a
  trait method.

## Tests

`bazel test //hal/blocking/spi_filter:spi_filter_test` runs the generic
functions against an in-crate fake with a bounded allow-list, a four-entry
region table, a violation queue and two lock targets.
