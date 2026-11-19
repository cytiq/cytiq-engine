# cytiq-cytoscan

CytoScan-style anomaly detection for cohorts of FCS files. Backs the
`cytiq cytoscan` command.

- Paper: <https://doi.org/10.1101/2025.09.24.678276>
- Upstream R reference: <https://github.com/AUMC-HEMA/CytoScan> (parity target
  CytoScan 0.1.0, `c97f403`)

Each file is reduced to one feature vector over the selected channels, then
files are flagged either as outliers within the test cohort or as novelties
against a separate high-quality reference cohort.

## Reading the flags

The two flags answer different questions, and neither carries a validated
false-positive rate. `cytoscan_config.json` (and `model_diagnostics.json`)
describe each flag of the run under `diagnostics.outlier_flag` and
`diagnostics.novelty_flag` (`null` when the run has no such flag):

| Field | `outlier_flag` | `novelty_flag` |
| --- | --- | --- |
| `scope` | `within_test_cohort`: the forest is fitted on the test files, so a file's score depends on the other files of the cohort | `against_reference`: each test file is scored on its own against the reference mixture |
| `threshold_rule`, `threshold_parameter` | `fixed_score`, 0.5 | `in_sample_quantile`, 0.05 |
| `calibration_status` | `uncalibrated_heuristic` | `insufficient_reference` or `in_sample_only` (as `novelty_calibration_status`) |
| `population_false_positive_rate` | `not_assessed` | `not_assessed` |
| `seed_dependence` | `seed_dependent` when trees are built (≥ 2 test files, ≥ 1 feature) | see below |
| `stability_status` | `not_assessed` without `--stability-seeds`; else `seed_invariant`, `stable` or `unstable` | same |

`insufficient_reference` means reference size does not exceed feature dimension
or has fewer than one observation in the nominal 5% tail, or a mixture component
has no more effective observations than feature dimensions. `in_sample_only`
means the threshold is the 5% quantile of the log-densities of the same files
the mixture was fitted on: held-out normal files fall below it more often than
5%. Scores and flags are those of upstream CytoScan; read a flag as a ranking
under one seed, not as a calibrated decision. The pre-registered novelty
benchmark ([`parity/scientific`](../../parity/scientific/README.md#cytoscan-novelty-benchmark))
measures false-positive rates on independent normal cohorts: on its synthetic
confirmation cohorts the upstream flag marks 30–100% of held-out normal files
in every summary/quantiles cell, at every reference size from 8 to 64.

## Calibrated novelty mode

`--novelty-mode conformal-ledoit-wolf-v1` (`CytoScanConfig::novelty_mode`,
`NoveltyMode::ConformalLedoitWolfV1`) is the one decision the benchmark's frozen
rule made eligible (rule v2, confirmation profile). It is opt-in and versioned;
the default `upstream` mode is unchanged and keeps R parity.

- **Model.** One Gaussian on the reference features with Ledoit–Wolf shrunk
  covariance, as `sklearn.covariance.LedoitWolf` (unit test against
  scikit-learn 1.9.1 to 1e-12 in the shrinkage).
- **Threshold.** Cross-conformal: the references (in resolved order) form
  `K = min(n, 10)` folds, reference `i` in fold `i mod K`. Each fold's model,
  fitted without it, scores its held-out references and every test file;
  `c = Σ_k #{held-out i : ll_k(i) ≤ ll_k(test)}`, `p = (1 + c) / (n + 1)`, and a
  test file is novel when `20 (1 + c) ≤ n + 1` (`p ≤ 1/20`, decided in
  integers).
- **Abstention.** Below 19 references no p-value can reach 1/20, so every
  file's `is_novelty` is empty, `novelty_decision` is `abstain` and the run
  warns; a fold whose covariance is not positive definite abstains the same
  way. The run still succeeds.
- **Scope.** `summary` and `quantiles` features only, and only with
  `--flag-method novelty` or `both`; other combinations are refused
  (`UnsupportedNoveltyMode`, exit 2) because the benchmark did not validate
  them.
- **Outputs.** `cytoscan_summary.csv` gains `novelty_p_value` and
  `novelty_decision` (`novel`, `not_novel`, `abstain`) after the upstream
  columns; `is_novelty` is the calibrated decision and
  `novelty_log_likelihood` the log-density under the model of every reference.
  `diagnostics.novelty_calibration_status` and `novelty_flag.calibration_status`
  are `cross_conformal`, `abstained_insufficient_reference` or
  `abstained_fit_failed`; `diagnostics.novelty_conformal` records the folds,
  shrinkage, guarantee and validation; the mclust fields are `null`. With
  `--stability-seeds`, `cytoscan_stability_seeds.csv` gains `novelty_p_value`
  and the margin is `p - 1/20`. The mode is recorded in provenance only when
  set.
- **What it guarantees.** On exchangeable references (same instrument, panel,
  processing and population) the false-positive rate is about 1/20 on average
  over reference cohorts. That is a marginal guarantee: one cohort's rate
  varies (see `P(FPR > 0.10)` in the benchmark), and a batch or instrument
  shift is a real difference that the mode flags. Real-data population FPR is
  `not_assessed`.

On the confirmation cohorts it gave FPR 0.027–0.053 (bootstrap upper bounds
≤ 0.078) with every anomaly class detected at ≥ 32 references, and the
benchmark checks production against its own composition decision for
decision ([`REPORT.md` §9](../../parity/REPORT.md#9-cytoscan-novelty-stability-and-calibration-2026-11-19)).

Every selected channel must have the same physical name, marker and effective
preprocessing in every reference/test file. Mixing raw channels with channels
compensated and transformed by CytoScan is rejected even without `--strict`.
Distinct acquisition-specific spillover values remain allowed when the effective
channel mapping and scale agree. This cannot detect undocumented processing
already applied by an external application.

## Pipeline

1. Resolve inputs: `.fcs` files, directories with immediate `.fcs` children,
   or simple file-name globs.
2. Read each file like `CytoScan::readInput`: truncate values above `$PnR`,
   compensate the selected channels with `SPILL` / `$SPILL`, transform with
   `asinh(x / 150)`. Without spillover the channels stay raw and a warning is
   recorded.
3. Subsample 1,000 events per file with the generator reseeded to the run's
   seed (42, as upstream) for every file, so results do not depend on path
   spelling or file order.
4. Compute features (`FeatureMethod`): `summary`, `quantiles` (default), `emd`,
   `binning` (`cut()` decile bins) or `fingerprint` (`flowFP` probability
   binning, 4 recursions). The last three use an aggregate of
   `ceil(10000 / n_files)` events per file, taken from the reference cohort
   when novelty is enabled.
5. Flag (`FlagMethod`):
   - `outlier` (default): isolation forest as
     `isotree::isolation.forest(sample_size = 1, ntrees = 1000, ndim = 1)`;
     score `>= 0.5` is an outlier.
   - `novelty`: a Gaussian mixture fitted on reference features as
     `mclust::Mclust` does by default (`G = 1..9`, best BIC over the
     covariance structures `EII`, `VII`, `EEI`, `VEI`, `EVI`, `VVI`, and with
     more reference files than features also `EEE`, `VVV`; EM from Ward's
     hierarchical clustering, as `hc(modelName = "EII", use = "SVD")`); test
     files at or below the 5% quantile of reference log-densities are
     novelties. With no more reference files than features a warning is
     recorded: only spherical and diagonal structures fit, as in mclust.
   - `both`.

Measured agreement with R is in [`parity/REPORT.md`](../../parity/REPORT.md) §2.7:
features match R to ~5e-13 on identical events; random draws use `rand`, so
results are reproducible but not bit-identical to R.
Event selections and tree-feature choices retain rand 0.8's integer sampling
semantics when using rand 0.10; golden tests guard those seeded results.

`--seed` (`CytoScanConfig::random_seed`, default `DEFAULT_RANDOM_SEED` = 42)
seeds the per-file subsample, the aggregate subsample and the isolation trees
(tree `t` uses `seed + t`, wrapping, so seeds 42 and 43 share 999 of 1000
trees). It is recorded in provenance `constants.random_seed`, so a different
seed invalidates reuse while runs without the flag keep their fingerprints.
One seed's flags are one Monte Carlo draw, not a calibrated decision.

`novelty_flag.seed_dependence` is exact: the novelty flag is `seed_dependent`
when a test or reference file has more events than the per-file sample
(1,000), or, for `emd`, `binning` and `fingerprint`, when a reference file has
more events than its aggregate share `ceil(10000 / n_reference)`. Otherwise no
random number is drawn for it and it is `seed_invariant`.

## Seed stability

`--stability-seeds N` (`CytoScanConfig::stability_seeds`, 2 to
`MAX_STABILITY_SEEDS` = 64) repeats the analysis of the loaded files with `N`
seeds in total: `--seed` and `stability_seed(seed, j) = seed + j * 2^32`
(wrapping) for `j = 1..N-1`. The stride keeps the generator seeds of every
tree of every seed disjoint (a unit test checks all 64,000). Each seed rebuilds
its aggregate, features, forest and mixture, so seed `j` reproduces a separate
run with `--seed stability_seed(seed, j)` exactly. Only the algorithm seed
varies: files, reference membership and channels are fixed. The summary and
feature tables do not change; two files are added:

- `cytoscan_stability.csv`, one row per test file: `seeds`, per flag the
  number of seeds assessed and flagged, the min/median/max outlier score and
  novelty margin (`log-likelihood - threshold`, flagged at `<= 0`), and
  `outlier_decision` / `novelty_decision`: `stable_flag`, `stable_no_flag`,
  `unstable` or `not_assessed`.
- `cytoscan_stability_seeds.csv`, one row per seed and test file:
  `seed_index`, `seed`, `status` (`ok`, `feature_failed`,
  `novelty_fit_failed`) and the summary's values plus `novelty_threshold`.
  The `seed_index` 0 rows repeat `cytoscan_summary.csv`.

`diagnostics.stability` records the seeds, failed seeds (kept as observations:
a seed whose aggregate or mixture cannot be fitted makes the flag `unstable`),
and per flag the share of files with a unanimous decision, the number of
unstable files, `pairwise_disagreement` (the probability that two seeds
disagree on a file) and the largest per-file score or margin range. A
`seed_invariant` flag is checked to come out bit-identical at every seed. The
cost is about `N` times that of the analysis; files are loaded once and seeds
run one after another.

`CytoScanConfig::events_per_file` (library only, default
`DEFAULT_EVENTS_PER_FILE` = 1,000, upstream's `which.lines`) sets the per-file
sample; other values deviate from upstream, are recorded in `constants` and
add a warning. The aggregate share and the debug boxplots do not follow it.

## Novelty reference cohorts

Use more reference files than features for a reliable novelty flag: `summary`
has 4 features per channel, `quantiles` 9 and `binning` 10. With fewer files,
only spherical and diagonal covariance structures fit, and the threshold
rests on a handful of log-densities; the run warns.

Unlike mclust, the mixture does not try its other full-covariance structures
(`VEV`, `EVE`, ...), and starts EM from Ward's clustering where mclust uses
`hcVVV` when files outnumber features. It also rejects a one-component full
covariance that is singular up to rounding. Binning and fingerprint features
sum to 1 per channel, so mclust may keep that covariance and flag every test
file. `cytoscan_config.json` records the chosen `novelty_model`, `G` and BIC.

## Usage

```bash
cytiq cytoscan --input ./test_fcs --output ./cytoscan_results --channels CD3,CD4,CD8
cytiq cytoscan --input ./test_fcs --reference ./reference_fcs \
  --output ./cytoscan_results --channel CD3 --channel CD4 \
  --method fingerprint --flag-method both --debug-dir ./cytoscan_debug
# calibrated novelty decision with its seed stability (summary or quantiles)
cytiq cytoscan --input ./test_fcs --reference ./reference_fcs \
  --output ./cytoscan_calibrated --channels CD3,CD4,CD8 --method quantiles \
  --flag-method novelty --novelty-mode conformal-ledoit-wolf-v1 --stability-seeds 10
```

From Rust:

```rust
use cytiq_cytoscan::{CytoScanConfig, CytoScanRunner, FeatureMethod, FlagMethod};
use cytiq_fcs::parse_channel_list;

let mut config = CytoScanConfig::new(
    vec!["./test_fcs".into()],
    "./cytoscan_results".into(),
    parse_channel_list("CD3,CD4,CD8")?,
);
config.reference_inputs = vec!["./reference_fcs".into()];
config.feature_method = FeatureMethod::Emd;
config.flag_method = FlagMethod::Both;
let result = CytoScanRunner::run(&config)?;
for row in &result.summary_rows { /* per-file flags and scores */ }
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `CytoScanRunner::run` / `CytoScanConfig` | Run the pipeline and write outputs |
| `CytoScanRunner::input_files` | Test then reference FCS files a run reads, resolved as `run` does |
| `FeatureMethod`, `FlagMethod` | Feature and flagging methods |
| `CytoScanResult` | `parameters`, test/reference `CytoScanFeatureMatrix`, `summary_rows`, `diagnostics` |
| `CytoScanParameters`, `CytoScanConstants` | Effective settings, serialized into `cytoscan_config.json`; the constants also go into provenance `constants` |
| `CytoScanFeatureMatrix`, `CytoScanFeatureRow`, `CytoScanSummaryRow`, `CytoScanDiagnostics` | Result rows and model diagnostics |
| `FlagAssessment`, `FlagScope`, `ThresholdRule`, `CalibrationStatus`, `Assessment`, `SeedDependence`, `StabilityStatus` | How to read each flag (`diagnostics.outlier_flag`, `novelty_flag`) |
| `stability_seed`, `MAX_STABILITY_SEEDS`, `STABILITY_SEED_STRIDE` | Seeds of a `--stability-seeds` run |
| `CytoScanStability`, `CytoScanStabilityFileRow`, `CytoScanStabilitySeedRow`, `StabilitySummary`, `FlagStability`, `FailedSeed`, `FileDecision`, `SeedStatus` | Rows and summary of a stability run (`CytoScanResult::stability`, `diagnostics.stability`) |
| `NoveltyMode`, `NoveltyDecision`, `ConformalStatus`, `ConformalDiagnostics`, `CONFORMAL_ALPHA`, `CONFORMAL_MAX_FOLDS`, `MIN_CONFORMAL_REFERENCE` | The calibrated novelty mode (`--novelty-mode`) |
| `MclustModel` | mclust's name of the fitted novelty structure (`VVI`, `XXX`, ...) |
| `DEFAULT_*` constants | Algorithm settings (default seed 42, overridable with `--seed`; 1,000 events, overridable through `CytoScanConfig::events_per_file`; fixed: 10,000 aggregate, 1,000 trees, 0.5 / 0.05 thresholds, cofactor 150, 4 recursions) |
| `CytoScanError` | Error type for every failure mode |

## Output rules

- Always written under `output_dir` (created if missing; a directory that
  already holds files is refused before any input is read unless `overwrite`,
  CLI `--overwrite`): `cytoscan_summary.csv`, `cytoscan_features.csv`,
  `cytoscan_config.json`.
- With `debug_dir`: `reference_features.csv`, `aggregate_sampling.csv`,
  `model_diagnostics.json`, `pca_coordinates.csv`, `pca_loadings.csv`,
  `feature_heatmap.csv`, `boxplot_samples.csv` and a Plotly `report.html`.
- With `stability_seeds`: `cytoscan_stability.csv` and
  `cytoscan_stability_seeds.csv`, published with the three tables. A run
  without it refuses (`StaleStabilityOutput`, exit 4) an `output_dir` that
  holds either file, since `overwrite` would leave them describing other
  outputs.
- `strict` (CLI `--strict`) turns the "no spillover metadata" and "channel
  not in the spillover matrix" warnings into errors.
- Inputs must be raw: a file another cytiq stage compensated, unmixed or
  transformed is `ProcessedInput`, because it would be scored on a different
  scale than the raw files of the cohort.
- `novelty` and `both` require reference inputs.
- Work runs on the caller's rayon pool; `cytiq cytoscan --threads` sizes it.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for reading and compensation
- `nalgebra` (Cholesky, SVD, PCA), `rand` / `rand_distr`, `rayon`, `csv`,
  `serde_json`, `plotly`

## Tests

```bash
cargo test -p cytiq-cytoscan
cargo test -p cytiq-engine --test cytoscan_cli
```

Unit tests in `src/runner.rs`, `src/mixture.rs` and `src/novelty.rs` pin
behaviour to the R packages (transport, `cut()`, flowFP, isotree, mclust's
BIC table, `hclass` partitions and densities).
