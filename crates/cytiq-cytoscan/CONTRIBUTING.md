# Contributing to cytiq-cytoscan

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — crate docs (paper and upstream links) and re-exports only.
- `src/runner.rs` — config and result types, input resolution (files,
  directories, globs), FCS preprocessing, subsampling, all five feature
  methods, `analyse` (one seed's features, outlier scores and novelty model),
  diagnostics, summary rows; most unit tests.
- `src/outlier.rs` — the isolation forest (`isotree`), fitted on the test
  cohort only.
- `src/assessment.rs` — how to read each flag: scope, threshold rule,
  calibration status, the exact seed-dependence rule.
- `src/stability.rs` — `--stability-seeds`: derived seeds, per-seed analyses
  of the loaded files, per-file and per-flag agreement.
- `src/calibration.rs` — `--novelty-mode conformal-ledoit-wolf-v1`: the
  Ledoit–Wolf Gaussian (port of `sklearn.covariance.LedoitWolf`), folds and the
  cross-conformal integer rule.
- `src/mixture.rs` — port of `mclust::Mclust`'s default search: `hcEII`
  start (Ward on SVD-whitened data), EM for `EII` … `VVV`, BIC table and
  `pickBIC`; unit tests against mclust's BIC table, `hclass` and `dens`.
- `src/novelty.rs` — the novelty model on top of it: threshold at the 5%
  quantile of reference log-densities.
- `src/sampling.rs` — stable seeded event and tree-feature selections, preserving
  rand 0.8's integer sampling across dependency upgrades; golden tests cover
  each sampling algorithm.
- `src/export.rs` — CSV / JSON outputs, debug artifacts, PCA, Ward-ordered
  heatmap and the Plotly HTML report.
- `src/error.rs` — `CytoScanError` (`thiserror`).

## Invariants

- Upstream CytoScan 0.1.0 is the reference. Every behavioural change must keep
  or improve parity; check it with the `parity/` harness (`parity/run_all.sh`) and update
  `parity/REPORT.md` §2.7 when numbers move. Comments explain which upstream
  call a piece of code mirrors (`readInput`, `cut()`, `flowFP`, `isotree`,
  `mclust`); keep them accurate.
- Determinism: all randomness is seeded from `CytoScanConfig::random_seed`
  (`DEFAULT_RANDOM_SEED`, 42, unless `--seed` says otherwise). Per-file
  subsamples reseed for every file and must not depend on the path or on
  processing order (`subsample_does_not_depend_on_path`). Isolation trees
  seed per tree index (`seed.wrapping_add(t)`), so results are stable under
  `rayon` parallelism. The seed is the only algorithm setting exposed as a
  flag; it is recorded in `constants`. `--stability-seeds` repeats the run
  with derived seeds and is recorded in the CLI config only when set.
- Algorithm settings are fixed `DEFAULT_*` constants, not CLI flags, except
  the seed. Exposing another is a deliberate API change: document it in this
  crate's [README](README.md). `events_per_file` is a library field only;
  it reaches the CLI only if the novelty benchmark's frozen decision rule
  makes its arm eligible (`parity/scientific/novelty_benchmark.py`).
- Stability seeds are `seed + j * 2^32`; the stride must stay above the tree
  count so that no two seeds share a tree (`stability_seeds_never_share_a_generator_seed`).
  A stability run never changes the primary outputs: seed 0 is the primary
  analysis itself, and the summary and features are byte-identical with and
  without `--stability-seeds`.
- `assessment::novelty_seed_dependence` must match what the sampler does:
  a stability run asserts that a `seed_invariant` flag is bit-identical at
  every seed, so a wrong rule fails loudly instead of misreporting.
- The debug boxplots sample `DISPLAY_EVENTS_PER_FILE` events: every value is
  one row, so they must not follow `events_per_file`.
- The conformal mode is exactly what the novelty benchmark validated:
  folds `i mod min(n, 10)` over the resolved reference order, counts with
  `<=`, the decision `20 (1 + c) <= n + 1` in integers, abstention below 19
  references, summary/quantiles only. `novelty_benchmark.py run
  --rust-calibration` compares production with the benchmark's composition
  seed by seed; a change here needs a new versioned mode, not an edit of v1.
- The default `upstream` mode's outputs stay byte-identical: the conformal
  columns are written only in the conformal mode.
- Preprocessing order is truncate at `$PnR`, compensate, then
  `asinh(x / 150)`. Missing spillover is a warning, not an error.
- The aggregate takes `ceil(10000 / n_files)` events from every file without
  truncating the total, and comes from the reference cohort when novelty is on.
- Output file names are constants in `src/export.rs` and are consumed by
  scripts and the CLI tests; do not rename them casually.

## Where changes belong

- FCS parsing, spillover parsing and matrix inversion belong in `cytiq-fcs`.
- CLI flags belong in `crates/cytiq-engine/src/commands/cytoscan.rs`; this crate takes a
  `CytoScanConfig` and has no `clap` dependency.

## Testing

```bash
cargo test -p cytiq-cytoscan
cargo test -p cytiq-engine --test cytoscan_cli
```

- `src/runner.rs` `#[cfg(test)]` — EMD vs `transport::wasserstein1d`, binning
  vs `cut()`, fingerprint vs flowFP, path-independent subsampling, aggregate
  shares, the pinned default constants JSON, `events_per_file` and
  `stability_seeds` validation.
- `src/mixture.rs` `#[cfg(test)]` — BIC tables of every structure and `G`
  vs `Mclust` (`n > d` and `n <= d`), `hclass` partitions, chosen model and
  log-densities; fixtures from `parity/make_fixtures.R`.
- `src/novelty.rs` `#[cfg(test)]` — `XXX` densities, determinism, no model.
- `src/outlier.rs` `#[cfg(test)]` — isotree expected depth, scores within
  Monte Carlo noise, seed dependence of the trees.
- `src/calibration.rs` `#[cfg(test)]` — Ledoit–Wolf against scikit-learn,
  one feature, two files, singular data and zero features, folds, each fold
  scored by a model fitted without it, the integer rule at 18/19/39
  references, abstention, ties, a singular fold.
- `src/stability.rs` / `src/assessment.rs` `#[cfg(test)]` — seed derivation
  and disjointness, flag boundaries, medians and infinite margins, failed
  seeds, pairwise disagreement, the seed-dependence rule.
- `crates/cytiq-engine/tests/cytoscan_cli.rs` — end-to-end runs for every
  feature method, novelty with reference, debug artifacts, error cases;
  stability rows equal separate `--seed` runs, seed-invariant novelty on short
  files, the stability-seed range and the stale-output refusal; the conformal
  mode's p-values and decisions, abstention below 19 references (and its debug
  label), its per-seed p-values and its refusals.

The crate's `tests/` directory is currently empty. Reference values in unit
tests come from the R packages; note the R call used to produce new ones.
