# Scientific regression and robustness experiments

Safety precedes performance. These tools measure uncertainty and compare
independent references; they never modify compensation or select a new solver.
All run evidence defaults to ignored `target/`. Golden imports are explicit
maintenance operations, excluded from CI.

## Required validation layers

| Layer | Invocation | Evidence |
| --- | --- | --- |
| Unit and malformed inputs | `just test` | numerical identities, resource bounds, saved fuzz crashes |
| Fast scientific regression | `just scientific` | fixture SHA-256, independent R/Python smoke, whole pipeline and three labelled real CyMAE excerpts |
| Numerical parity | `just parity` and `just parity-cytonorm` | all existing tolerances against pinned R/Python |
| Independent acquisitions | `just parity-real` and `just parity-cytonorm-real` | pinned real acquisitions and held-out donor |
| Biological retention | commands below | original-population denominators after each assessed filtering stage |

`.github/workflows/scientific.yml` conservatively selects full parity for shared
numerics, relevant crates, fixtures, dependency locks and reference scripts.
Weekly/manual runs and release delivery perform full validation. Release/manual
full runs additionally fetch every required CyMAE file; the normal-donor and
weak-phenotype checks use in-repository fixtures and run with every selected
parity run. Each layer runs and reports even after an earlier one failed, and
any failure fails the job. No missing dataset can turn a required suite into a
passing skip. Uploads contain results/environment, not full acquisitions or
source archives. The reference environment (keyed by the R/Python/mise locks)
and the content-addressed real-data cache (keyed by the dataset pins, restored
from older keys and SHA-256-verified on use) are cached separately, with
main-only saves; the complete CyMAE corpus is deliberately outside the cache.

Release validation and publication use the same resolved commit SHA. The final
signed tag cannot be moved on retry; a conflicting tag fails publication.

`setup_env.sh` restores `requirements.lock` (transitive wheel hashes) and
`r-packages.lock.json` (source tarball versions/hashes and dependency order).
The R lock includes the complete Depends/Imports/LinkingTo closure, including
packages found outside the managed library and recommended R packages. Only
base packages are provided by the pinned R runtime. Restore validates closure
and installation order before consulting an existing library, so a warm local
environment cannot conceal a missing dependency. Setup logs are retained in
`target/scientific/environment/` even when package restoration fails.
Changing a reference environment requires an explicit lock update and review.
`check_fixtures.py` verifies contents and inventory of both fixture families.
Do not regenerate goldens or widen tolerances to accommodate changed results.

## Reproducible experiments

After `just parity-setup` and a release build:

```bash
PY=parity/.env/py/bin/python
BIN=target/release/cytiq
$PY parity/scientific/run.py spectral --binary "$BIN" --output target/scientific/spectral --replicates 1000
$PY parity/scientific/run.py compensation --binary "$BIN" --output target/scientific/compensation --replicates 1000
$PY parity/scientific/run.py novelty --binary "$BIN" --output target/scientific/novelty
$PY parity/scientific/normal.py --binary "$BIN" --output target/scientific/normal
python3 parity/scientific/datasets.py fetch --lock data/fixtures/scientific/cymae.lock.json --cache target/scientific/cymae
$PY parity/scientific/run.py retention --binary "$BIN" --registry data/fixtures/scientific/cymae.lock.json --cache target/scientific/cymae --output target/scientific/retention
```

Each output directory must be new. Reports record binary/generator/dataset hashes,
seeds, parameters, completion/failure and wall time. The complete CyMAE registry
contains 258 FCS, five dictionaries/metadata files and four descriptive root
assets (267 files, approximately 16 GB).
`datasets.py fetch` verifies the committed lock and never rewrites it. Signed
download URLs are neither logged nor stored. `import-cymae` is only for an explicit
new public-version import. Dataset provenance, licenses and the documented
PeacoQC upstream indexing discrepancy are in
[`data/fixtures/scientific`](../../data/fixtures/scientific).

Control bootstrap resamples events and recomputes gating/fitting on every sample.
Percentile intervals with 1,000 resamples describe within-acquisition sampling
uncertainty, not variation between donors/instruments. Three independently seeded
synthetic control acquisitions also measure between-control variation; real matched
repeat controls remain unavailable. The independent AutoSpread experiment records
row=dye/column=secondary detector orientation. Empirical spreading has secondary
units/√primary units; AutoSpread's `sqrt_like` regressions have this scaling only
asymptotically. `spread_units.py` checks both-detector rescaling at 0.01/1/100.
It compares the raw-detector surrogate with variance excess after compensation,
keeps negative variance differences visible and excludes the structural diagonal.
SSI is never an independent matrix correctness oracle.
`coefficient_perturbation_all` perturbs every off-diagonal spillover coefficient
by 1e-4/1e-3/1e-2 and reports RMSE, RMSE per unit coefficient error, maximum and
per-detector error of the compensated events. With A = E·S⁻¹ the exact error r
and the first-order term f = −latent·A satisfy f − r = r·A, so f is within
‖A‖₂ of r; the run fails if any cell violates that bound.

`metrics.py` computes spectral cosine, SVD conditioning and OLS unit-noise
covariance independently with NumPy QR. Independent synthetic positive/negative
controls also compare unmixing-dependent variance excess with `P diag(variance) Pᵀ`
under the generator's declared gain/electronic-noise law, with orientation and
units recorded in the report. Residual size and unit-noise covariance
do not measure empirical biological spreading. Offline solver experiments use
known count calibration/background and oracle variances or known second AF;
their gains cannot justify production WLS/Poisson/multiple-AF estimation on ADC
or normalized CyTOF. Householder OLS remains the production baseline.

For real Aurora AF sensitivity, first fetch `aurora_pbmc_spectral` with
`just parity-real`. Then run:

```bash
$PY parity/scientific/af_sensitivity.py --binary "$BIN" --source target/scientific/real/aurora_pbmc_spectral --output target/scientific/af-sensitivity --seeds 20
```

The same 4,000 sample events and stained controls stay fixed. Unstained controls
use all/1,000/10,000 events and seeds 1–20. Export checks raw values, physical
channel names, ranges, gains, labels and linear-amplifier metadata. Reports expose
AF peak switches, spectral cosine and downstream unmixing RMS differences. This
measures sensitivity to event selection, not independent AF donor variability.

Novelty experiments vary reference count, feature method/dimension and five
reference-membership seeds at the default algorithm seed 42.
`production_seed_stability` then fixes 32 reference files and the test cohort
and varies only `cytoscan --seed` (event subsample and isolation trees) over
1/7/42/101/2026, reporting per-file outlier-score ranges and how often each
outlier and novelty flag agrees across seeds. It is reported, not gated.
`subsampling.py` additionally tests 250/1,000/2,000 available events and five
seeded event permutations at fixed reference membership, exercising the actual
1,000-event production cap with summary/quantile features.
Reference-only shrinkage/PCA trials are research outputs, not new models. The
normal cohort holds out one of four independent unstimulated donors at a time;
three-reference scores have `insufficient_reference` status. Wilson intervals
expose the very small denominator. Overlapping reference splits are not independent
replications and do not increase the effective number of normal acquisitions.

Retention reports use original source rows, conditional/cumulative retention and
absolute/relative population-frequency errors. Rare populations are visible even
when overall removal is small. Published population labels do not identify
technical artifacts or weak-positive ground truth: technical removal accuracy and
CyTOF FSC/SSC cleaning are explicitly unassessed. The synthetic pipeline separately
tests known artifacts and rare events. `weak_population.py` attenuates measured
compensated CD19 by 20x in its labelled B/rare cells, remixes the declared spillover
and measures every filtering stage against the original technical truth. Optional
`--baseline-bin` requires identical population metrics from clean main. This
counterfactual sensitivity experiment is not independently labelled experimental
weak-positive truth, which remains unavailable. New biological acceptance bounds must
be measured on a specific dataset and frozen with provenance; none is inferred
from an overall removed fraction.

An experiment's `completed` status establishes that its inputs were present and
measurements were produced. It does not turn high observed FPR, missing technical
truth or a phenotype perturbation into evidence of biological safety.

## Paired performance

```bash
cargo build --release --locked -p cytiq-engine --example performance_support
$PY parity/scientific/prepare_performance.py --output target/scientific/workloads --helper target/release/examples/performance_support --binary "$BIN" --accept peacoqc_1000000,cytonorm_apply_1000000
$PY performance/scientific.py --manifest target/scientific/workloads/cases.json --baseline-bin target/performance/baseline/cytiq --candidate-bin "$BIN" --helper target/release/examples/performance_support --output target/scientific/performance
```

Use an isolated clean-main baseline and the same toolchain/profile/build flags.
The 100k/1M workloads cover FlowSOM, CytoNorm training and apply (to a model
trained during preparation), AutoSpectral and PeacoQC; each pair
uses the same paths and arguments and alternates order, with two warmups and 20
measured pairs. Run sequentially on an idle host. Shortened runs are diagnostic.
Outputs must match exactly (logical Parquet equality); only documented additive
AutoSpectral diagnostics and individually verified volatile provenance fields
are excluded. The Rust helper verifies every execution fingerprint and every
input/output digest before comparison. CPU, wall time, RSS and OS I/O blocks are
recorded. Logical file sizes are also reported; OS block counters may be zero on
macOS and are not converted into claimed physical I/O bytes.

Acceptance requires at least 10% lower median RSS and an upper paired-bootstrap
95% wall-ratio bound ≤1.05 for the optimization cases a run designates with
`--accept` (the example names this change's PeacoQC and CytoNorm apply cases);
every other case is a diagnostic. An
unoptimized diagnostic case need not improve. Report failures and insufficient
evidence; do not change exactness checks or repeatedly select favorable runs.
