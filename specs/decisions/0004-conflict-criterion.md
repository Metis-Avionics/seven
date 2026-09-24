# Decision 0004 — conflict criterion for `BeliefEngine.conflicts` (DECIDED)

**Status**: decided 2026-09-24 — option **B** adopted (`CONFLICT_SIGMA = 3.0`).
Implemented in `seven-belief` for both paths: `BeliefEngine::incorporate` and
`ImuFusedTrack::update_from_evidence` (same sigma, same likelihood variances,
checked against pre-update marginals — EKF position/velocity diagonal of `P`).

**Refinement made during implementation**: the check uses *predictive*
variance `var + R` (track variance plus the dimension's measurement noise),
not bare `var` — bare `var` collapses toward zero and would flag every
subsequent observation (option A in disguise). First observation per subject
never flags; duplicates never flag (dedup gate returns first).

**Context**: `BeliefEngine` has a `conflicts: Vec<(SubjectId, Evidence, Evidence)>`
field for explicitly flagged conflicting pairs, but nothing populates it. The
engine already *retains* all observations (every independent observation updates
the posterior; none overwrite — invariant 11 test asserts the count), so the
field's purpose is narrower: *flagging* pairs a consumer should inspect, not
retention itself. S04 lists `conflict_retention` as Seven-owned but defines no
criterion for what counts as a conflict. Populating the field requires one, and
the criterion is science — it must be specified, not invented in code.

**Options**:

- **A. Any second distinct observation for the same subject.** Every new
  `observation_id` paired with its predecessor is recorded. Simple and
  deterministic, but noisy: two adjacent GNSS fixes would be "conflicts",
  diluting the signal to uselessness.
- **B. N-sigma disagreement against the current posterior.** A new observation
  is flagged iff it disagrees with the posterior mean by more than `k`
  standard deviations (per-dimension, using the track's own `var`) in any of
  the six dimensions. Deterministic (pure function of prior + evidence),
  self-scaling (tight tracks flag smaller disagreements), and tunable via one
  explicit constant. Suggested default `k = 3.0`.
- **C. Remove the field.** Declare retention-by-count sufficient for invariant
  11 and drop `conflicts`. Smallest surface, but loses the explicit-flagging
  use case the field was designed for (downstream audit, S17
  `conflicting_sensors` scenario introspection).

**Recommendation**: **B** with `CONFLICT_SIGMA = 3.0` as a named, documented,
tunable constant. On `incorporate`, compare the incoming measurement against
the *pre-update* posterior; if any dimension exceeds `k` sigma, push
`(subject, previous_evidence, new_evidence)` — which requires retaining the
last `Evidence` per subject (`BTreeMap<SubjectId, Evidence>` alongside the
existing `incorporated` gate). Same prior + same evidence ⇒ same flags, so
invariants 8/9/13 extend naturally; tests: deterministic-flagging,
no-flag-on-agreement, flag-on-wild-disagreement.

**Consequences (if B is adopted)**: `BeliefEngine` gains a `last_evidence` map
(memory: one `Evidence` per subject — bounded by subject cardinality, same as
`tracks`); `incorporate` stays deterministic; VERIFICATION.md invariant-11 row
gains the flagging tests; S04 `[hard_requirements]` should note the criterion
(`conflict_criterion = "3-sigma-vs-prior"`). If C is adopted instead, delete
the field and close this record as rejected.

**Awaiting**: nothing open. (Deliberately not using `themql-estimation`'s
`EstimatorHealth` innovation gate: Seven's rule reuses the crate's own
likelihood, keeping one criterion across both belief paths.)
