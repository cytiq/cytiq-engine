# Quality checks

Details behind the checks summarized in [CONTRIBUTING.md](../CONTRIBUTING.md).

## CI extras

Rust checks keep their `quality` and `test` names and parallel runners. Optional `ci-cache-*` artifacts record
exact Cargo cache hits for the organization usage report; telemetry failures do not affect correctness checks.
The manual **Rust CI benchmark** workflow measures combined/separate checks on cold and warm ephemeral targets.
It never uploads build caches, since extra target caches previously evicted main's cache. Its warm measurements
describe repeated Cargo invocations, not Actions cache-transfer savings. Compare runner setup and summed job
durations too; the experiment does not alter required checks.

`CI_LIGHT_RUNNER` defaults to `ubuntu-24.04` and controls only release-version resolution. Trial `ubuntu-slim`
there after compatibility testing; retain it only when cost falls without increasing required-check P95 by
more than 10%. It never controls Docker or Rust runners.

`.github/workflows/lakehouse.yml` runs `just lakehouse-interop`. It uses one million events on pull
requests that change the events export, the FCS reader or the bounded Parquet reader, and ten
million weekly and on demand. It is not a required check.

`.github/workflows/fuzz.yml` fuzzes the FCS reader and the spillover parser (`cytiq-fcs`), the repair plan (`cytiq-repair`), the population JSON/YAML document, observation-table, annotation-table, events-table and dataset-manifest readers (`cytiq-population`), specimen result comparison (`cytiq-specimen`), the Gating-ML parser and writer (`cytiq-gating`) and the artifact-truth reader (`cytiq-qc`): one minute a target on pull requests that change one of those crates, ten minutes weekly, and on demand. Run a target locally with `just fuzz <crate> <target> [seconds]` (`just fuzz-fcs` remains for `cytiq-fcs`). It is not a required check. A crash uploads its input as an artifact; turn it into a regression test as `crates/cytiq-fcs/CONTRIBUTING.md` describes. `cargo test` replays the saved regressions on every run.

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
`cytiq-population` and `cytiq-specimen`, and the artifact-truth contract,
artifact-removal metrics and retention core of `cytiq-qc`, and the seed-stability
aggregation, seed-dependence rules and conformal Ledoit–Wolf novelty mode of
`cytiq-cytoscan`. Keep this selection in sync
when extracting or renaming helpers: a move must not remove mutation coverage.
The runner rejects an empty selection and checks the unmodified tests before
injecting mutations into a scratch copy; the working tree is never mutated.
The copy includes `.git` (`copy_vcs`): provenance identifies each mutated
build as `<sha>-dirty.<digest>`, which the reuse tests need.
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
separately from PR checks. Its thirteen-crate matrix runs up to three jobs at once,
with a 120-minute limit per crate and reports retained for 35 days even on
failure. `just verify` remains the ordinary quality gate. Neither mutation
testing nor local CRAP analysis rewrites the reviewed Linux CRAP baseline.
