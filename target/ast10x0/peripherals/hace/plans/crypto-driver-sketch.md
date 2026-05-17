# Crypto driver — architectural sketch (USART-framed)

**Status:** design sketch / agent kickoff. Non-normative. Models the layered
driver in `drivers/usart/` ([README](../../../../../drivers/usart/README.md)) and
binds it to the decisions in [goal.md §5](goal.md) and the consumer split in
[crypto-consumers.md](crypto-consumers.md). Where this sketch and §5 disagree,
§5 wins.

The single most important thing in this document is **§3 — where the crypto
driver must *not* copy USART**. The crate/file shape is mechanical; the
divergences are the architecture.

---

## 1. Crate map (mirrors `drivers/usart/`)

| Bazel target | Crate | Role | USART analog |
|---|---|---|---|
| `//drivers/crypto/api` | `crypto_api` | Wire protocol + backend trait contract | `usart_api` |
| `//drivers/crypto/server:crypto_server` | `crypto_server` | `dispatch_request` + `runtime::run` loop; owns **both** engines | `usart_server` |
| `//drivers/crypto/traits` | `crypto_traits` | **Abstract `Digest`/`Mac`/`Cipher`/`Verify` traits + the generic `Stack<B>` facade — the substitution seam (§3.3, ADR-C1)** | `mctp-api` (traits + `stack::Stack`) |
| `//drivers/crypto/soft` | `crypto_soft` | In-process software impl **of the `crypto_traits` traits** | *(new)* |
| `//drivers/crypto/client` | `crypto_client` | **IPC adapter that *implements* the `crypto_traits` traits** (≈ `IpcMctpClient`), talking to `crypto_server` | `mctp-client` (`IpcMctpClient`) |
| `//target/<plat>/backend/crypto` | `crypto_backend` | `impl SymmetricBackend`/`PublicKeyBackend`; **the only crate that names HACE/SBC** | `usart_backend` |
| `//target/<plat>/peripherals` | `<plat>_peripherals` | Borrow-arbitrated `HaceDevice` / SBC raw-MMIO drivers (the ports we documented) | PAC UART |

> **Naming guardrail (ADR-C2, see §3.8 & `drivers/usart/README.md` §0):**
> everything under `drivers/crypto/` is platform-agnostic and names *capability
> classes* — **Symmetric** (hash/MAC/cipher) and **PublicKey** (verify/modexp) —
> never silicon. The vendor mapping `Symmetric → HACE`, `PublicKey → SBC` exists
> **only** in `target/<plat>/backend/crypto`. Vendor names below appear solely
> in `target/`-scoped rows/boxes by that rule.

Two capability classes, two contracts, **one server instance owning both**
(decided). The two underlying engines remain *separate singletons* —
[crypto-consumers.md §2.1](crypto-consumers.md) — each its own borrow-arbitrated
device with its own backend trait; the single server process owns **both** MMIO
+ DMA regions in one address space and routes by the `engine` (class)
discriminator in the request header (§4). **Accepted tradeoff:** one dispatch
loop serialises both classes, coupling their availability — acceptable because
every traced consumer is bounded run-to-completion and neither path is hot. If
that ever changes, the split into two server binaries is mechanical (per-class
backend already separated).

## 2. Layer diagram

```
┌─ Consumer (SPDM / attestation / PFR / DICE / secure-boot) ────────┐
│   depends ONLY on crypto_traits — never names a backend (§5.2.1)  │
└───────────────┬──────────────────────────┬───────────────────────┘
        links   │                          │  links
        (boot / secret / interleaved)      │  (bulk non-secret, runtime)
                ▼                          ▼
        ┌─ crypto_soft ─┐          ┌─ crypto_client ─┐
        │  in-process   │          │  IPC facade     │
        │  SW impls     │          └────────┬────────┘
        └───────────────┘                   │ Pigweed IPC channel (whole-object)
         (no engine, no IPC)                 ▼
                                   ┌─ crypto_server (one process) ───┐
                                   │ runtime::run → dispatch_request │
                                   │ single loop = the request queue │
                                   │ routes by header.engine         │
                                   └────────┬────────────────────────┘
                                            │ SymmetricBackend + PublicKeyBackend
                                            ▼
                                   ┌─ crypto_backend (per platform) ─┐
                                   │ ONLY layer that names silicon:  │
                                   │ Symmetric→HACE, PublicKey→SBC   │
                                   └────────┬────────────────────────┘
                                            ▼
                                   borrow-arbitrated PAC ports (vendor blocks)
```

## 3. Divergences from the USART model — the actual architecture

USART is a byte-stream device with blocking/non-blocking reads and IRQ-parked
requests. Crypto is a non-multiplexable run-to-completion engine with a
secret-material hazard. Five deliberate departures:

### 3.1 No streaming across IPC — whole-object ops only
USART's `Read`/`TryRead` + `PendingRead` + IRQ-completion park pattern
(`server/runtime.rs`) is **exactly the pattern crypto must NOT copy**
([goal.md §5.2.2](goal.md)). A held-open `begin/update/finish` session across
the IPC boundary recreates the unsupported held-across-yield engine lock, now
spanning processes. Crypto ops are **whole-object, run-to-completion**: the
server completes them internally and releases the engine *before* it replies.
`HashRegion` runs the 4 KB page loop *inside the server*. There is **no
`PendingRead` equivalent and no IRQ-park branch** — the crypto runtime is
strictly simpler than USART's.

### 3.2 The dispatch loop *is* the request queue
USART needs `PendingRead` because RX is asynchronous. Crypto doesn't: a
single-threaded `runtime::run` loop processing one whole-object request to
completion before the next **is** the serialization that replaces the
reference's `in_use`/`-EBUSY` flag ([goal.md §5.2.2](goal.md)). The
borrow-arbitrated symmetric (and public-key) engine singleton lives in the
backend owned by that one loop → engine exclusivity is structural, not a runtime
flag (delta A1). With one server owning both classes the loop also serialises
them — a deliberate simplification (§1), not a constraint either engine imposes.

### 3.3 Backend selection is a wiring decision, not driver code
The `crypto_traits` seam (no USART analog) is the core of §5. Consumers depend
only on abstract traits; the binary wires either `crypto_soft` (in-process) or
`crypto_client` (IPC → server) per the [§5.3 policy table](goal.md):

| Workload | Wire to | Why |
|---|---|---|
| SPDM transcript, HKDF, session keys, signatures, secret-keyed AES | `crypto_soft` | Long-lived/interleaved or secret-bearing; HW would starve others |
| PFR / image / manifest measurement, bulk non-secret AES | `crypto_client` → server | Bounded, non-yielding, non-secret; IPC amortised over large buffers |
| Small one-shot non-secret hash | `crypto_soft` (default) | Not worth IPC unless profiled hot |
| Fallback when HW busy | `crypto_soft` (always present) | Correctness must never depend on the singleton |

### 3.4 Secret material does not cross the IPC boundary
USART payloads are non-sensitive bytes. Over the crypto wire a key is **only an
opaque sealed-key handle** (the hardware sideload path; on AST10x0 this maps to
the OTP slot path — [crypto-consumers.md §1 row 6](crypto-consumers.md)) — never
raw secret bytes. Raw-key / secret AES and signing live in `crypto_soft`, in the
consumer's own address space. `KeyRef` over the wire is effectively
`Sealed(u8)` only; `Raw(&[u8])` is permitted only for the non-secret bulk case.

### 3.5 Verify-failure is a result, not an error
For public-key `EcdsaP384Verify` / RSA, a failed signature check is a **normal
cryptographic outcome**, a distinct status (`VerifyFailed`) — not
`InternalError`. (USART has no analog; getting this wrong turns a forged
signature into a retryable transport error.)

### 3.6 Boot-time path
DICE measured-boot and early secure-boot verify (the earliest consumers) run
before user-space exists and **cannot RPC** a server that is not up yet. They
take either pure-software `crypto_soft` **or the HW engine via
`Client<LoopbackTransport>`** (in-process dispatch, no IPC) — see ADR-C5; never
`crypto_client` over IPC. The bounded-`CryptoBackend` seam (§3.3, ADR-C3) is
what makes that substitution invisible to the consumer.

### 3.7 ADR-C1 — IPC stays behind the trait; consumers never name a transport
**Decided. Mirrors `services/mctp` (`mctp-api` traits + `IpcMctpClient` +
`stack::Stack<C>`).**

The MCTP rule is *not* "nothing is coupled to IPC" — it is **the IPC coupling is
confined to one trait-implementing adapter crate, and consumers only ever see
the trait.** `IpcMctpClient` is intentionally soaked in syscalls; SPDM never
touches it — it depends on `MctpClient` and is handed a `Stack<C>`.

Crypto adopts this exactly:

- `crypto_client::CryptoClient` is the IPC adapter (≈ `IpcMctpClient`). Being
  coupled to `userspace::syscall` / `pw_status` is *correct and expected for
  this crate* — that is its single responsibility.
- It must **`impl crypto_traits::{Digest,Mac,Cipher,Verify} for CryptoClient`**.
  IPC marshalling is private. It must **not** present inherent op methods as the
  consumer API (the divergence the first scaffold introduced).
- `crypto_soft` implements the *same* traits in-process. The two are
  interchangeable; neither name appears in consumer code.
- `crypto_traits` ships a generic **`Stack<B>`** facade (≈ `mctp-api::stack`):
  `Stack::new(backend)` where `B: Digest + Mac + Cipher + Verify`, exposing
  ergonomic `digest()/mac()/cipher()/verify()` and hiding which backend/transport
  is underneath. Consumers depend on `crypto_traits` **only**; the binary wires
  `Stack::new(CryptoClient::new(h))` or `Stack::new(SoftCrypto::new())` per the
  §3.3 / [§5.3](goal.md) table.

Consequence: a consumer cannot become IPC-coupled even by accident — it has no
path to a syscall except through a trait it could equally satisfy with
`crypto_soft` or a test mock. This is the property that makes §3.6 (boot-time
substitution) and §3.3 (wiring-time backend choice) actually hold.

### 3.8 ADR-C2 — `drivers/` is platform-agnostic; capability names only
**Decided. Codified as the repo-wide guardrail in `drivers/usart/README.md`
§0.** Nothing under `drivers/` (or any path outside `target/<plat>/`) may name
a SoC, vendor, silicon block, or peripheral instance — not in code, types,
identifiers, doc comments, or Bazel targets, and no PAC/`*_peripherals` dep.
Name by **capability class**:

| Agnostic (`drivers/crypto/`) | AST10x0 silicon (`target/.../backend/crypto` only) |
|---|---|
| `Engine::Symmetric`, `SymmetricOp`, `SymmetricBackend` | HACE (hash/HMAC/AES block) |
| `Engine::PublicKey`, `PublicKeyOp`, `PublicKeyBackend` | SBC (ECDSA/RSA block) |
| `KeyRef::Sealed(u8)` | OTP slot / `…KEY_FROM_OTP` sideload |

The class→block mapping exists in exactly one crate: the per-platform
`crypto_backend`. Smell test: if a name in a `drivers/` crate would change to
port to another SoC, it is in the wrong layer. (This ADR is why the first
`crypto_api` scaffold's `Hace`/`Sbc` names were reverted.)

### 3.9 ADR-C3 — reuse `openprot_hal_blocking`, bounded by one supertrait
**Decided.** The bespoke `crypto_traits` `Digest/Mac/Cipher/Verify` are a
duplicate of the repo's canonical crypto HAL (`openprot_hal_blocking`, already
implemented by the HACE port) and are removed. But the HAL is a *general crypto
framework*, overengineered for a driver seam (see ADR-C4). The driver therefore
reuses **one bounded slice** of it, pinned in exactly one place:

```
pub trait CryptoBackend:
      digest::scoped::DigestInit<Sha2_256> + …<Sha2_384> + …<Sha2_512>
    + mac::scoped::MacInit<…>
    + cipher::CipherInit<Ecb> + cipher::CipherInit<Cbc>
    + ecdsa::EcdsaVerify<P384> {}
// blanket impl; Stack<B: CryptoBackend> unchanged
```

Scope rules: **scoped API only** (never surface `owned::*`); **no** AEAD /
stream / `CipherStatus` / rekey / keygen / sign / non-P384 curves. A single
runtime-dispatch shim in `crypto_server` bridges the wire `u8` algo → the typed
HAL call, absorbs `Digest<N>` (`[u32;N]`) ↔ `&[u8]`, and wraps the verify
mismatch (ADR-C4). All HAL impedance lives in that one module.

### 3.10 ADR-C4 — the HAL's overengineering is quarantined, not adopted
**Decided (assessment-driven).** Flagged HAL smells, kept *behind* the ADR-C3
seam, not propagated, and raised as upstream HAL tech-debt (separate from this
driver): (1) duplicate `scoped` vs `owned` trait families; (2) `cipher.rs` is an
~8-trait framework for what is ECB/CBC-128/256 here; (3) `ecdsa.rs` is a full
multi-curve keygen/sign/verify stack for what is P-384-verify-only;
(4) compile-time-algorithm typestate vs the driver's runtime dispatch;
(5) `Digest<N>` is words not bytes; (6) **`EcdsaVerify::verify` returns
`Result<(),Error>` — a failed signature is modelled as an error, violating
§3.5.** The verify shim translates "invalid signature" → `Ok(false)`, real
faults → `Err`; this reconciliation is mandatory and isolated.

### 3.11 ADR-C5 — transport is a pluggable, whole-object-by-construction trait
**Decided.** The trait seam already lets the *binary* hand a consumer an
in-process backend directly (zero transport/marshalling — the efficient
single-address-space path) **or** a cross-process client. Orthogonally, the
client's transport itself is abstracted so the *same marshalling code* serves
more than Pigweed IPC:

```
trait Transport { fn transact(&mut self, req: &[u8], resp: &mut [u8])
                              -> Result<usize, TransportError>; }
// crypto_client::Client<T: Transport> implements the ADR-C3 seam, transport-agnostic
```

`transact` is **inherently whole-object** (bytes in → bytes out, one shot) for
*every* impl, so §3.1 "no streaming across the boundary" is enforced
structurally by the signature, not by convention.

Impls / priority:
- **P1 — `IpcTransport`** (Pigweed channel; the production cross-process path).
- **P2 — `LoopbackTransport` (testability, first-class, not deferred):** calls
  `crypto_server::dispatch_request` directly against an in-process backend, so
  the marshalling + protocol + backend path is host-testable with **no
  kernel/QEMU**.
- Later — other channels (shared-mem mailbox, etc.) as needed.

This also corrects §3.6: the **boot-time HW path is `Client<LoopbackTransport>`
(in-process dispatch), not forced pure-software `crypto_soft`.** Caution
(anti-overbuild): `Transport` is *not* mandatory on every call — pure
single-address-space still hands the backend trait object over directly; the
abstraction earns its keep only for IPC, loopback-test, and future channels.

## 4. Wire protocol (`crypto_api::protocol`) — sketch

Same `repr(C, packed)` + `zerocopy` discipline as `usart_api::protocol`.

One header, an `engine` (capability-class) discriminator routes to the right
backend in the one server; op space is namespaced *per class*.

```rust
#[repr(u8)] pub enum Engine { Symmetric = 0x00, PublicKey = 0x01 }

#[repr(u8)] pub enum SymmetricOp {   // engine == Symmetric
    HashOneShot   = 0x01,            // algo + payload            -> digest
    HashRegion    = 0x02,            // algo + RegionDescriptor   -> digest  (server runs page loop)
    HashSg        = 0x03,            // algo + [complete segments]-> digest
    Hmac          = 0x04,            // algo + KeyRef + payload   -> mac
    AesCrypt      = 0x05,            // AesParams + KeyRef + iv + payload -> out
}                                    // NOTE: no Begin/Update/Finish — §3.1

#[repr(u8)] pub enum PublicKeyOp {   // engine == PublicKey
    EcdsaP384Verify = 0x01,          // pubkey + sig + 48B digest -> {Pass|VerifyFailed}
    RsaModexp       = 0x02,          // ExponentSel + key + in    -> out
}

#[repr(u8)] pub enum CryptoError {
    Success=0x00, InvalidOperation=0x01, InvalidAlgo=0x02, InvalidKeyHandle=0x03,
    InputNotBlockAligned=0x04,        // AES delta A4 — typed, pre-engine
    Busy=0x05, Timeout=0x06, EngineFault=0x07,
    VerifyFailed=0x08,                // §3.5 — a result, not a fault
    InternalError=0xFF,
}

#[repr(C, packed)] #[derive(FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct CryptoRequestHeader { pub engine:u8, pub op:u8, pub algo:u8,
                                 pub key_handle:u8, pub arg0:u16, pub payload_len:u16 }
// 8 bytes, like UsartRequestHeader. engine→{SymmetricOp|PublicKeyOp} decode.
// CryptoResponseHeader mirrors UsartResponseHeader: { status:u8, _r:u8, payload_len:u16 }
```

`KeyRef` (§3.4): `Sealed(u8)` over the wire; `Raw` only for non-secret bulk.
`RegionDescriptor { base:u64, len:u32 }` — server owns the page loop.

## 5. Backend trait (`crypto_api::backend`) — the platform seam

Mirrors `UsartBackend`; `BackendError → CryptoError` 1:1 via `From`. Capability
names only (ADR-C2); the platform crate binds them to silicon.

```rust
pub trait SymmetricBackend {                  // platform impl owns the one engine
    fn hash(&mut self, a: Algo, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError>;
    fn hash_region(&mut self, a: Algo, r: Region, out: &mut [u8]) -> Result<usize, BackendError>;
    fn hash_sg(&mut self, a: Algo, s: &[Seg], out: &mut [u8]) -> Result<usize, BackendError>;
    fn hmac(&mut self, a: Algo, k: KeyRef, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError>;
    fn aes(&mut self, p: AesParams, k: KeyRef, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError>;
}
pub trait PublicKeyBackend {                  // separate engine, SAME server process
    fn ecdsa_p384_verify(&mut self, pk: EcPubP384, sig: EcSig, digest48: &[u8;48]) -> Result<bool, BackendError>;
    fn rsa_modexp(&mut self, e: ExpSel, key: &RsaKey, input: &[u8], out: &mut [u8]) -> Result<usize, BackendError>;
}
// target/<plat>/backend/crypto exports `pub type SymmetricBackend` +
// `pub type PublicKeyBackend` and is the ONLY place HACE/SBC are named.
```
On AST10x0 the `SymmetricBackend` impl holds the borrow-arbitrated `HaceDevice`
by value; `HaceDigest`/`HaceHmac`/`AesCipher` are obtained per-call via the
`&mut` borrow-split (`design-patterns :: borrow-arbitrated-engine-exclusivity`).
Single ownership in the server loop = structural hash⇄AES exclusivity. (Those
vendor types live only in the platform backend, per ADR-C2.)

## 6. Server (`crypto_server`) — simpler than USART

- `dispatch_request<Y: SymmetricBackend, P: PublicKeyBackend>(&mut Y, &mut P,
  req, resp) -> usize` — pure protocol→backend translator. Reads
  `header.engine`, routes to the Symmetric or PublicKey arm, decodes the
  per-class op. No IPC, no parking, **no `DispatchOutcome::Queued` variant**
  (everything is `Respond(len)`).
- `runtime::run<Y, P>(&mut Y, &mut P, wg, …) -> !` — `object_wait →
  channel_read → dispatch_request → channel_respond`. Same topology-agnostic
  `user_data` channel routing as USART; one `crypto` channel carries both
  engines. **No IRQ-park branch.** Completion polls the engine with a bounded
  budget + `yield_fn` (goal §1 D1) entirely *within* `dispatch_request`; the
  engine is released before `channel_respond`.

## 7. Per-target binding & system image (mirrors USART §5/§7)

`drivers/crypto/` ships only platform-agnostic libs. Per platform,
`target/<plat>/tests/crypto/` carries one `system.json5` mapping **both** the
HACE and SBC MMIO regions, a single `crypto` IPC channel handle, and the server
thread stack; one `crypto_server_bin` (`rust_app` deps `//drivers/crypto/server`
+ the platform `crypto_backend`); and consumer apps. Consumers on the software
path link `crypto_soft` and need **no server and no channel**. Companion
`:crypto_test` (QEMU) + `:no_panics_test` like USART.

## 8. Extension points

- **New algo/op**: add to `SymmetricOp`/`PublicKeyOp`, extend the backend
  trait, add a `dispatch_request` arm, add the abstract-trait method, update §4.
- **New platform**: new `target/<plat>/backend/crypto` exporting
  `pub type SymmetricBackend`/`PublicKeyBackend`; nothing under
  `drivers/crypto/` changes.
- **New consumer**: depend on `crypto_traits` only; the binary picks
  `crypto_soft` vs `crypto_client` per the §3.3 / [§5.3](goal.md) table.

## 9. Suggested agent decomposition

Independent, parallelisable once §4/§5 are frozen:

1. `crypto_api` — protocol (`engine`-discriminated header) + both backend traits
   + `From` glue (no deps; do first).
2. `crypto_traits` + `crypto_soft` — abstract seam + software backend.
3. `crypto_server` — `dispatch_request<Y,P>` + `runtime::run<Y,P>`, class
   routing (depends on 1).
4. `crypto_client` — IPC adapter implementing the `crypto_traits` traits for
   both classes (depends on 1 + 2).
5. `target/ast10x0/backend/crypto` — one crate binding `SymmetricBackend` →
   the HACE port and `PublicKeyBackend` → the SBC port; **only crate naming
   silicon** (depends on 1 + the existing ports).
6. `target/ast10x0/tests/crypto` — one `system.json5` (both MMIO regions), one
   server bin, QEMU smoke test (depends on 3/4/5).
