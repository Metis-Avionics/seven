# Decision 0005 — S15 replay-resistance reading (PROPOSED)

**Status**: proposed 2026-09-24. Awaiting adjudication — spec clarification
only; explicitly NO code or schema change proposed. See invariant 9
(duplicates), invariant 10 (stale), invariant 6 (forwarding).

**Question**: `specs/s03-evidence.toml` (S15) states replay resistance comes
from "`observed_at` + `expires_at` + sequence carried canonically". But:

- there is NO field named `sequence` anywhere in `CanonicalState` or
  `Evidence`, and
- `expires_at` is deliberately OUTSIDE the hashed identity
  (`EvidenceId = blake3(canonical_bytes)`; `CanonicalState` carries only
  position/velocity/attitude/`observed_at_nanos`).

A literal reader might conclude replay protection is unimplemented and
"fix" it by folding a sequence counter and/or expiry into the canonical
bytes. That fix would be destructive (see below).

**Analysis — the substance already exists, under different names**:

- "Sequence" ≡ `observation_id` + provenance chain. The same physical
  observation re-observed or replayed yields the same payload hash, hence the
  same `observation_id`, hence dedup at the `BeliefEngine` gate (invariant 9)
  and the `ImuFusedTrack` gate. Causal order across distinct observations
  comes from `parent_evidence_ids` + `Provenance.hops`, not from a counter.
  A counter would need a single writer per subject to be meaningful —
  Seven has independent observers by design, so a counter could not even be
  assigned without coordination (which partitioned nodes cannot do).
- `expires_at` MUST stay outside identity. Folding it into the hashed bytes
  would fork identities on re-issue with extended expiry (breaking
  forwarding, invariant 6: same observation, new id) and would conflate
  freshness with identity (forbidden by invariant 10: a valid signature must
  never imply fresh, and identity must never imply fresh either).
- `observed_at` IS canonical (part of the hashed bytes), so backdated replays
  of distinct observations keep distinct identities and are judged stale by
  `is_stale`, not by identity tricks.

**Recommendation**: clarify the S15 wording to
`replay_resistance = "observed_at (canonical) + expires_at (carried, never hashed) + observation_id dedup + provenance order"`,
and record the negative constraint: never fold expiry or a sequence counter
into `CanonicalState` (would break invariants 6/9/10 and fork every evidence
id per decision 0001's consequences note). No code, test, or schema change.

**Awaiting**: wording approval (or an alternative reading with a concrete
attack the current mechanism fails to stop — replayed-duplicate,
delayed-replay, and forked-identity cases are all covered above).
