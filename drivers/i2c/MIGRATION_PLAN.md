# I2C Service — Migration Plan

Plan to move the existing I2C service from
[`openprot/services/i2c`](../../../openprot/services/i2c/) into
`final-usart/drivers/i2c`, conforming to the layered driver pattern
already established by [`drivers/usart`](../usart/README.md).

The peripheral-level I2C driver (`Ast1060I2c<'a, Y>`) is **already in
place** at [`target/ast10x0/peripherals/i2c`](../../target/ast10x0/peripherals/i2c/) —
imported in a previous step, with `proposed_traits` removed and the
yield closure / raw-pointer constructor pattern matched to UART. This
plan covers everything *above* the peripheral: the IPC service, its
backend bridge, the client facade, and the system image.

## 0. Architectural target

Mirror exactly what `drivers/usart/` does today. Five Bazel targets,
same names, same conventions:

| Layer | usart (today) | i2c (target) |
|---|---|---|
| Wire protocol + backend trait | `//drivers/usart/api:usart_api` | `//drivers/i2c/api:i2c_api` |
| Pure dispatch + reusable runtime | `//drivers/usart/server:usart_server` | `//drivers/i2c/server:i2c_server` |
| Client facade | `//drivers/usart/client:usart_client` | `//drivers/i2c/client:i2c_client` |
| Platform backend (compile-time picked) | `//target/ast10x0/backend/usart:usart_backend_ast10x0` (`crate_name = "usart_backend"`) | `//target/ast10x0/backend/i2c:i2c_backend_ast10x0` (`crate_name = "i2c_backend"`) |
| System image (kernel + apps + config) | `//target/ast10x0/tests/usart:usart` | `//target/ast10x0/tests/i2c:i2c` |

The compile-time backend handle (`use i2c_backend::Backend;` in
`server_main.rs`) makes a future label_flag swap cheap, identical to
the existing usart story.

## 1. Where things live in openprot today (gap analysis)

Files that have to move and reshape:

| openprot path | Lines | Becomes |
|---|---|---|
| [`api/src/lib.rs`](../../../openprot/services/i2c/api/src/lib.rs) | 101 | `drivers/i2c/api/src/lib.rs` (verbatim — keep the existing module split) |
| [`api/src/wire.rs`](../../../openprot/services/i2c/api/src/wire.rs) | 840 | `drivers/i2c/api/src/wire.rs` (verbatim) |
| [`api/src/error.rs`](../../../openprot/services/i2c/api/src/error.rs) | 264 | `drivers/i2c/api/src/error.rs` (verbatim) |
| [`api/src/operation.rs`](../../../openprot/services/i2c/api/src/operation.rs) | 116 | `drivers/i2c/api/src/operation.rs` (verbatim) |
| [`api/src/address.rs`](../../../openprot/services/i2c/api/src/address.rs) | 185 | `drivers/i2c/api/src/address.rs` (verbatim) |
| [`api/src/client.rs`](../../../openprot/services/i2c/api/src/client.rs) | 270 | `drivers/i2c/api/src/client.rs` (verbatim — `I2cClient` trait stays in api) |
| [`api/src/target.rs`](../../../openprot/services/i2c/api/src/target.rs) | 295 | `drivers/i2c/api/src/target.rs` (verbatim — types stay; dispatcher rejects slave ops until Phase 7) |
| [`server/src/main.rs`](../../../openprot/services/i2c/server/src/main.rs) | 417 | split: `drivers/i2c/server/src/lib.rs` (pure `dispatch_request<B>`) + `drivers/i2c/server/src/runtime.rs` (`run<B>`) + `target/ast10x0/tests/i2c/server_main.rs` (binary glue) |
| [`backend-aspeed/src/lib.rs`](../../../openprot/services/i2c/backend-aspeed/src/lib.rs) | 695 | `target/ast10x0/backend/i2c/src/lib.rs` — re-pointed at `ast10x0_peripherals::i2c::Ast1060I2c<Y>` instead of `aspeed_ddk::i2c_core` |
| [`target/ast1060-evb/i2c/`](../../../openprot/target/ast1060-evb/i2c/) | — | `target/ast10x0/tests/i2c/` (system image, system.json5) |

Notable mismatches we'll fix during the move:

1. **Service vs driver naming.** openprot uses `services/i2c/...` and
   `i2c-backend-aspeed`. Final-usart uses `drivers/<name>` and
   `backend-<chip>`. Just rename — no semantic change.
2. **Server is a binary in openprot, a library in final-usart.** The
   pure dispatcher (`dispatch_request<B>`) becomes a library so the
   binary glue can live next to its system image. Same split usart did.
3. **Backend has no trait in openprot.** It's just a struct with 18
   public methods. Final-usart's pattern requires a trait so the
   server binary is portable across backends. We'll define
   `I2cBackend` and have `Ast1060I2cBackend` implement it. Same shape
   as `UsartBackend`/`Ast10x0UsartBackend`.
4. **Backend depends on `aspeed-ddk` (out-of-tree).** That dependency
   moves to the in-tree peripheral driver we just imported
   ([target/ast10x0/peripherals/i2c](../../target/ast10x0/peripherals/i2c/)). One
   fewer external crate.
5. **Yield closure threading.** The new `Ast1060I2c<'a, Y>` takes a
   yield closure. Backend has to pick one. See §7.
6. **Slave/target mode is half-wired in openprot** (server's IRQ path
   is commented out). We're not going to land it in this migration —
   it goes into Phase 7.

The api crate's internal module split (address / client / error /
operation / target / wire) is **left untouched**. usart's two-file
shape is right for usart's smaller surface; i2c's surface is bigger
and the existing decomposition reads cleanly. Same goes for
`I2cClient` living in `api/src/client.rs` — no reason to move it out.

## 2. Operation set

Port openprot's full operation set as-is — controller and slave both.
The openprot backend already implements all 15 ops; the dispatcher
just routes to them.

| Op | Code | Backend method | Notes |
|---|---|---|---|
| `Write` | 0x01 | `write(bus, addr, data)` | Controller |
| `Read` | 0x02 | `read(bus, addr, buf)` | Controller |
| `WriteRead` | 0x03 | `write_read(bus, addr, w, r)` | Combined transaction |
| `Probe` | 0x04 | `probe(bus, addr)` | Controller |
| `ConfigureSpeed` | 0x05 | `configure_speed(bus, speed)` | Controller |
| `RecoverBus` | 0x06 | `recover_bus(bus)` | Controller |
| `ConfigureSlave` | 0x07 | `configure_slave(bus, addr)` | Slave |
| `EnableSlave` | 0x08 | `enable_slave(bus)` | Slave |
| `DisableSlave` | 0x09 | `disable_slave(bus)` | Slave |
| `SlaveReceive` | 0x0A | `slave_receive(bus, buf)` | Slave (poll) |
| `SlaveWaitEvent` | 0x0B | `slave_wait_event(bus, …)` | Slave (poll-blocking) |
| `SlaveSetResponse` | 0x0C | `slave_set_response(bus, data)` | Slave |
| `EnableSlaveNotification` | 0x0D | `enable_slave_notification(bus)` | Slave (state flag) |
| `DisableSlaveNotification` | 0x0E | `disable_slave_notification(bus)` | Slave (state flag) |

What stays **out of Phase 1-6** is the IRQ-driven half of the slave
notification path: the `handle_i2c_interrupt` function and the
`raise_peer_user_signal` wake-up from openprot's
[`server/src/main.rs:90-136`](../../../openprot/services/i2c/server/src/main.rs#L90)
that's currently `//`-commented. Ops still work via polling
(`SlaveReceive`, `SlaveWaitEvent`); Phase 7 wires the wait-group's
IRQ branch so clients can block on a notification instead.

## 3. Phased execution

Six PR-sized phases, build green between each.

### Phase 1 — `drivers/i2c/api` (lift & shift)

- Copy the api crate's six `.rs` files (`address`, `client`, `error`,
  `operation`, `target`, `wire`) and `lib.rs` from openprot
  unchanged. Keep the existing module split.
- Author a `BUILD.bazel` from the openprot one with the dep labels
  re-pointed at final-usart's registry (`@rust_crates//:...`).
- Author a `Cargo.toml` mirroring the openprot one (same deps,
  `edition = "2024"`).
- No backend traits yet — those land in Phase 2 inside `i2c_server`.

**Checkpoint:** `bazelisk build //drivers/i2c/api:i2c_api` clean.

### Phase 2 — `drivers/i2c/server` (dispatch + runtime)

- `server/src/lib.rs`: define `pub trait I2cBackend` (parallel to
  `UsartBackend`) covering all 15 ops openprot's `AspeedI2cBackend`
  exposes today + a `BackendError` enum + `From<BackendError> for ResponseCode`.
- Implement `dispatch_request<B: I2cBackend>(backend, req, resp) -> usize`
  with one arm per op. Lift the body of openprot's
  [`dispatch_i2c_op`](../../../openprot/services/i2c/server/src/main.rs#L147) almost
  verbatim — it's already structured this way.
- `server/src/runtime.rs`: copy the usart pattern — `run<B>(backend, wg, irq, irq_signals)`. The IRQ branch acks (Phase 7 fills in the
  drain-and-notify body); the channel branch reads → dispatches →
  responds. Same wait-group topology.

**Checkpoint:** `bazelisk build //drivers/i2c/server:i2c_server`
clean. No backend impls yet — only the trait surface.

### Phase 3 — `drivers/i2c/client` (concrete IPC client)

- The `I2cClient` *trait* already lives in `api/src/client.rs`. This
  crate provides the concrete IPC implementation that drives requests
  through the channel handle.
- Lift openprot's [`client/src/lib.rs`](../../../openprot/services/i2c/client/src/lib.rs)
  unchanged where possible, re-pointing the channel API at the
  final-usart `userspace` syscall surface.
- Bus index is a parameter on every method (per the openprot wire,
  bus lives in the request header, not in the client handle).

**Checkpoint:** `bazelisk build //drivers/i2c/client:i2c_client`
clean.

### Phase 4 — backends (platform + mock)

Two crates land in this phase. Both publish `crate_name = "i2c_backend"`
and `pub type Backend: I2cBackend`. The system image picks one via
its Bazel deps.

**`target/ast10x0/backend/i2c/` — production AST10x0 backend.**

- `Ast1060I2cBackend` struct holds **one** `Ast1060I2c<'static, fn(u32)>`
  for the bus declared in `system.json5` (single-bus per §5), plus
  the bus index it's bound to (e.g., `bus_id: u8 = 2`).
- Implements `I2cBackend` — each method validates `bus: u8` matches
  `bus_id`, returns `BackendError::InvalidBus` otherwise, dispatches
  to the single `Ast1060I2c<…>` instance.
- See §7 for the yield closure decision (function pointer, no generic).
- `pub type Backend = Ast1060I2cBackend;` is the stable alias the
  server binary imports.

**`drivers/i2c/backend-mock/` — host-side loopback for tests.**

- `MockI2cBackend` simulates a small in-memory bus map for dispatcher
  tests without needing QEMU.
- `target_compatible_with = []` (host only); never linked into the
  embedded image.
- Implements `I2cBackend` against `Vec`/`HashMap` (alloc available
  on host).
- Used by the dispatcher's host tests in Phase 2's checkpoint.

**Checkpoint:** AST10x0 backend builds for `--platforms=//target/ast10x0:ast10x0`;
mock backend builds for host; `bazel test`-style dispatch tests pass
against the mock.

### Phase 5 — `target/ast10x0/tests/i2c/` (system image)

- `server_main.rs` — wait_group setup + `runtime::run`. Identical
  shape to [`tests/usart/server_main.rs`](../../target/ast10x0/tests/usart/server_main.rs).
- `client_main.rs` — minimal smoke test: configure speed, probe a
  known address, write/read a small buffer, then `debug_shutdown(Ok(()))`.
- `target.rs` — same `cortex_m_semihosting::debug::exit` shutdown
  glue we added for usart and crypto, so QEMU exits with status 0.
- `system.json5` — MMIO mapping for the **one** bus the image owns
  (single-bus per §5), the IPC channel handler, and a wait_group.
  **No interrupt objects** in Phase 5 — see §6. Phase 7 adds one
  for the configured bus.
- `BUILD.bazel` — copy of `tests/usart/BUILD.bazel` with
  `usart_*` → `i2c_*` rename and a few dep swaps.

**Checkpoint:**
`bazelisk build --platforms=//target/ast10x0:ast10x0 //target/ast10x0/tests/i2c:i2c`
clean.

### Phase 6 — QEMU smoke test

- Boot the system image in `qemu-ast10x0-i2c`.
- `client_main.rs` runs to completion; semihosting `SYS_EXIT` → qemu
  exits 0.
- Optional second harness mirroring
  [bundle's qtest plan](../../../bundle/drivers/usart/tests/QTEST_PLAN.md):
  use `-qtest unix:` to peek I2C MMIO state mid-flight and assert
  expected register programming. Same Mode A / Mode B split as the
  USART validation.

### Phase 7 — IRQ-driven slave notifications (separate effort)

Slave ops themselves work via polling after Phase 2-6 ships. What
this phase adds is the **wake-up path**: clients can `EnableSlaveNotification`
and then block on the IPC channel until an interrupt-driven RX event
fires.

- Fill in `runtime::run`'s IRQ branch: drain slave RX via
  `backend.drain_slave_rx(bus)` for every notification-enabled bus,
  then `raise_peer_user_signal(channel)` to wake the client.
- Add `enable_slave_notification` / `disable_slave_notification`
  state tracking (the per-bus `notification_enabled: [bool; 14]`
  table from openprot's main.rs).
- Bring back the two halves of openprot's
  [`server/src/main.rs:90-136`](../../../openprot/services/i2c/server/src/main.rs#L90)
  that are currently `//`-commented.

## 4. Backends

The service is designed to host **multiple backends**. The trait
surface is the contract; each backend crate ships its own struct,
its own state, its own constructor.

| Backend | Crate (target name) | Status |
|---|---|---|
| AST10x0 hardware | `//target/ast10x0/backend/i2c:i2c_backend_ast10x0` | Phase 4 |
| Host-side mock (loopback) | `//drivers/i2c/backend-mock:i2c_backend_mock` | Phase 4 (companion, for dispatch tests) |
| Future chip (e.g., AST27x0) | `//target/<plat>/backend/i2c:i2c_backend_<plat>` | When that platform lands |

**Design rules that follow from "multiple backends":**

- **`I2cBackend` is non-generic.** No `const N`, no associated lifetime,
  no closure type leaking into the trait. The wire's `bus: u8` is the
  only universal identifier, and the backend validates it however it
  likes.
- **Each backend picks its own state shape.** AST10x0 holds a single
  `Ast1060I2c<'static, fn(u32)>` for the one bus the system image
  owns (see §5 — single-bus is enough for MCTP-over-I2C, our target
  use case). The mock can use whatever fits the test (a single
  in-memory slot, a small map, etc.). None of this is the trait's
  or the runtime's concern.
- **Backend selection is Bazel-level.** Same convention as usart:
  every backend target uses `crate_name = "i2c_backend"` and exports
  `pub type Backend: I2cBackend`. The system image binary writes
  `use i2c_backend::Backend;` and depends on exactly one backend
  crate. Swapping backends is a one-line dep change in
  `tests/i2c/BUILD.bazel` (or a `label_flag` flip if we add that
  indirection).

## 5. Single-bus, wire-multi-bus-ready

The migration scopes the AST10x0 backend to **one bus per system
image** because the only consumer is MCTP-over-I2C, which runs one
packet stream per bus interface. Multi-bus aggregation belongs above
this layer (in the MCTP service), not below it.

**What single-bus buys today:**

- Backend state is one `Ast1060I2c<…>` field, not an
  `[Option<…>; 14]` array. Saves ~1 KB RAM.
- Runtime has no `notification_enabled` array — one `bool` (or
  nothing, until Phase 7 lands).
- IRQ topology collapses: one bus → one IRQ object (Phase 7), no
  per-bus-vs-bundled decision (see §6).
- Server has no enumeration / "which bus fired" logic.
- Tests don't permute bus indices.

**What stays multi-bus-ready:**

- Wire format keeps the `bus: u8` byte unchanged. Server validates
  it equals the configured bus and returns `InvalidBus` otherwise.
- `I2cBackend` trait keeps `(bus: u8, …)` signatures. A future
  multi-bus backend slots in without protocol or trait changes.
- system.json5 declares which bus this image owns; backend reads it
  via codegen and configures only that controller.

**What this defers (recoverable later):**

- Hosting unrelated I2C consumers from the same server task.
  Re-introducing multi-bus is a 2-3 hour change to the AST10x0
  backend (re-array the slot, validate `bus: u8` against the array)
  plus the runtime's `notification_enabled` going scalar → array.
  Wire and trait don't move.

## 6. IRQ topology

AST1060 has 14 I2C controllers with **one NVIC line per bus**
([qemu/hw/arm/aspeed_ast10x0.c](../../../qemu-ast10x0-i2c/hw/arm/aspeed_ast10x0.c#L94),
IRQs 110–123). With single-bus scoping (§5), only the configured
bus's IRQ matters — and only for the slave-notification path.

| Operational mode | Needs IRQ object? | Why |
|---|---|---|
| Controller (`Write`/`Read`/`WriteRead`/`Probe`/`Configure`/`Recover`) | **No** | `wait_completion` polls via the yield closure |
| Slave polling (`SlaveReceive`/`SlaveWaitEvent` without `EnableSlaveNotification`) | **No** | Backend polls hardware status |
| Slave notification (`EnableSlaveNotification` → `Signals::USER` on channel) | **Yes** | Server must be wakeable from hardware |

**Phase 1-6 ships with zero interrupt objects.** The smoke-test
`system.json5` declares only the `wg`, the `i2c` channel handler,
and the MMIO mapping for the one bus.

**Phase 7 adds one interrupt object** for the configured bus, e.g.:

```jsonc
{ name: "i2c_irq", type: "interrupt", irqs: [{ name: "i2c", number: 112 }] }
```

The runtime registers it with the wait_group with a fixed
`user_data` tag (e.g., `1`); on wake, it drains slave RX and raises
`Signals::USER` on the channel. No bus disambiguation needed — there's
only one.

## 7. Yield closure decision

The peripheral `Ast1060I2c<'a, Y>` requires `Y: FnMut(u32)` —
called as `(yield_ns)(100_000)` between status polls inside
`wait_completion`. Two questions: how to thread the type through, and
what the function should *do*.

### Type threading

- **(a) Generic backend struct.** `Ast1060I2cBackend<Y>` carries the
  closure type. `pub type Backend = Ast1060I2cBackend<fn(u32)>;`
  pins it to a function pointer.
- **(b) Erased fn-pointer field.** `Ast1060I2cBackend` is non-generic,
  takes `fn(u32)` at construction. Loses capturing closures but
  keeps the multi-backend `crate_name = "i2c_backend"` substitution
  trivial.

Settled on **(b)**: backend type stays non-generic, every call site
closure-type-free.

### What the yield does

The peripheral driver doesn't care; the binary supplies the body.
Three sensible bodies:

| Body | Behavior | When to pick |
|---|---|---|
| `core::hint::spin_loop()` | Busy-poll | Bare-metal, no scheduler, latency over CPU |
| `cortex_m::asm::wfe()` | Bare-metal sleep until any event | Low-power without a kernel |
| `object_wait(WG, signals::I2C, MAX) + interrupt_ack` | Task-sleep until I2C IRQ fires | Running under the Pigweed kernel — what we use |

The third option is what
[`tests/i2c/server_main.rs:wait_for_i2c_irq`](../../target/ast10x0/tests/i2c/server_main.rs)
implements. Net behavior: between issuing an I2C op and its
completion, the server task is fully descheduled — the kernel runs
other tasks. The peripheral's polling-shaped loop iterates only when
the controller actually has news.

### IRQ-consumption ordering

The runtime's main loop and `wait_for_i2c_irq` both `object_wait` on
the same `signals::I2C` entry. They never run concurrently — the
server is single-threaded, so only one is parked at a time:

| Server state | Who's blocked on `signals::I2C` |
|---|---|
| Idle in runtime loop | runtime's `object_wait` |
| Mid-dispatch, peripheral op in flight | `wait_for_i2c_irq` inside `wait_completion` |

Real consequence: if a **slave RX event** fires on the bus while the
controller is in the middle of a master TX (multi-role server, rare
but possible on a multi-master bus), the IRQ wakes
`wait_for_i2c_irq`, the peripheral reads status, decides "not my
completion", re-arms NVIC, and iterates. The runtime's drain-and-
notify branch is **not** the one that ran. The slave bytes sit in
the FIFO until the master op finishes and the runtime's loop
eventually fires for the next IRQ.

For single-master controller use (today's smoke test) this never
matters. For mixed-role traffic, latency on slave notifications is
bounded by the longest in-flight master op. Address by either:

- Routing the runtime IRQ branch from inside `wait_completion`'s
  iteration when a non-completion IRQ is detected, or
- Splitting controller and slave IRQs onto separate signals so
  `wait_for_i2c_irq` only consumes completion events.

Neither is needed for Phase 1-6.

## 8. Risks & open questions

- **QEMU AST1060 I2C model fidelity.** The vendored
  `qemu-ast10x0-i2c` has an aspeed_i3c device; the I2C controller
  model coverage is less rigorous than the NS16550 UART. Plan a Mode
  A harness early to surface model gaps before depending on them
  (mirror `bundle/drivers/usart/tests/QTEST_PLAN.md`).
- **Single-bus scoping.** Backend manages exactly one I2C controller
  per system image, the one declared in `system.json5`. Adequate for
  MCTP-over-I2C. Hosting unrelated I2C consumers from the same
  server task is deferred — recoverable in 2-3 hours of backend work
  with no wire or trait changes (§5).
- **I2C IRQ number.** `system.json5` will need the right
  `ast1060_pac::Interrupt` value. Crypto's scaffold left this as a
  placeholder; we should resolve it properly here against the PAC
  binding before Phase 5.
- **`MAX_PAYLOAD_SIZE`.** openprot api uses 256. Reasonable for
  controller-mode I2C; revisit if Phase 7's slave RX path needs
  larger buffers.
- **`pw_log` dependency.** openprot's server uses `pw_log::info!`
  liberally. The bundle's usart server is silent. Match the bundle's
  silence; if logs help debugging during phases 5-6, leave them in
  behind a feature gate.
- **IRQ steal during master ops.** The yield closure
  (`wait_for_i2c_irq`) and the runtime IRQ branch share
  `signals::I2C`. While a master op is in flight, an unrelated
  slave-event IRQ wakes the yield (not the runtime), delaying the
  drain-and-notify path until the master op completes. Bounded by
  the longest in-flight master op. See §7 for mitigations; not
  load-bearing for single-master use.

## 9. What's explicitly out of scope

- Multi-master arbitration changes — peripheral already supports it,
  service stays controller-side single-master.
- DMA mode — peripheral has `new_with_dma` constructor, but the
  service won't expose it in this migration. Add when there's a
  consumer that needs it.
- `embedded-hal::i2c::I2c` trait passthrough at the IPC client.
  Nice-to-have, but the openprot client doesn't do it either; defer.
- Hubris-style key-handle bus identifiers. Keep the `bus: u8` index
  the openprot wire uses.

## 10. Effort estimate

| Phase | Estimate |
|---|---|
| 1 — api | 1.5 h |
| 2 — server | 2 h |
| 3 — client | 1 h |
| 4 — backend | 3 h (most of the work; multi-bus init + per-bus `Ast1060I2c<Y>` instances) |
| 5 — system image | 1.5 h |
| 6 — QEMU smoke | 2 h (includes resolving any model gaps) |
| **Total (Phase 1-6)** | **~11 h** |
| 7 — slave/target | +6-8 h |

Total to controller-mode parity: about 1.5 working days.
