# Mētis Avionics/Seven

**Seven** is an experimental distributed aviation-resilience research system:
independently operating nodes maintain a deterministic, provenance-aware
probabilistic representation of physical state while communications degrade
(loss, partition, delay, duplication, reordering).

> **Safety boundary (spec §21).** Seven is a research platform. It must never
> issue ATC clearances, command aircraft, provide separation assurance,
> autonomously direct aircraft, claim certification, or present experimental
> state as authoritative aviation state.

## Architecture

Seven composes existing Mētis primitives rather than recreating them:

```text
Physical observation
        ↓
Canonical state            ← seven-core      (S02)
        ↓
Evidence identity+provenance ← seven-evidence (S03/S15)
        ↓
Bayesian belief            ← seven-belief    (S04)
        ↓
Seven domain state
        ↓
theMQL (Message projection) ← seven-mql      (S06)
        ↓
Transport adapters
        ├── QUIC           (Phase 8)
        ├── LoRa           (Phase 8)
        └── Lossy simulated ← seven-sim      (S13/S17)
```

Orthogonal substrates:

```text
Seven state
    ├── theSix policy/cache substrate  ← seven-six   (S07)
    └── HelixDB persistent graph       ← seven-helix (S05/S09)
```

## Crates (published as `seven-*`)

| Crate | Role |
|---|---|
| `seven-core` | Canonical deterministic state; quaternion hemisphere rule; quantization |
| `seven-evidence` | blake3 evidence identity; ed25519 signing; provenance |
| `seven-belief` | Deterministic Bayesian updates; duplicate/conflict handling |
| `seven-helix` | HelixDB evidence-graph persistence (Rust SDK) |
| `seven-replay` | Deterministic replay (event log + fingerprint) |
| `seven-sim` | Seeded failure injection (loss/dup), scenario drivers |
| `seven-mql` | theMQL `Message` projection; simulated lossy transport |
| `seven-six` | Aviation cache policy over `thesix` |
| `seven-bin` | End-to-end reference pipeline (not for operational use) |

All dependencies come from crates.io (`themql-core/-estimation 0.1`,
`thesix =0.2.3`, `prv-monte-carlo 0.2.1`, `helix-db 3.0.0`); see
`specs/decisions/` for the sourcing rationale (`0003-themql-sourcing.md`).

## Verification

```bash
cargo nextest run --workspace --no-fail-fast          # 68 tests
cargo clippy --workspace --all-targets -- -D warnings  # clean
SEVEN_HELIX_LIVE=1 cargo nextest run -p seven-helix    # live podman dev
```

`specs/VERIFICATION.md` maps every spec-§22 invariant to a named test.

## Try it

```bash
helix start dev                    # podman instance (see AGENTS.md)
cargo run -p seven-bin             # deterministic evidence→belief demo
```

## License

MIT (matches the rest of Mētis Avionics).
