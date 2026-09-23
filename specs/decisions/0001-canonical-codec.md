# Decision 0001 — Canonical codec

**Status**: decided 2026-09-23.

**Context**: spec §4/§6 require deterministic canonical bytes for hashing, identity, persistence, and transport. Decision must be reproducible from the workspace.

**Options**: postcard vs Borsh.

**Measurement** (`cargo bench -p seven-core --bench canonical_codec`, this machine, 2026-09-23):

- postcard: 36 bytes for CanonicalState
- Borsh:    88 bytes for the same shape
- Latency:  postcard ≈ 267 ns, Borsh ≈ 30 ns (both far below any LoRa budget)

**Decision**: **postcard**. Rationale:
1. 36 vs 88 bytes — LoRa payload budgets (S11) favour compactness.
2. `themql_core::FormatTag::Postcard` already exists upstream — zero impedance.
3. serde integration keeps domain types clean.
4. Latency is a non-issue for the spec's workloads; byte size is not.

**Consequences**: any future change to postcard's wire format or the
`CanonicalState` field order *changes evidence identity*. The
`canonical_bytes()` tests pin the current format via round-trip + determinism
property tests.
