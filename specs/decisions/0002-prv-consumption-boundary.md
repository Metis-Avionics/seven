# Decision 0002 — economic-prv consumption boundary

**Status**: decided 2026-09-23.

**Audit finding**: economic-prv's `prv_core::State` is economically shaped
(`capacity`, `investment`, `labour_absorption`, …) and `prv-monte-carlo`'s
`Simulator` is coupled to that type: `ShockSpec::apply(&self, state: &mut State)`.

Spec §S08 demands importing *generic mathematical primitives* "where their
existing contracts apply" and forbids importing economic-domain semantics.

**Decision**: Seven declares `prv-monte-carlo = "0.2.1"` (crates.io) but does
**not** use its `Simulator`/`ShockSpec` for aviation state. Seven's `seven-sim`
uses seeded `rand::StdRng` with recorded seeds for failure injection; belief
updating composes `themql_estimation::Ekf` (theMQL's safety-critical filter,
the origin prv's EKF derives from).

If a genuinely generic primitive later lands in prv (e.g. a seeded RNG harness
or covariance utilities independent of `State`), Seven adopts it. Copying prv
code into Seven is forbidden (spec §15 "do not copy, extract generically").

**Consequences**: the `prv-monte-carlo` dep is currently inert compile-side;
it documents intent and keeps the composition surface visible. The dep is a
leaf in the graph and cannot leak economic semantics.
