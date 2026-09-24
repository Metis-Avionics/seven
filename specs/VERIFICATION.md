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
| 8 | Bayesian updates deterministic | `inv8_deterministic_posterior` + `imu_track_deterministic_posterior` (EKF path: same evidence ⇒ same fused posterior) | seven-belief | unit |
| 9 | Duplicates not independently counted | `inv9_duplicate_not_recounted` + `imu_track_duplicate_collapsed_predict_counted` (EKF dedup gate) | seven-belief | unit |
| 10 | Stale distinguishable; sig ≠ freshness | `inv10_stale_distinguishable_signature_independent` | seven-evidence | unit |
| 11 | Conflicting evidence preserved | `inv11_conflicting_evidence_retained` + `conflict_wild_disagreement_flagged_with_pair` + `conflict_agreement_not_flagged` + `conflict_deterministic_and_duplicates_ignored` + `imu_track_conflict_flagging_mirrors_engine` (decision 0004, 3-sigma vs predictive variance, both belief paths) | seven-belief | unit |
| 12 | Transport preserves semantics | `inv12_message_projection_roundtrip` + `subject_shape_matches_spec_grammar` + `nonconforming_subject_rejected` + `quic::frame_roundtrip_preserves_message` (QUIC envelope) + `lora::compact_frame_roundtrip` (compact projection, covered fields) | seven-mql | unit |
| 13 | Identical evidence ⇒ equivalent beliefs | `inv8_deterministic_posterior` (same engine, same inputs) | seven-belief | unit |
| 14 | Replay deterministic | `inv14_replay_deterministic` + `fingerprint_covers_beliefs_and_db` (fingerprint covers messages, node liveness, db overlay, AND belief posteriors) | seven-replay | unit |
| 15 | Persistence preserves identity/provenance | `write_and_read_subject_history` (live podman dev: identity read-back + `FORWARDED_TO` custody-chain round-trip via `write_provenance`/`read_provenance` + `Belief` snapshot round-trip via `write_belief_state`/`read_belief` with `SUPPORTS` edge + parent/child lineage round-trip via `write_parentage`/`read_derived` with `PARENT_OF` + `DERIVED_FROM` edges) | seven-helix | integration |
| 16 | Transport failure does not corrupt canonical | `inv16_transport_failure_does_not_corrupt_canonical` | seven-sim | unit |
| 17 | Never operational authority | compile-time: no ATC/clearance APIs exist in workspace (checked by inspection; §21 is a design boundary) | — | policy |

## Phase 8 — transport framing + EKF wiring (beyond v0.1 core)

| Item | Tests | Crate | Notes |
|------|-------|-------|-------|
| EKF composed for IMU-fused tracks (S04) | `imu_track_deterministic_posterior`, `imu_track_duplicate_collapsed_predict_counted`, `imu_track_bad_dt_rejected` | seven-belief | `ImuFusedTrack` owns gating; `themql-estimation::Ekf` owns filter mechanics; `sample()` never called on the deterministic path |
| QUIC framing codec (S06, no `TransportKind` extension) | `quic::frame_roundtrip_preserves_message`, `quic::partial_frame_waits_for_more_bytes`, `quic::oversize_frame_rejected_explicitly`, `quic::back_to_back_frames_decode_in_order` | seven-mql | `[len_be32][json]` envelope (JSON because `metadata.extensions` is `serde_json::Value`, which postcard cannot represent); socket wiring (quinn/TLS) still pending |
| LoRa compact framing + budget (S06/S11) | `lora::compact_frame_roundtrip`, `lora::representative_frame_fits_app_budget` (≤200 B app / ≤237 B packet), `lora::hop_count_summarizes_provenance`, `lora::fragments_roundtrip_out_of_order_with_dups`, `lora::reassembly_rejects_missing_fragment`, `lora::corrupt_version_rejected` | seven-mql | Full `Message` is 785 B JSON / 429 B postcard (measured) — never rides LoRa directly; provenance summarizes to saturating hop count, full chain via HelixDB `reconstruct_provenance`; `expires_at` round-trips via `seven_expires_at` extension (`received_at` is per-receipt and legitimately does not) |
| Sim reorder stage (S13/S17) | `reorder_deterministic_per_seed`, `reorder_events_observable`, `send_stream_no_faults_is_identity`, `send_stream_deterministic_and_conserving`, `send_stream_reorder_probability_gates_shuffle` | seven-sim | Seeded Fisher–Yates `reorder_in_place` closes the loop the `LossyChannel` comment promised: every swap logs `TransportReorder`, permutation is lossless; `send_stream` combines per-message loss/dup with the shuffle stage, giving `reorder_p` its meaning |

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
- [x] Property/invariant tests cover the core math contracts — 65 tests.
- [x] EKF wired into belief for IMU-fused tracks — `ImuFusedTrack` composes `themql-estimation::Ekf` (determinism + dedup + explicit-dt tests).
- [x] QUIC framing codec — length-prefixed JSON envelope roundtrip + partial/oversize/ordering tests (socket wiring pending).
- [x] LoRa framing codec — compact binary frame with ≤200 B budget test + fragmentation/reassembly tests (radio driver pending).
- (deferred) QUIC socket wiring (quinn/TLS/congestion) — *Phase 8 remainder* per §22 dependency ordering.
- (deferred) LoRa radio driver + RF physical harness — *Phase 8–9 remainder* per §22 dependency ordering.

The deferred items are spec-sanctioned: §22's implementation order places both
network transports after the deterministic core, which this commit establishes.
