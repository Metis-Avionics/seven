# Working with this HelixDB project

This project uses [HelixDB](https://docs.helix-db.com). `helix.toml` holds the project
config; `.helix/` holds instance state (gitignored). The full docs index for agents is at
<https://docs.helix-db.com/llms.txt>.

## Workflow

```bash
helix start dev                                  # start the local instance (Podman here)
helix query dev --file examples/request.json     # send a query JSON request
helix query dev -e 'readBatch().varAs("ev", g().nWithLabel("Evidence").count()).returning(["ev"])' --host 127.0.0.1
helix status                                  # instance state
helix logs dev                                   # container logs
helix stop dev                                   # stop (in-memory: data lost; see notes below)
```

There is no `helix compile` or `helix check` — queries are validated by the running
instance. `helix query -e` evaluates a TypeScript DSL expression and needs Node 20+.

### Runtime notes (Seven-specific)

- `helix.toml`: `container_runtime = "podman"` (Docker is absent on this host).
- `storage = "memory"` for the dev instance. Helix CLI 3.2.0's `--disk` path
  uses CLI-managed MinIO whose `minio/minio:latest` image short-name fails
  rootless podman. For real persistence use `--storage-uri` with an external
  S3-compatible store (see `specs/s05-s09-helix.toml` notes) or revisit once
  upstream unifies the podman disk path.
- Endpoint default is `http://127.0.0.1:6969` (IPv4). `localhost` can resolve
  to `::1` and pasta's forwarding is v4-only — the IPv4 literal is intentional.

## Project layout (v0.1)

```text
crates/seven-core      # S01/S02 — canonical state, quaternion, quantization
crates/seven-evidence  # S03/S15 — EvidenceId = blake3(canonical), provenance, ed25519
crates/seven-belief    # S04    — deterministic Bayesian engine (linear-Gaussian for
                       #          independent evidence; composes themql-estimation EKF)
crates/seven-helix     # S05/S09 — HelixDB graph + checked-in query suite
crates/seven-replay    # S12    — EventLog + replay fingerprint
crates/seven-sim       # S13/S17 — LossyChannel loss/dup injection, seeded
crates/seven-mql       # S06    — Evidence ⇄ themql Message projection; Payload::Postcard
crates/seven-six       # S07    — AviationCachePolicy over thesix (=0.2.3)
crates/seven-bin       # end-to-end runnable pipeline demo
specs/                 # authoritative v0.1 specs + VERIFICATION.md + decisions/
```

Composition rule (§2): **Seven never reimplements theMQL, theSix, prv, or
HelixDB internals.** theMQL 0.1 (core + estimation) and `prv-monte-carlo` 0.2.1
come from crates.io (decision `specs/decisions/0003-themql-sourcing.md`).

## Verification gates

```bash
cargo nextest run --workspace --no-fail-fast          # 65 tests, must be green
cargo clippy --workspace --all-targets -- -D warnings # must be silent
SEVEN_HELIX_LIVE=1 cargo nextest run -p seven-helix   # requires helix dev up
cargo bench -p seven-core --bench canonical_codec     # codec decision evidence
```

`specs/VERIFICATION.md` maps every spec §22 invariant to a named test. Do not
merge any change that breaks that mapping or weakens an invariant to silence a
lint.

## If `helix` is not installed

macOS and Linux:

```bash
curl -sSL "https://install.helix-db.com" | bash
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/HelixDB/helix-db/main/crates/cli/install.ps1 | iex
```

## If the container runtime is unavailable

`helix start` needs a working Docker or Podman:

- macOS: `brew install --cask docker`, or `brew install colima docker && colima start`.
- Linux: `curl -fsSL https://get.docker.com | sh`, or `apt-get install -y podman` and set
  `container_runtime = "podman"` under `[project]` in `helix.toml`.
- Daemon installed but stopped: `open -a Docker` (macOS) or `sudo systemctl start docker` /
  `sudo dockerd &` (Linux). `helix start` also tries this automatically.
- Restricted sandboxes without root usually cannot run containers. Use a host where Docker
  works, or point queries at a reachable instance with `helix query --host <h> --port <p>`.

## Query syntax

- TypeScript DSL: <https://docs.helix-db.com/database/querying-guide/overview>
- Dynamic JSON request shape: <https://docs.helix-db.com/cli/command-reference/query>
- Seven's checked-in suite lives in `crates/seven-helix/src/lib.rs` (`queries` module).
