# ECDSA Interface-Authority Assessment (`openprot_hal_blocking::ecdsa`)

Feeds Phase 4 (separate the three authorities) of [goal.md](goal.md). Target
reviewed: [`hal/blocking/src/ecdsa.rs`](../../../../../hal/blocking/src/ecdsa.rs)
(1102 lines), the **interface authority** the AST10x0 ECDSA port must satisfy.
This is *verify-the-mandate* due diligence: the trait is quoted, not assumed.

## Framing correction (important)

`openprot_hal_blocking` is a **forward-looking HAL interface crate**, intended
to be implemented by platforms that do not exist yet. An earlier draft of this
assessment scored it as application code and treated "broad unused surface +
zero workspace consumers" as over-engineering. **That lens was wrong and is
withdrawn:**

- Trait-splitting (`EcdsaSign`/`EcdsaKeyGen`/`EcdsaVerify`) when this target
  only verifies is normal HAL design (cf. `embedded-hal`).
- A curve-marker vocabulary (P256/P384/P521/Brainpool/Secp256k1) defined ahead
  of implementations is the *purpose* of a HAL.
- Zero consumers in this workspace is **expected** for an interface crate and
  is **not** a defect. (Verified: of the over-built symbols, only `P256`
  appears once outside the def/`target` dirs; the rest have no in-tree
  consumer — for a HAL this is normal.)

The assessment below is therefore re-anchored on the only question that
matters for a HAL: **is this a good, implementable, permanent interface
contract?** Interface defects propagate to *every* future platform, so they
weigh more, not less.

## Scores (recalibrated for HAL intent)

| Axis | Score | One-line |
|------|-------|----------|
| Ergonomics (of *implementing* the trait) | **5/10** | Verify subset clean; out-params + dual error model are friction every platform eats |
| Complexity / over-engineering | **7/10** | Surface breadth is defensible for a HAL; demerits are genericity that doesn't pay for itself, not bloat |
| Rust BKMs | **5/10** | Good `non_exhaustive`/`Error::kind`/`Infallible`/`Zeroize`; undermined by contract-vs-impl gaps and doc drift |

(Earlier draft scored Complexity 3/10 — revised up after the HAL-intent
correction; the surface-breadth critique was retracted.)

## Defects that stand (independent of consumer count; worse for a HAL)

1. **`Curve` cannot satisfy its own documented contracts.** `Curve` exposes
   only `DigestType` + `Scalar` (`ecdsa.rs:382-387`); markers are ZSTs. Yet
   `PrivateKey::validate(&self, curve: &C)` (`:352`) documents a
   `1 < key < curve_order` check (`:345-351`) — the order/modulus exist
   nowhere in the trait, and `&C` carries no runtime data. Every future
   implementer inherits an unmeetable contract and a dead `&C` parameter.
   *Highest-leverage fix:* either give `Curve` the domain parameters its
   contracts require, or delete the contracts that lie. (Additive / largely
   non-breaking.)

2. **Two error models in one interface.** Operations use the typed
   `Self::Error: Error` + `kind()` mapping (`:129-167`, `:825`); constructors
   and `validate` return the bare generic `ErrorKind` (`:352`, `:439`,
   `:510`). The error contract is the most-copied part of any interface;
   inconsistency here propagates to all consumers. Unify on one model
   (recommend: associated `Self::Error` everywhere, `kind()` for generic
   reaction).

3. **Reference impl violates its own trait contract.** `Signature` promises
   `from_coordinates` rejects out-of-range `r,s` (`:427-438`); the shipped
   `P384Signature::from_coordinates` has `TODO: Add proper signature
   validation` and accepts anything (`:977-980`). For a HAL the reference impl
   is the conformance template — it currently teaches the wrong behavior.

4. **Un-idiomatic accessors baked into the trait.** `coordinates(&self,
   x_out: &mut C::Scalar, y_out: &mut C::Scalar)` (`:443-448`, `:489-494`)
   justified as "zero-allocation for embedded", but `C::Scalar` is a `Copy`
   array (`[u8;48]` for P384) — `-> (C::Scalar, C::Scalar)` is equally
   alloc-free and idiomatic. Permanent implementer friction. Plus the
   trait-level params are named `x_out/y_out` for a signature whose
   components are `r/s` (`:448`).

5. **Asymmetric concrete types.** Six curves have `Curve` impls but only
   P384 has concrete `P384PublicKey`/`P384Signature` (`:914-994`). A HAL
   should provide either none (platforms define their own) or a consistent
   set; shipping exactly one is neither vocabulary nor implementation —
   incoherent, though half-excused since markers alone are fine.

6. **Doc/code drift, untested docs.** `validate` example shows `fn
   validate(&self)` (`:315`) vs the actual `fn validate(&self, curve:&C)`
   (`:352`); nearly all examples are `rust,ignore` so they are never
   compile-checked and already stale.

## Bottom line for the AST10x0 port (Phase 4 input)

The port's interface obligation is **only** `EcdsaVerify<P384>::verify(&mut
self, &P384PublicKey, digest, &P384Signature) -> Result<(), Self::Error>`
(`:808-826`, `:930-994`). That subset is usable and adequately ergonomic;
none of the defects above block the port.

The one finding to carry into Phase 4 §interface-split: **the trait documents
signature-component range validation that its own reference `P384Signature`
does not perform** (defect 3). Therefore the port must treat `r,s` validity as
**not guaranteed by the interface layer** — exactly the HACE conclusion
("openprot mandates *shape*, not *algorithm*"). If `r,s` range checking
matters for parity/correctness it must live in the port or be explicitly
declared out of scope; it cannot be assumed from the trait contract.

No code was modified. This is an advisory interface-authority assessment.
Fixing the HAL defects (1)–(6) is a separate decision outside this port's
parity scope.
