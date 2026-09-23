# Verification — spec §22 invariants × tests

Mapping from the 17 core invariants (spec §22) to named tests in this repo.
All tests run under `cargo nextest run --workspace`. Pedantic clippy must be
clean: `cargo clippy --workspace --all-targets -- -D warnings`.

| # | Invariant (spec §22) | Test | Crate | Class |
|---|----------------------|------|-------|-------|
| 1 | Canonicalization deterministic | `prop_canonicalization_deterministic` + `serialization_deterministic` | seven-core | property |
| 2 | Canonicalization idempotent | `prop_canonicalization_idempotent` + regression `idempotence_regression_found_by_proptest` | seven-core | property |
| 3 | `C(q) == C(-q)` (sign) | `prop_quaternion_sign_equivalence` + `sign_hemisphere_is_deterministic` | seven-core | property |
| 4 | Invalid states rejected | `zero_quaternion_rejected`, `nan_component_rejected`, `infinity_rejected` | seven-core | unit |
| 5 | Evidence id deterministic | `inv5_evidence_identity_deterministic` + `prop_identity_deterministic` | seven-evidence | property |
| 6 | Forwarding ≠ independent evidence | `inv6_inv7_forwarding_not_independent_provenance_kept` + `inv9_duplicate_evidence_not_independent` | seven-evidence | unit |
| 7 | Provenance preserved | same as #6 (asserts hops grow; payload tamper fails verify) | seven-evidence | unit |
| 8 | Bayesian updates deterministic | `inv8_deterministic_posterior` | seven-belief | unit |
| 9 | Duplicates not independently counted | `inv9_duplicate_not_recounted` | seven-belief | unit |
| 10 | Stale distinguishable; sig ≠ freshness | `inv10_stale_distinguishable_signature_independent` | seven-evidence | unit |
| 11 | Conflicting evidence preserved | `inv11_conflicting_evidence_retained` | seven-belief | unit |
| 12 | Transport preserves semantics | `inv12_message_projection_roundtrip` | seven-mql | unit |
| 13 | Identical evidence ⇒ equivalent beliefs | `inv8_deterministic_posterior` (same engine, same inputs) | seven-belief | unit |
| 14 | Replay deterministic | `inv14_replay_deterministic` | seven-replay | unit |
| 15 | Persistence preserves identity/provenance | `write_and_read_subject_history` (live podman dev) | seven-helix | integration |
| 16 | Transport failure does not corrupt canonical | `inv16_transport_failure_does_not_corrupt_canonical` | seven-sim | unit |
| 17 | Never operational authority | compile-time: no ATC/clearance APIs exist in workspace (checked by inspection; §21 is a design boundary) | — | policy |

## Definition of Done — v0.1 (spec §23)

- [x] Seven builds reproducibly — `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` green.
- [x] theMQL/theSix/PRV contracts respected — all consumed via published crates; none reimplemented.
- [x] Canonical state exists — `CanonicalState` in seven-core.
- [x] Quaternion normalization/canonicalization tested — proptest + unit (idempotence regression included).
- [x] Evidence IDs deterministic — blake3 over postcard canonical bytes.
- [x] Provenance represented — `Provenance.hops`, forwarded-not-forked.
- [x] Bayesian update deterministic — linear-Gaussian conjugate update; posterior equality test.
- [x] Duplicate evidence detected — `observation_id` gate.
- [x] Conflicting evidence retained — belief engine keeps both; count test.
- [x] HelixDB persistence/recovery — live integration test on podman.
- [x] Deterministic replay — double-run fingerprint equality.
- [x] Simulated transport with fault injection — `LossyMql` (loss/dup) + `LossyChannel` in sim (reorder at stream level pending Phase 8).
- [x] Property/invariant tests cover the core math contracts — 39 tests.
- (deferred) QUIC transport — *Phase 8* per §22 dependency ordering.
- (deferred) LoRa transport — *Phase 8* per §22 dependency ordering.

The deferred items are spec-sanctioned: §22's implementation order places both
network transports after the deterministic core, which this commit establishes.
