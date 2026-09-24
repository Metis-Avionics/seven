# Mētis Avionics/Seven

Seven is an experimental distributed aviation-resilience research platform investigating whether an independent, decentralised network can preserve a degraded but useful representation of aviation state when conventional ATC communications or supporting infrastructure become unavailable or severely degraded.

The research premise is not that a backup network can reproduce the safety or capacity of the primary ATC system. Rather, Seven explores whether independently operating nodes can maintain some degree of shared, provenance-aware situational state under degraded communications, providing a foundation for studying graceful degradation and resilience in aviation systems.

Seven models physical observations as deterministic, provenance-aware probabilistic state and distributes that state across independently operating nodes while explicitly modelling communications failure modes including loss, partition, delay, duplication, and reordering.

The intended research architecture is therefore:

              PRIMARY ATC / AVIATION INFRASTRUCTURE
                         │
                  degradation/failure
                         ↓
              ┌──────────────────────┐
              │       SEVEN          │
              │ independent backup   │
              │ research network     │
              └──────────────────────┘
                         │
              degraded shared state
              + provenance + belief
                         │
                         ↓
             degraded resilience layer

Seven is specifically concerned with the failure regime between normal operation and complete loss of useful information. The objective is to quantify what information can remain trustworthy, how uncertainty evolves, and how independently maintained state behaves when network conditions deteriorate.

> Safety boundary (spec §21). Seven is a research platform. It must never
issue ATC clearances, command aircraft, provide separation assurance,
autonomously direct aircraft, claim certification, or present experimental
state as authoritative aviation state.

Seven therefore does not attempt to replace ATC, become an operational ATC system, or establish an alternative source of authoritative aviation state. Its purpose is to provide an experimental environment for studying the engineering properties of an independent, decentralised resilience mechanism operating beneath degraded conditions.

Architecture
```
Seven composes existing Mētis primitives rather than recreating them:

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
```
Seven state
    ├── theSix policy/cache substrate  ← seven-six   (S07)
    └── HelixDB persistent graph       ← seven-helix (S05/S09)
```