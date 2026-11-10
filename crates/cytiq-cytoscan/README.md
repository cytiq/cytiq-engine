# cytiq-cytoscan

CytoScan-style anomaly detection for cohorts of FCS files. Backs the
`cytiq cytoscan` command.

- Paper: <https://doi.org/10.1101/2025.09.24.678276>
- Upstream R reference: <https://github.com/AUMC-HEMA/CytoScan> (parity target
  CytoScan 0.1.0, `c97f403`)

Each file is reduced to one feature vector over the selected channels, then
files are flagged either as outliers within the test cohort or as novelties
against a separate high-quality reference cohort.

Diagnostics expose reference size, numerical feature rank, constant features,
mixture effective component counts and `novelty_calibration_status`.
`insufficient_reference` means reference size does not exceed feature dimension
or has fewer than one observation in the nominal 5% tail, or a mixture component
has no more effective observations than feature dimensions. `in_sample_only` still
does not imply held-out calibration. Existing scores and flags are preserved;
small-cohort flags should not be interpreted as a calibrated false-positive rate.
The independent normal-donor experiment is documented in
[`parity/scientific`](../../parity/scientific).

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
3. Subsample 1,000 events per file with the generator reseeded to 42 for every
   file, so results do not depend on path spelling.
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
```

From Rust:

```rust
use cytiq_cytoscan::{CytoScanConfig, CytoScanRunner, FeatureMethod, FlagMethod, parse_channel_list};

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
| `MclustModel` | mclust's name of the fitted novelty structure (`VVI`, `XXX`, ...) |
| `DEFAULT_*` constants | Fixed algorithm settings (seed 42, 1,000 events, 10,000 aggregate, 1,000 trees, 0.5 / 0.05 thresholds, cofactor 150, 4 recursions) |
| `parse_channel_list` | Parse a comma-separated `--channels` value |
| `CytoScanError` | Error type for every failure mode |

## Output rules

- Always written under `output_dir` (created if missing; existing files are
  replaced): `cytoscan_summary.csv`, `cytoscan_features.csv`,
  `cytoscan_config.json`.
- With `debug_dir`: `reference_features.csv`, `aggregate_sampling.csv`,
  `model_diagnostics.json`, `pca_coordinates.csv`, `pca_loadings.csv`,
  `feature_heatmap.csv`, `boxplot_samples.csv` and a Plotly `report.html`.
- `strict` (CLI `--strict`) turns the "no spillover metadata" and "channel
  not in the spillover matrix" warnings into errors.
- Inputs must be raw: a file another cytiq stage compensated, unmixed or
  transformed is `ProcessedInput`, because it would be scored on a different
  scale than the raw files of the cohort.
- `novelty` and `both` require reference inputs; `threads = Some(0)` is
  rejected.

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
