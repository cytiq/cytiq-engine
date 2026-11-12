# Contributing to cytiq-cytoscan

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — crate docs (paper and upstream links) and re-exports only.
- `src/runner.rs` — config and result types, input resolution (files,
  directories, globs), FCS preprocessing, subsampling, all five feature
  methods, isolation-forest outlier scoring, summary rows; most unit tests.
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
  flag; it is recorded in `constants`.
- Algorithm settings are fixed `DEFAULT_*` constants, not CLI flags, except
  the seed. Exposing another is a deliberate API change: document it in this
  crate's [README](README.md).
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
  vs `cut()`, fingerprint vs flowFP, isotree expected depth and score noise,
  path-independent subsampling, aggregate shares.
- `src/mixture.rs` `#[cfg(test)]` — BIC tables of every structure and `G`
  vs `Mclust` (`n > d` and `n <= d`), `hclass` partitions, chosen model and
  log-densities; fixtures from `parity/make_fixtures.R`.
- `src/novelty.rs` `#[cfg(test)]` — `XXX` densities, determinism, no model.
- `crates/cytiq-engine/tests/cytoscan_cli.rs` — end-to-end runs for every
  feature method, novelty with reference, debug artifacts, error cases.

The crate's `tests/` directory is currently empty. Reference values in unit
tests come from the R packages; note the R call used to produce new ones.
