# Contributing

## Project Scope

This repository is a Rust workspace named `cytiq-engine`, providing the `cytiq` binary. Keep contributor guidance specific to this codebase. Do not copy workflow, database, Python, or GitHub automation instructions from other projects unless those tools are added here.

## Project Structure

- `crates/cytiq-engine/` contains the CLI binary entry point.
- `crates/cytiq-*/` contain the library crates behind each command (`cytiq-fcs` is the shared FCS reader/writer, `cytiq-density` the shared density primitives, `cytiq-publish` the output publication every command writes through: a failed run publishes none of its outputs).
- `parity/` contains the scripts and report that compare results against the upstream R/Python tools.
- `data/` contains checked-in sample and research assets such as `data/data.fcs` and `data/CD22/`.
- `docker/Dockerfile` is the place for container build logic.

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
- `just run` starts the `cytiq` binary from the `cytiq-engine` package.
- `just build` builds the workspace in debug mode.
- `just build-release` builds an optimized release binary.
- `just check` runs `cargo check --workspace`.
- `just fmt` formats the codebase with `rustfmt`.
- `just lint` runs Clippy with warnings denied.
- `just test` runs the workspace test suite.
- `just coverage` runs the CI test suite with LLVM line coverage.
- `just crap` reports CRAP and baseline deltas from that coverage run.
- `just crap-check` tests report validation without coverage tools.
- `just mutants` runs mutation tests for the CRAP remediation functions;
  `just mutants cytiq-fcs` selects one of the eleven supported crates.
- `just package-check` checks crates.io metadata and internal dependency
  versions and packages every crate without uploading anything.
- `just verify` runs the standard local quality gate.
- `just performance-check` checks the algorithm benchmark harness; `just performance --help`
  describes the paired CLI benchmarks in [`performance/`](performance/README.md).
- `just docker-build` verifies container changes.

Before opening a pull request, run `just verify`. CI enforces the equivalent checks on every pull request with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` and the packaging check (job `quality`) and `cargo test --workspace --locked` under coverage instrumentation (job `test`). The release build and the Docker image (`.github/workflows/package.yml`, job `docker`) take about six minutes, so they run only on pull requests that change manifests, the lockfile, `build.rs`, `cytiq-classify`, its fixtures or `docker/`, plus weekly and on demand; run `just docker-build` locally when a change can affect packaging in other ways.

Releases go through `.github/workflows/cd.yml`, which calls the reusable changelog and Docker workflows of [`cytiq-ci`](https://github.com/cytiq/cytiq-ci).
1. Pushing an `X.Y.Z-rcN` tag opens a `chore/update-changelog-X.Y.Z` PR with `CHANGELOG.md`, `RELEASE_NOTES.md` (git-cliff, `cliff.toml`) and the workspace version bump in `Cargo.toml`/`Cargo.lock`. It also publishes `ghcr.io/cytiq/cytiq-engine:X.Y.Z-rcN` for `linux/amd64` and `linux/arm64`, each built on a native runner.
2. Merging that PR pushes the signed `X.Y.Z` tag and the GitHub Release, and publishes `:X.Y.Z` and `:latest` built from the merge commit.
3. The same merge dispatches `.github/workflows/crates-publish.yml` on the `X.Y.Z` tag, which publishes all 24 crates to crates.io (see [crates.io publication](#cratesio-publication)); release candidates are never published there.

Images record that commit in `CYTIQ_BUILD_COMMIT` and carry SBOM and provenance attestations. The release jobs need the `GPG_PRIVATE_KEY`, `GPG_PASSPHRASE`, `GPG_FINGERPRINT` and `GH_TOKEN` secrets.

Full scientific validation checks out the resolved release commit before any
publication. The local release job signs that exact commit and refuses to move
an existing version tag. A later change to `main` cannot enter a release that
has already passed validation.

Docker publication opts into the platform-specific registry layer cache in `cytiq-ci` 0.4.0. Cache export failures
do not hide build failures or block a successful publication. `docker/Dockerfile` compiles the dependencies with
cargo-chef in a layer of their own, keyed by a recipe of the manifests and lockfile with the workspace version masked.
Source changes and release version bumps reuse that layer; any dependency change in `Cargo.lock` rebuilds it. The
workspace crates and the fat-LTO link of `cytiq` always rebuild. The `package.yml` job reads the amd64 release cache
and never writes it. Keep the reusable workflow pinned to a released tag after rollout validation.
The changelog and Docker calls use `cytiq-ci@0.4.0`, which includes the workspace-version fix from
`cytiq-ci#24`.
If GitHub cannot resolve an annotated release tag, pin its peeled commit SHA
and retain the version in a comment; this keeps the same reviewed workflow
code without rewriting the upstream release tag.

Rust checks keep their `quality` and `test` names and parallel runners. Optional `ci-cache-*` artifacts record
exact Cargo cache hits for the organization usage report; telemetry failures do not affect correctness checks.
The manual **Rust CI benchmark** workflow measures combined/separate checks on cold and warm ephemeral targets.
It never uploads build caches, since extra target caches previously evicted main's cache. Its warm measurements
describe repeated Cargo invocations, not Actions cache-transfer savings. Compare runner setup and summed job
durations too; the experiment does not alter required checks.

`CI_LIGHT_RUNNER` defaults to `ubuntu-24.04` and controls only release-version resolution. Trial `ubuntu-slim`
there after compatibility testing; retain it only when cost falls without increasing required-check P95 by
more than 10%. It never controls Docker or Rust runners.

`.github/workflows/fuzz.yml` fuzzes the FCS reader and the spillover parser (`cytiq-fcs`), the population JSON/YAML document and observation-table readers (`cytiq-population`) and specimen result comparison (`cytiq-specimen`): one minute a target on pull requests that change one of those crates, ten minutes weekly, and on demand. Run a target locally with `just fuzz <crate> <target> [seconds]` (`just fuzz-fcs` remains for `cytiq-fcs`). It is not a required check. A crash uploads its input as an artifact; turn it into a regression test as `crates/cytiq-fcs/CONTRIBUTING.md` describes. `cargo test` replays the saved regressions on every run.

The upstream contracts are pinned and checked offline by `cargo test`: `crates/cytiq-population/contracts/cytiq-tree.lock.json` holds the sha256 of each vendored cytiq-tree schema, and `data/fixtures/classify/cytiq-ml.lock.json` the sha256 of each cytiq-ml bundle fixture. When bumping either, run `just contracts-upstream`, which compares the cytiq-tree pin with `git show` in a sibling checkout (`$CYTIQ_TREE_DIR`, default `../cytiq-tree`) and checks that a recorded cytiq-ml revision exists (`$CYTIQ_ML_DIR`). Hosted CI has no sibling checkouts, so this check is local.

Scientific validation has four layers: unit tests, numerical parity, real acquisitions and population retention. The required PR `test` job verifies SHA-256-pinned fixtures and runs the fast scientific regressions with the workspace tests; `just scientific` runs that subset locally. `.github/workflows/scientific.yml` selects full pinned R/Python parity for numerical changes, with weekly and manual runs. Release delivery depends on full scientific validation, including every FCS in the pinned CyMAE version. Missing required inputs fail; a skipped optional domain is explicitly reported. Reference environments and small real-data inputs are cached, with saves restricted to main; the 16 GB CyMAE corpus is not cached or uploaded.

Run `just parity-setup`, `just parity`, `just parity-real`, `just parity-cytonorm` and `just parity-cytonorm-real` locally. Python wheels and R source archives are pinned by versions and SHA-256. Outputs default to ignored `target/scientific/`. Existing golden fixtures are never generated by tests or CI: an upstream or dataset update requires separate scientific review. Do not regenerate fixtures or widen tolerances to make an implementation pass. See [scientific experiments](parity/scientific/README.md) for independent oracles, dataset provenance, limitations and paired performance measurements.

## crates.io publication

Every workspace crate is published, because `cytiq-engine` depends on all the
others. They share the workspace version. Library crates are depended on through
`[workspace.dependencies]` in the root `Cargo.toml`, which gives crates.io the
version and local builds the path. Each such version must equal the workspace
version; `just package-check` and the `quality` job reject a mismatch, a crate
without `description` or `license`, and an internal dependency missing from
`[workspace.dependencies]`. The `cytiq-ci` release job pinned in `cd.yml`
bumps `[workspace.package]` and path-dependency requirements on the previous
version together, before running `cargo update --workspace`. The changelog PR
includes the modified manifests and lockfile; external dependency versions stay
locked. This order also supports minor and major releases: a `0.3.1` requirement
cannot resolve a workspace crate after it moves to `0.4.0`.

`.github/scripts/publish_crates.py` checks the versions against the tag, skips
versions already on crates.io and runs one `cargo publish --workspace`, which
orders the crates by their dependencies. The workflow verifies every package by
building it from its `.crate` archive first, then mints a 30-minute token through
trusted publishing (OIDC) and uploads without building again. A failed run is
safe to re-run, or to dispatch by hand on the tag (**Run workflow**, tag
`X.Y.Z`). The dispatch job stops if the tag does not point to the merge commit
the images were built from.

Trusted publishing cannot create a crate. The first version of a crate, and of
any crate added to the workspace later, is published once by hand. The
workflow refuses a release with such a crate before uploading anything:

1. Sign in to crates.io with GitHub, verify the email and create an API token
   with the `publish-new` and `publish-update` scopes, expiring soon.
2. From a clean checkout of the release tag, `cargo login`, then
   `just publish-crates-first`. It verifies every new crate, then publishes
   them. crates.io creates at most five new crates at once and then about one
   every ten minutes, so the first publication of all 24 takes over three
   hours; the script waits out each refusal and resumes. Ask help@crates.io to
   raise the limit beforehand to shorten it. Revoke the token afterwards.
3. For each crate, add a trusted publisher in **Settings → Trusted Publishing**:
   owner `cytiq`, repository `cytiq-engine`, workflow `crates-publish.yml`,
   environment `crates-io`.

The repository needs a `crates-io` environment (**Settings → Environments**)
whose deployment rule admits only release tags (`[0-9]*.[0-9]*.[0-9]*`; the
workflow itself rejects RC tags); required reviewers are optional. No registry secret is stored.

## Coverage and CRAP

`just mise` installs the versions CI uses (`mise.toml`), including the
toolchain's `llvm-tools`:

```sh
just mise
just coverage
just crap
```

`just coverage` uses `cargo llvm-cov show-env --sh` and runs the existing
`cargo test --workspace --locked`, the Python performance/report tests and the
`performance_support` Rust example tests once. Plain Cargo preserves doctests;
their own coverage instrumentation is not enabled on stable. Instrumented
dependencies live in `target/coverage`, separate from ordinary local builds.
CI caches this directory under the existing `test` job and saves only on `main`.
`just test` and `just verify` do not require coverage tools or collect coverage.

`just crap` reads `target/quality/lcov.info` without running tests. The
`crap-report` CI artifact contains the full absolute `crap.json`, the full
`crap-delta.json`, `summary.md`, LCOV and `coverage-run.json` (test success,
source revision, platform, compiler, coverage-tool version and input hashes).
Artifacts are retained for 35 days. The job summary lists per-crate metrics,
functions above 30, maximum CRAP, regressions and the top 30 functions.
The top-30 limit applies only to the summary; JSON and baseline contain every
analyzed function. CI uses the same scripts as the `just` recipes.

CRAP combines cyclomatic complexity and **line** coverage:
`CC² × (1 − coverage / 100)³ + CC`. At 100% coverage it still equals CC.
The threshold of 30 is an initial reporting choice, not a standard or an
active quality gate. `.cargo-crap.toml` keeps both `fail-above` and
`fail-regression` disabled. Scores and regressions do not fail CI; failed
tests, tool installation, invalid reports, missing baselines and zero
source/LCOV overlap do. Missing coverage remains visible and is scored as 0%.
Test code, examples, benches, fuzz targets and build scripts are not scored.
This metric complements scientific parity and mutation testing; it does not
replace them.

The reviewed Linux CI reference lives in `.github/crap-baseline.json`, with
its collection provenance in `.github/crap-baseline-run.json`. CI never updates
either file. To intentionally refresh it, download `crap-report` from a
successful Linux `test` job, review the full absolute report and changes to
the compiler, tool versions or exclusions, then copy its `crap.json` and
`coverage-run.json` to those two paths in a reviewed PR. Keep the absolute
report sorted by file and never use a delta report or a top-30 selection as
a baseline. Local macOS reports are useful for investigation but do not
replace the Linux reference. Record the originating Actions run in the PR.
For initial collection or a tool-version migration, the explicit
`bash .github/scripts/crap.sh baseline` command generates an absolute candidate
after `coverage`, without reading or modifying the checked-in reference;
use it in the Linux CI preparation commit, then restore `report` before merge.

## Mutation testing

`just mise` installs the version the mutation workflow uses (`mise.toml`):

```sh
just mise
just mutants
just mutants cytiq-gating
```

`.cargo/mutants.toml` selects the 15 functions addressed by the initial CRAP
analysis and their extracted helpers in ten crates, plus the validation,
reconciliation, anchor-reliability and comparison functions of
`cytiq-population` and `cytiq-specimen`. Keep this selection in sync
when extracting or renaming helpers: a move must not remove mutation coverage.
The runner rejects an empty selection and checks the unmodified tests before
injecting mutations into a scratch copy; the working tree is never mutated.
It uses `cargo test --locked` for the mutated crate, including its integration
tests and doctests. Library regressions must be tested in that library, not
only by CLI tests. Coverage instrumentation is not used for mutation tests.

Results live in `target/mutation/<crate>/mutants.out/`: `outcomes.json`, mutant
diffs and build/test logs. `selected.json` alongside it records the selected
mutations. `caught` means a test failed; `missed` means a test still needs a
meaningful assertion. `unviable` means the mutant did not compile and does not
demonstrate test strength. A timeout is a separate failure, not a caught mutant.
The command fails on survivors, timeouts or baseline failures. Add a minimal
regression test for each meaningful survivor and rerun the affected crate.
Only demonstrably equivalent mutants may be excluded, with a narrow regex and
a comment explaining why their behavior is identical; do not skip whole
functions to obtain a passing report.

The initial selection excludes one equivalent mutant: `>` to `>=` at the
`f32` probability-sum tolerance. Equality with `1e-5` cannot occur after
subtracting one from an `f32` sum near one. Tests exercise the nearest
representable sums inside and outside that boundary. A second one is `<` to
`<=` in FlowSOM's `training_rows`: a subsample of every event is the whole
matrix in row order, so training is identical. The exact exclusions and their
rationale live in `.cargo/mutants.toml`.

The **Mutation tests** workflow runs manually and on Mondays at 04:00 UTC,
separately from PR checks. Its eleven-crate matrix runs up to three jobs at once,
with a 120-minute limit per crate and reports retained for 35 days even on
failure. `just verify` remains the ordinary quality gate. Neither mutation
testing nor local CRAP analysis rewrites the reviewed Linux CRAP baseline.

## Coding Style

Follow Rust 2024 conventions and let `rustfmt` control formatting. Use:

- 4-space indentation
- `snake_case` for functions and modules
- `PascalCase` for structs, enums, and traits
- `SCREAMING_SNAKE_CASE` for constants

Keep `cytiq-engine` thin and move reusable logic into a focused library crate. Add brief comments only where the intent is not obvious from the code.

## Testing

New logic should include tests. Put unit tests near the implementation with `#[cfg(test)]`, and place broader scenarios in `tests/`. Name tests by observable behavior, for example `loads_cd22_sample_data` or `fails_when_input_is_missing`.

## Commits and Pull Requests

Git history currently contains only `Initial commit`, so keep commit messages short, imperative, and scoped. Examples: `Add FCS file loader`, `Refactor input parsing`.

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
