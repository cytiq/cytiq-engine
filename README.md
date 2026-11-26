# cytiq-engine

Rust workspace for flow-cytometry tooling: FCS I/O, compensation and spectral
unmixing, event filtering, quality control, batch normalization, gating, clustering,
classification and vector diagnostic plots.

[Specimen processing](crates/cytiq-specimen/README.md) adds versioned acquisition
selection, population observations and panel reconciliation above those stages.
It supports conventional flow, spectral gating and existing CyTOF classifiers.
`specimen compare` checks native results with explicit tolerances;
`specimen report` exports results and comparisons as offline HTML, CSV and JSON.

## Pipeline

Solid arrows carry FCS files and matrices; dotted arrows carry
[per-event masks](crates/cytiq-mask/README.md), annotations and summary artifacts.

```mermaid
flowchart TB
    subgraph inputs["Inputs"]
        direction LR
        manifest[/"run manifest<br/>run.yaml"/]
        controls[/"single-stain controls"/]
        raw[/"raw sample FCS"/]
    end

    subgraph checks["Checks before computation"]
        direction LR
        preflight["preflight<br/>cytiq-qc::preflight"]
        meta["meta<br/>cytiq-inspect"]
        repair["repair (plan)<br/>cytiq-repair"]
        stats["stats (technical QC)<br/>cytiq-inspect"]
        cytoscan["cytoscan (cohort)<br/>cytiq-cytoscan"]
    end

    subgraph signal["Compensation or unmixing"]
        autospill["autospill / autospread<br/>cytiq-autospill"]
        compensate["compensate<br/>cytiq-compensate"]
        spillqc["spillqc (matrix check)<br/>cytiq-spillqc"]
        autospectral["autospectral<br/>cytiq-autospectral"]
    end

    transform["transform<br/>cytiq-transform"]

    subgraph events["Event filtering"]
        peacoqc["peacoqc<br/>cytiq-peacoqc"]
        clean["clean<br/>cytiq-clean"]
        combine["mask combine<br/>cytiq-mask"]
    end

    qc["qc (panel policy)<br/>cytiq-qc"]
    cytonorm["cytonorm train / apply<br/>cytiq-cytonorm"]

    subgraph analysis["Analysis"]
        direction LR
        gate["gate<br/>cytiq-gating"]
        flowsom["flowsom<br/>cytiq-flowsom"]
        classify["classify<br/>cytiq-classify"]
    end

    inputs --> checks
    checks ~~~ signal
    preflight ~~~ meta ~~~ stats ~~~ cytoscan
    gate ~~~ flowsom ~~~ classify

    plot["plot (vector SVG, any stage)<br/>cytiq-plot"]

    controls --> autospill
    autospill -->|"compensation.csv"| compensate
    raw -->|"conventional"| compensate
    raw -->|"stained samples"| spillqc
    autospill -.->|"candidate matrix"| spillqc
    controls --> autospectral
    raw -->|"spectral"| autospectral

    compensate -->|"comp.fcs"| transform
    autospectral -->|"unmixed.fcs"| transform
    transform -->|"t.fcs"| peacoqc
    raw -->|"raw scatter and viability"| clean

    compensate -.->|"margin_pass"| combine
    autospectral -.->|"margin_pass"| combine
    peacoqc -.->|"peacoqc_pass"| combine
    clean -.->|"non_debris, singlets, live"| combine

    combine -.->|"analysis mask"| qc
    combine -.->|"analysis mask"| analysis
    transform -->|"t.fcs"| analysis
    peacoqc -->|"QC-filtered transformed FCS"| cytonorm
    cytonorm -->|"norm.fcs + pinned model ID"| analysis
    raw -->|"before QC"| plot
    transform -->|"t.fcs"| plot
    combine -.->|"analysis mask, whatever qc says"| plot
    analysis -.->|"gates, clusters, classes"| plot
    manifest -.-> specimen_plan["specimen plan<br/>cytiq-specimen"]
    analysis -.->|"membership / predictions"| population["population observe<br/>cytiq-population"]
    qc -.->|"technical QC"| population
    specimen_plan -.-> population
    population -.-> specimen_result["specimen run<br/>reconciliation, QC, JSON / Parquet"]
    specimen_plan -.-> specimen_result
```

See the [pipeline guide](crates/cytiq-engine/PIPELINE.md) for a complete command
chain and signal-state checks.

## Repository layout

- [`crates/cytiq-engine/`](crates/cytiq-engine/README.md) — the `cytiq` binary; argument parsing and command dispatch.
- [`crates/cytiq-*/`](crates/) — library crates with reusable algorithms and workflows.
- [`data/fixtures/`](data/fixtures/) — synthetic test data and reference outputs; dataset READMEs describe their contents and generation.
- [`parity/`](parity/) — validation against upstream R/Python tools.
- [`docker/Dockerfile`](docker/Dockerfile) — container build.

Each crate has a `README.md` for usage and public APIs, and a `CONTRIBUTING.md`
for implementation details and invariants. New binaries and libraries belong
under `crates/`.

## Documentation

- [CLI commands and shared conventions](crates/cytiq-engine/README.md)
- [Pipeline, stage order and signal-state checks](crates/cytiq-engine/PIPELINE.md)
- [Dataset contract: `cytiq.events.v1`, dataset manifest, identities and privacy](docs/dataset-contract.md)
- [Papers and upstream implementations](CITATIONS.md)
- [Contributor guidelines](CONTRIBUTING.md)

## Development

Requires `just`, `curl` and a C/C++ toolchain (Xcode Command Line Tools, or
`g++` on Linux). `just mise` installs [mise](https://mise.jdx.dev) and the
tools that [`mise.toml`](mise.toml) pins: Rust 1.99.1 with `rustfmt` and Clippy,
Python, uv and the coverage, mutation and fuzzing tools. Every recipe runs with
those versions; shell activation is optional. Docker and, for parity, R 4.5.3
are installed separately. The first build needs
HTTPS access to download ONNX Runtime; it is linked statically into `cytiq`.
See [classification build details](crates/cytiq-classify/README.md).
Classification accepts legacy binary bundles and v2–v4 vocabulary-aware multiclass
bundles, including a profile for stored FlowCyt author exports.

```bash
just mise               # once, and after mise.toml changes: pinned tools
just build              # debug workspace build
just run --help         # run the CLI from source
just build-release      # optimized binary: target/release/cytiq
just test               # workspace tests
just verify             # format, cargo check, Clippy and tests
```

Run `just verify` before opening a pull request. Use `just --list` for all
recipes, or target one crate with `cargo test -p cytiq-fcs`.

## Algorithm validation

For changes that can affect scientific results, run both parity suites locally.
Setup requires R and `uv` and creates an isolated environment in `parity/.env`:

```bash
just parity-setup       # once: install upstream R/Python tools
just parity             # synthetic datasets
just parity-real        # real acquisitions
just benchmark-spillqc  # what spillqc detects on real data (after parity-real)
```

Results and known differences are in [`parity/REPORT.md`](parity/REPORT.md);
[real-data documentation](parity/real/README.md) covers dataset sources.
Violations of [`parity/tolerances.json`](parity/tolerances.json) or
[`parity/real/tolerances.json`](parity/real/tolerances.json) fail the run.

CI runs the Rust quality checks and tests on every pull request, and the Docker
checks when packaging can change. It validates the
[end-to-end pipeline](data/fixtures/pipeline/README.md) and
[upstream parity smoke fixtures](data/fixtures/parity/README.md) without R;
the full parity suites run locally. Regenerate reference fixtures only for
intentional generator, dataset or upstream-version changes; see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Install from source with Cargo

```bash
cargo install --git https://github.com/cytiq/cytiq-engine --tag 0.5.0 --locked cytiq-engine
```

For an existing `cargo install cytiq-cli` installation, add `--force` to replace
the executable. See [crates.io publication](CONTRIBUTING.md#cratesio-publication)
for registry setup.

## Docker

```bash
docker pull ghcr.io/cytiq/cytiq-engine:0.5.0
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/data" ghcr.io/cytiq/cytiq-engine:0.5.0 autospill \
  --controls data/fixtures/autospill/controls.csv \
  --input-dir data/fixtures/autospill --output compensation.csv
```

The image runs as an unprivileged user (UID 10001), with `/data` as its working
directory. `--user` makes outputs in the mounted directory belong to you.
For source development, `just docker-build` builds `cytiq:dev`.

Releases are published as `ghcr.io/cytiq/cytiq-engine:<version>` (and `:latest`)
for `linux/amd64` and `linux/arm64`, with SBOM and provenance attestations, so
`docker pull` gets a native image on Apple silicon too. Pin the digest when you
deploy, e.g. in `cytiq-argo`'s WorkflowTemplates. Pushing an `X.Y.Z-rcN` tag
publishes `:X.Y.Z-rcN` and opens the changelog PR; merging it tags `X.Y.Z` and
publishes the release image (`.github/workflows/cd.yml`).
