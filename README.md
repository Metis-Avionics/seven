# Mētis Avionics/Seven

Seven is an experimental aviation-resilience research platform investigating whether selected ATC functions can be decentralised onto inexpensive edge compute and existing communications infrastructure, forming an independent peer-to-peer network capable of continuing to exchange and maintain degraded aviation state when conventional centralised infrastructure is impaired.

## Research objective

The central research question is:

> Can a decentralised network of low-cost edge nodes provide useful, trustworthy aviation state without depending on a single central infrastructure path?

Seven explores this question under progressively degraded conditions. Nodes independently observe, canonicalise, authenticate, persist, and update probabilistic representations of physical state, then exchange that state over transport mechanisms that may experience loss, delay, duplication, reordering, partition, or complete communication failure.

The research is therefore concerned with the architecture and failure behaviour of decentralised aviation infrastructure:
```
                 Conventional ATC infrastructure
                           │
                     normal operation
                           │
              ┌────────────┴────────────┐
              │                         │
              ▼                         ▼
        centralised path          Seven edge nodes
                                      │
                         ┌────────────┼────────────┐
                         ▼            ▼            ▼
                       Node A       Node B       Node C
                         │╲          ╱│╲          ╱│
                         │ ╲        ╱ │ ╲        ╱ │
                         │  ╲      ╱  │  ╲      ╱  │
                         └─── P2P / degraded network ───┘
                                      │
                                      ▼
                             shared probabilistic
                                  aviation state
```
The underlying hypothesis is that useful resilience does not necessarily require duplicating the entire conventional ATC infrastructure. Instead, some functions may be decomposed into smaller services that can execute locally on commodity edge hardware and exchange state directly with neighbouring nodes.

Seven therefore studies the trade-off between:
```
- cost — commodity rather than specialised infrastructure;
- locality — computation performed close to observations;
- decentralisation — removal of dependence on a single central path;
- connectivity — operation across heterogeneous and degraded links;
- state quality — provenance, uncertainty, conflicts, and evidence freshness;
- failure behaviour — measurable degradation rather than an assumed binary available/unavailable model.
```
The intended property is graceful degradation of information and capability, not preservation of normal ATC capacity or authority.

> Safety boundary (spec §21). Seven is a research platform. It must never
issue ATC clearances, command aircraft, provide separation assurance,
autonomously direct aircraft, claim certification, or present experimental
state as authoritative aviation state.

Seven does not currently constitute an operational ATC system. Its purpose is to provide an experimental environment in which the feasibility, performance, cost, consistency, and failure modes of decentralised aviation infrastructure can be measured.