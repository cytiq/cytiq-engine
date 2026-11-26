# Contributing

## Project Scope

This repository is a Rust workspace named `cytiq-engine`, providing the `cytiq` binary. Keep contributor guidance specific to this codebase. Do not copy workflow, database, Python, or GitHub automation instructions from other projects unless those tools are added here.

## Project Structure

- `crates/cytiq-*/` contain the library crates behind each command (`cytiq-fcs` is the shared FCS reader/writer, `cytiq-density` the shared density primitives, `cytiq-publish` the output publication every command writes through: a failed run publishes none of its outputs).
- `parity/` contains the scripts and report that compare results against the upstream R/Python tools.
- `data/` contains checked-in sample and research assets such as `data/data.fcs` and `data/CD22/`.

The repository should consist of crates only. Do not add a root-level `src/`; new binaries or libraries belong under `crates/`.

## Development Commands

Use `just` as the primary command runner. The recipes wrap the same Cargo commands used in CI.

`mise.toml` pins every development tool and `mise.lock` records the download and
checksum of each. `just mise` installs the mise release that `min_version` names
outside `PATH`, under `~/.local/share/cytiq/mise/`, then those tools; it needs only
`just`, `bash` and `curl`, and is safe to rerun. Every other recipe runs through
that mise's `mise exec`, so it uses the pinned versions without shell activation
and ignores another mise on `PATH`; until `just mise` has run, recipes fail with
“could not find the shell `mise-cytiq`”. To change a version, edit `mise.toml`, run
`"$(just --evaluate MISE)" lock` (the pinned mise) and commit both files; CI installs the same versions through
`cytiq-ci`'s mise action, and each job enables only the tools it needs.
Docker, a C/C++ toolchain and, for parity, R are prerequisites: parity compiles
Bioconductor and git packages against the system R, which mise cannot provide,
so `just parity-setup` checks it against `CYTIQ_R_VERSION`. Fuzzing uses the
nightly in `CYTIQ_FUZZ_TOOLCHAIN`, which `just fuzz` installs on first use.

- `just mise` installs mise and the pinned tools; run it first.
- `just --list` shows every recipe; `just verify` is the local quality gate.
- Benchmarks: [`performance/`](performance/README.md). Lakehouse checks:
  [`parity/lakehouse/`](parity/lakehouse/README.md).

Before opening a pull request, run `just verify`. CI enforces the equivalent checks on every pull request with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` and the packaging check (job `quality`) and `cargo test --workspace --locked` under coverage instrumentation (job `test`). The release build and the Docker image (`.github/workflows/package.yml`, job `docker`) take about six minutes, so they run only on pull requests that change manifests, the lockfile, `build.rs`, `cytiq-classify`, its fixtures or `docker/`, plus weekly and on demand; run `just docker-build` locally when a change can affect packaging in other ways.

Releases go through `.github/workflows/cd.yml`. Read [releasing](docs/releasing.md) before cutting a release
or changing its workflows. Keep the reusable workflows pinned to a released `cytiq-ci` tag, and never move an
existing version tag.

The lakehouse and fuzz workflows, CI telemetry and `CI_LIGHT_RUNNER` are described in
[quality checks](docs/quality-checks.md#ci-extras). Run a fuzz target with `just fuzz <crate> <target> [seconds]`;
turn a crash into a regression test as `crates/cytiq-fcs/CONTRIBUTING.md` describes.

The dataset contract (`docs/dataset-contract.md`) is this repository's own:
`crates/cytiq-population/contracts/dataset/v1.lock.json` pins its published
schemas and identity vectors, and `data/fixtures/dataset/sha256.json` pins the
reference dataset, which `cytiq-engine`'s `dataset_fixtures` test rebuilds
with the real commands. A released contract version never changes.

The upstream contracts are pinned and checked offline by `cargo test`: `crates/cytiq-population/contracts/cytiq-tree.lock.json` holds the sha256 of each vendored cytiq-tree schema, and `data/fixtures/classify/cytiq-ml.lock.json` the sha256 of each cytiq-ml bundle fixture. When bumping either, run `just contracts-upstream`, which compares the cytiq-tree pin with `git show` in a sibling checkout (`$CYTIQ_TREE_DIR`, default `../cytiq-tree`) and checks that a recorded cytiq-ml revision exists (`$CYTIQ_ML_DIR`). Hosted CI has no sibling checkouts, so this check is local.

Scientific validation has four layers: unit tests, numerical parity, real acquisitions and population retention. The required PR `test` job verifies SHA-256-pinned fixtures and runs the fast scientific regressions with the workspace tests; `just scientific` runs that subset locally, and `just scientific-metrics` (pinned Python environment) every scientific-metrics unit test plus the AF estimator oracle. `.github/workflows/scientific.yml` selects full pinned R/Python parity for numerical changes, with weekly and manual runs. Release delivery depends on full scientific validation, including every FCS in the pinned CyMAE version. Missing required inputs fail; a skipped optional domain is explicitly reported. Reference environments and small real-data inputs are cached, with saves restricted to main; the 16 GB CyMAE corpus is not cached or uploaded.

Run `just parity-setup`, `just parity`, `just parity-real`, `just parity-cytonorm` and `just parity-cytonorm-real` locally. Python wheels and R source archives are pinned by versions and SHA-256. Outputs default to ignored `target/scientific/`. Existing golden fixtures are never generated by tests or CI: an upstream or dataset update requires separate scientific review. Do not regenerate fixtures or widen tolerances to make an implementation pass. See [scientific experiments](parity/scientific/README.md) for independent oracles, dataset provenance, limitations and paired performance measurements.

## crates.io publication

Every workspace crate is published and shares the workspace version. Depend on library crates through
`[workspace.dependencies]` in the root `Cargo.toml`, with a version equal to the workspace version.
`just package-check` and the `quality` job reject a mismatch, a crate without `description` or `license`, and an
internal dependency missing from `[workspace.dependencies]`. A new crate is published once by hand before trusted
publishing can take over: see [releasing](docs/releasing.md#cratesio-publication).

## Coverage and CRAP

Run `just coverage`, then `just crap`; see [quality checks](docs/quality-checks.md#coverage-and-crap).
CI never updates `.github/crap-baseline.json` or `.github/crap-baseline-run.json`. Refresh them only in a
reviewed PR, from the `crap-report` of a successful Linux `test` job, and never from a delta report, a top-30
selection or a local macOS report.

## Mutation testing

Run `just mutants [crate]`; see [quality checks](docs/quality-checks.md#mutation-testing). Keep the selection in
`.cargo/mutants.toml` in sync when extracting or renaming helpers: a move must not remove mutation coverage.
Exclude only demonstrably equivalent mutants; do not skip whole functions to obtain a passing report. Test library
regressions in that library, not only through the CLI.

## Coding Style

Follow Rust 2024 conventions and let `rustfmt` control formatting.

Keep `cytiq-engine` thin and move reusable logic into a focused library crate. Add brief comments only where the intent is not obvious from the code.

## Testing

New logic should include tests. Put unit tests near the implementation with `#[cfg(test)]`, and place broader scenarios in `tests/`. Name tests by observable behavior, for example `loads_cd22_sample_data` or `fails_when_input_is_missing`.

## Commits and Pull Requests

Use Conventional Commits with a short imperative subject, for example `fix(fcs): refuse files with more than one data set`, or `feat!: …` for a breaking change. `cliff.toml` leaves any other commit out of the changelog.

Pull requests should include:

- a short summary of the change
- the reason for the change
- the commands used for verification
- notes about any updates to `data/`

## Crate Guidance

Every crate under `crates/` has its own `README.md` (purpose, public API, usage) and `CONTRIBUTING.md` (module layout, invariants, tests), plus `AGENTS.md` and `CLAUDE.md` pointers for coding agents. Read the crate's `CONTRIBUTING.md` before changing it, and keep its `README.md` in sync when the public API, output format or CLI flags change.

## Data Handling

Treat `data/` as intentional repository content. Do not commit secrets, temporary exports, or generated artifacts unless they are required for reproducible development or tests.

## Reusable execution

Global `--reuse skip` reuses only matching v3 provenance with every output hash
verified under a destination lock. `--reuse reject` refuses stale/damaged/unrecorded
outputs; `--reuse recompute` keeps ordinary overwrite behavior. See the engine
README for the policy table. QC outcomes retain their original exit code on reuse.

Fingerprint config is semantic by default. Only overwrite/debug_dir/dry_run/threads
are recursively removed; classify adds effective thread count as parallelism.
New algorithm inputs and options must enter the recorder before begin. When these
rules change, bump FINGERPRINT_VERSION and test invalidation. Paths matter;
outputs are re-hashed. Keep dependency installations locked.
