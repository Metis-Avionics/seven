# Decision 0003 — theMQL sourcing

**Status**: decided 2026-09-23 (revises the earlier vendor-first plan).

**Plan earlier**: vendor themql-core + themql-estimation from the cargo git
checkout (fork `RAliane-REBORN/theMQL` @ `64a6923`) because they were believed
to be unpublished.

**Corrected finding**: all needed theMQL crates **are on crates.io** as
`0.1.0` (published 2026-09-14 from the Mētis org; Mētis owns theMQL so crates.io
consumption is the supported path). Verified against the crates.io API.
`themql-core`, `themql-estimation` consumed at `0.1` — the same code as the
git checkout (checked by diffing the two trees).

**Decision**: depend on crates.io theMQL directly; no vendor directory. The
reproducibility story is crates.io + Cargo.lock's checksum pinning, which is
the standard Mētis pattern (prv and NS-Pro both use crates.io versions).

**Consequences**: workspace builds without network to GitHub; the whole graph
comes from crates.io.
