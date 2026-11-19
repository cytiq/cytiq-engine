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
| CytoScan novelty calibration and stability | [`novelty_benchmark.py`](#cytoscan-novelty-benchmark) | FPR with replicate-bootstrap intervals on independent synthetic normal cohorts, seed agreement, per-class detection, batch shifts; real-donor FPR `not_assessed` |
| Technical artifact removal | [`qc_truth.py`](#qc-artifact-truth-benchmark) | per-column sensitivity, specificity, precision and clean loss against generated (synthetic) and definitional truth; real-data removal `not_assessed` |

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
and varies only the algorithm seed (event subsample, aggregate subsample and
isolation trees) with production's `cytoscan --stability-seeds 5`, reporting
per-file outlier-score ranges and how often each outlier and novelty flag
agrees across seeds. It is reported, not gated. Its earlier seeds
1/7/42/101/2026 shared 900–994 of 1000 isolation trees pairwise (tree `t` is
seeded `seed + t`) and understated the forest's seed variance; the stability
seeds `42 + j * 2^32` share none. The pre-registered
[CytoScan novelty benchmark](#cytoscan-novelty-benchmark) supersedes these
exploratory runs for every FPR and stability claim.
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

## AF reference benchmark

`af_benchmark.py` compares the production autofluorescence (AF) reference, a
hard argmax (`rlm` slopes against the detector with the highest gated mean),
with alternative estimators. It never changes production.

```bash
$PY parity/scientific/af_benchmark.py oracle --binary "$BIN" --source data/fixtures/parity/autospectral --output target/scientific/af-oracle
$PY parity/scientific/af_benchmark.py synthetic --binary "$BIN" --output target/scientific/af-benchmark/synthetic --held-out-seeds 2
$PY parity/scientific/af_benchmark.py real --binary "$BIN" --source target/scientific/real/aurora_pbmc_spectral --output target/scientific/af-benchmark/real --sample-step 10
$PY parity/scientific/af_benchmark.py table --report target/scientific/af-benchmark/synthetic/report.json --report target/scientific/af-benchmark/real/report.json --output target/scientific/af-benchmark/table
```

**Oracle.** Every run first calls `cytiq autospectral --debug-dir`, whose
`reference_events.json` exports the rows behind every spectrum. `af_estimators.py`
ports the estimator and the positive/negative selection operation by operation
(sequential sums, R type 7 quantiles, stable tie order) and must reproduce the AF
means, peak, runner-up, sensitivity, positives, matched negatives and every
spectrum to 1e-9 (`ORACLE_TOLERANCE`, never widened); a held-out run must also
reproduce its production AF, and the baseline must reproduce the unmixed FCS
within f32 rounding. On the parity fixture and the real Aurora controls all
values are bitwise equal. The scientific workflow runs the oracle on both.

**Arms.** `peak_argmax` (production), `runner_up` (the same estimator on the
second detector, as `af_peak_sensitivity`), peak-free single spectra
(`total_signal`: slopes against the per-event detector sum; `mean_ratio`: the
mean spectrum over its maximum; `principal_axis`: the top covariance
eigenvector), and multi-AF arms with k = 2, 3: `scatter_strata` (SSC quantile
strata, an identification by an independent variable) and `spectral_clusters`
(seeded spherical k-means on unit spectra; data-driven). Multi-AF arms are
unmixed jointly (one column per AF), by per-event minimum residual, and by
AutoSpectral's `assign.af.residuals` score. A multi-AF arm must pass the
predeclared `IDENTIFIABILITY` gates (events per component, pairwise and
AF–fluorophore cosine ≤ 0.98, condition number ≤ 1.5× production, split-half
component cosine ≥ 0.99) or it is `not_identifiable`.

**Metrics** per arm and solver:
- stability on the fixed production gate: cosine to the arm's own full-data
  estimate over 1,000/10,000-event subsamples (seeds 1–20), split halves,
  argmax switch rates, and a 1,000-replicate bootstrap of the peak margin and
  switch probability (`af_sensitivity.py` measures the same end to end with the
  gate recomputed);
- held-out unstained events: each fold's AF control half is rebuilt by
  production (so evaluated events never served as negatives), every arm is
  fitted on that half, and the other half's gated events are unmixed. Unstained
  cells should carry no fluorophore signal (AutoSpectral's
  `benchmark_af_spectra` criterion): per fluorophore median bias, robust SD
  ((p84 − p16)/2), p99 |abundance|, and relative residual;
- conditioning: condition numbers, maximum AF–fluorophore cosine, diag(P Pᵀ);
- in-sample single-stain spreading, labelled as such;
- downstream on the stained sample (every 10th event, R's rows): change against
  production in units of each component's robust SD, negative fraction,
  residual inside and outside the AF gate's FSC/SSC hull, and correlation to
  AutoSpectral's abundances, whose own AF spectrum varies between random draws
  and is not truth;
- `weak_positive_spike_in`, labelled semi-synthetic: a noise-free fluorophore
  signal added to held-out unstained events at 1/2/3/5× the baseline robust SD.
  The added vector lies in the fluorophore span, so every solver here recovers
  it without bias and its AUC only restates the unstained spread; with no
  calibrated photon noise it is optimistic;
- synthetic truth (`af_scenarios.py`; single AF, two near-tied sources on
  separated detectors, a 5% minority source linked or not linked to scatter,
  dim positives): AF subspace angle, abundance RMSE/bias, weak-population AUC.

**Decision rule.** `DECISION_RULE` (sha256 in every report and pinned by
`test_af_estimators.py`) was fixed before any arm was evaluated. An arm is
eligible only if the oracle passed, it lowers the median held-out robust SD to
≤ 0.95× production on every multi-source scenario and every real dataset
(≤ 1.01× on single-source scenarios), raises no component's p99 by > 5%, keeps
split-half cosine ≥ max(production, 0.999), condition ratio ≤ 1.25 and
AF–fluorophore cosine increase ≤ 0.01, holds on every synthetic seed, is
identifiable when multi-AF, and does so on **at least two independent real
datasets**. With Aurora the only real spectral dataset, every arm is reported
`eligible: false`. Even an eligible arm only opens a separate, reviewed proposal
that states its R parity impact; nothing is adopted automatically. Results and
interpretation are in [`parity/REPORT.md`](../REPORT.md#af-reference-benchmark).

## Repeat-control compensation study

```bash
$PY parity/scientific/repeat_controls.py run --binary "$BIN" --output target/scientific/repeat-controls
$PY parity/scientific/repeat_controls.py table --report target/scientific/repeat-controls/report.json --output target/scientific/repeat-controls/table
```

Synthetic and labelled so (`real_repeat_controls: not_assessed`). Six
predeclared scenarios (`null`, `brightness`, `tandem`, `af`, `gain`,
`combined`) generate 20 sessions × 8 independently stained tubes per dye from
`gen_autospill`'s dyes, spillover and noise models: per-session detector gains,
per-tube brightness, tandem donor drift (PE-Cy7→PE-A, APC-Cy7→APC-A) and AF
level/shape. Every matrix comes from production `cytiq autospill` (one run per
tube set, since a manifest holds one control per dye), every compensated sample
from `cytiq compensate`, whose output must equal raw·S⁻¹ within f32 rounding.

Tube truth, session truth (E[tube | session], exact for floored tandem draws)
and the stained sample's own truth are reported separately. The production
event bootstrap (3 sessions × 100 resamples, exploratory below 1,000) describes
one tube's sampling noise; t-intervals over a session's tubes target the session
truth; prediction intervals a new tube. Per off-diagonal coefficient the report
gives between-tube, between-session and within-tube SD and their excess, and
every coverage with a Wilson interval. Downstream, each session's matrix
compensates a sample of double negatives, single positives, a weak PE
population and FITC-bright/PE-dim co-expressors; gates are the 99.5th
percentile of truth-compensated double negatives. The report gives false
positives, weak and co-expressed PE detection, median shifts against the true
matrix, the Spearman correlation of FITC→PE coefficient error with the change in
PE false positives, and asserts the first-order bound
`‖f − r‖ ≤ ‖r‖·‖E·S⁻¹‖₂` (`metrics.matrix_perturbation`).

## QC artifact-truth benchmark

Population labels show what filtering did to each population, not whether
the removed events were artifacts. This benchmark measures technical
artifact removal separately from biological retention, against per-event
truth in the [`cytiq.qc.truth.v1`](../../docs/qc-truth.md) contract. It never
changes a filter, a QC policy or a release decision.

```bash
$PY parity/scientific/qc_truth.py synthetic --binary "$BIN" --output target/scientific/qc-truth [--seeds 10] [--baseline-bin "$BASELINE_BIN"]
$PY parity/scientific/qc_truth.py table --report target/scientific/qc-truth/report.json --output target/scientific/qc-truth
```

**Scenarios.** `qc_scenarios.py` predeclares 14 scenarios, ten seeds, and
`rare` as the population reported on its own. Each scenario is the
committed `ci` profile of `parity/gen/pipeline.py` with one domain stressed:

| Domain | Scenarios |
| --- | --- |
| all | `baseline` (seed 1 reproduces `data/fixtures/pipeline`), `events_50k` |
| debris | `debris_low` (2%), `debris_high` (25%), `rare_small` (1% rare cells with small, agranular scatter) |
| doublets | `doublet_high` (15%) |
| time | `time_short` (1% of the acquisition), `time_long` (20%) |
| saturation | `saturation_high` (3%) |
| viability | `dead_high` (30%), `dead_dim` (dead dye 2,000 ± 800 against live 300 ± 200) |
| weak positive | `weak_cd19` (compensated CD19 ×0.05 in B and rare cells, remixed) |
| rare | `rare_0.1pct`, `rare_1pct` |

The CI runs seeds 1 to 3; local runs take up to ten.

**Truth.**
- Debris, doublets, dead cells and the unstable interval come from the
  generator (`synthetic`).
- `saturated` is recomputed from the written raw FCS: a value at or above
  `$PnR − 1` on a spillover detector or scatter channel (`definitional`). It
  therefore also holds for the remixed weak scenario, where the generator's
  flags no longer would.
- Populations are the generator's labels, written as a `cytiq.mask.v2`
  membership, so gate errors do not enter.
- Every class labels every event, so no event is undetermined, and each
  bound equals its value.

**Run.**
1. The real chain runs on every case: compensate, transform, PeacoQC (FL1 to
   FL5), clean, mask combine, then `cytiq qc --artifact-truth --populations`.
   A `qc` exit code of 3 (a failed policy) is a result: truth never changes it.
2. `metrics.artifact_removal_oracle` and `metrics.membership_retention_oracle`
   are an independent NumPy port. They recompute every count, ratio and bound
   from the masks and the truth, and must equal `qc.json` exactly, type
   included. Both sides make the same single IEEE divisions of exact counts.
3. The integer counts of the CI seeds must equal
   `data/fixtures/scientific/qc-truth.json`. Clean main froze that file
   through the explicit `freeze` operation. `--baseline-bin` requires
   identical counts from another binary; the oracle needs no `--artifact-truth`
   support, so any older binary qualifies.

**Report.** Each report gives, per scenario, the seed median and range of
every column's sensitivity, specificity, precision and clean loss, and the
combined artifact recall and clean retention. It gives each population's
clean retention and its frequency error against clean events, with Wilson
intervals for `rare`. Those intervals are conditional on each acquisition's
fitted gates; the seeds vary the data.

Specificity counts every event that is not of the target class, including
other artifacts. `singlets` removing debris lowers its specificity and
precision without harming any cell. Read the harm in `clean_loss`, and
cross-class removal in `removed_by_class`.

Every result is labelled `synthetic_only`. Real-data removal accuracy and
policy derivation are `not_assessed` in every report, and the table prints no
number for an unassessed class. Results and interpretation are in
[`parity/REPORT.md`](../REPORT.md#8-qc-artifact-truth-and-population-retention-2026-11-19).

**Policy protocol.** `POLICY_PROTOCOL` (SHA-256 pinned by
`test_qc_truth.py`) fixes how a population acceptance bound could ever be
derived:
- Only `independent` or `expert_reference` truth may derive a bound.
- At least two real derivation datasets and one held-out dataset are needed.
  `disjoint_acquisitions` refuses any SHA-256 the two share.
- Population, metric and bound rule are frozen before evaluation.
- A bound that holds only opens a reviewed `QcPolicy` change.

With no such dataset registered, no bound is proposed and nothing changes a
status, an exit code or a release decision.

### Required real data for artifact truth

None of the following is pinned. Until it is, every real-data artifact
removal result is `not_assessed`, and real acquisitions support only
descriptive population retention and definitional saturation.

| Domain | Independent truth that would count | Why population labels are not enough |
| --- | --- | --- |
| Debris | Buffer-only or bead-only acquisitions on the same instrument settings (every event is debris or noise); FSC-sorted cell fractions reacquired | an FSC/SSC gate drawn by an expert uses the filter's own channels |
| Doublets | Mixtures of separately barcoded or mutually exclusive-marker cells (heterotypic events are doublets); imaging flow cytometry of the same tubes | expert singlet gates use FSC-A/H, the filter's own ratio |
| Viability | A second, independent viability readout (another dye or a heat-killed spike-in at a known fraction) | the filter thresholds the same dye |
| Time anomalies | Acquisitions with deliberate, logged disturbances (pauses, clogs, sheath or fluidics changes) and their time stamps | nothing in the data marks an anomaly independently |
| Saturation | Definitional only: values at `$PnR − 1` (`just parity-real` margins, `REPORT.md` §2.10) | checks the implementation, not whether saturation should remove an event |
| Weak positives | FMO controls and titrations, or dim beads, for the dim markers | the CD19 attenuation is a counterfactual on generated data |

Each domain needs at least two independent datasets for derivation and one
held out (`POLICY_PROTOCOL`). Register them in `parity/real/datasets.json`
with SHA-256 pins, license, provenance and a `cytiq.qc.truth.v1` table whose
descriptor states evidence, selection, channels, annotators and agreement.

## CytoScan novelty benchmark

`cytiq cytoscan`'s novelty flag thresholds a reference-fitted mclust mixture
at the 5% quantile of the reference files' own log-densities. This benchmark
measures what that decision does on independent normal files, how much it
depends on the algorithm seed, and whether a predeclared alternative is
better. It changes no default output: an arm that the frozen rule makes
eligible becomes an opt-in, versioned production mode.

```bash
just novelty-benchmark ci                                   # every arm and check, 3 replicates
just novelty-benchmark development target/scientific/novelty-dev --jobs 6 --with-r-prior
just novelty-benchmark confirmation target/scientific/novelty-confirmation --jobs 6 --baseline-bin "$BASELINE_BIN"
```

**Cohorts.** Every replicate draws a fresh reference pool (64 files), 40
held-out normal test files and 10 anomalous files (two each of `gain_shift`,
`wrong_spill`, `population_loss`, `compression`, `debris`) from
`parity/gen/cytoscan.py`, 5,000 events each. Each role has its own
`SeedSequence` stream and each file its own spawned generator, so file `i`
does not depend on the pool size and reference sizes are nested prefixes.
Generator seeds are predeclared in `novelty_scenarios.py`: development
`20261201–20261220` (CI: the first three) and confirmation
`20261301–20261350`, disjoint from each other and from every other
generator seed in this repository. Replicates are the independent units:
intervals are percentile bootstraps over replicates (2,000 resamples, seed
`20261119`); pooled Wilson intervals, which ignore that test files of one
replicate share a reference model, are secondary.

| Family | Role | Cells |
| --- | --- | --- |
| `exchangeable` | decision | summary, quantiles × reference 8/16/20/32/64 × 6 channels |
| `dimension` | decision (calibration) | quantiles × 2 and 4 channels × reference 20/64 |
| `large_events` | decision (stability) | 20,000-event files, summary, quantiles × reference 20/32 |
| `batch_shift` | descriptive | normal test files from another batch: gain +2.5/5/10/20%, true spillover ×1.5 under the nominal `SPILL`, noise SD 40; paired latent draws |
| `aggregate_methods` | descriptive | emd, binning, fingerprint × reference 20/64 |
| real donors | descriptive | the four unstimulated donors, leave one out, 5 methods × 4 / 10 type markers |
| contract | check | a missing channel and, with `--strict`, missing spillover are refused (exit 2), not scored |

**Arms.** Every decision uses seed 42; flag agreement uses
`--stability-seeds` (seeds `42 + j·2^32`, which share no isolation tree).

| Arm | What changes | Status in rule v2 |
| --- | --- | --- |
| `baseline` | production, `--stability-seeds 10` | reference |
| `cross_conformal` | the mclust model of each of `K = min(n, 10)` folds (`i mod K` over the resolved reference order, aggregates rebuilt per fold) scores its held-out references and every test file; `c = Σ_k #{i ∈ F_k : ll_k(i) ≤ ll_k(t)}`; novel iff `20 (1 + c) ≤ n + 1`; abstains for `n < 19` | calibration candidate |
| `seed_ensemble_5` | novel iff the mean margin over seeds 0–4 is ≤ 0; stability = disagreement with seeds 5–9 | stability candidate |
| `events_2500`, `events_5000` | `CytoScanConfig::events_per_file` through `examples/cytoscan_events.rs` | stability candidates |
| `shrinkage_ledoit_wolf+cross_conformal` | one Gaussian with Ledoit–Wolf shrunk covariance (scikit-learn) on production features, computed per seed, with the same folds and integer rule | calibration candidate (from v2) |
| `cross_conformal_ensemble_5`, `shrinkage_ledoit_wolf`, `pca_gaussian`, `pca_gaussian+cross_conformal`, `mclust_prior` (R `priorControl()`, development only) | research models on production features | never promoted |

The cross-conformal arm is composed from ordinary production runs
(`--reference` = the training folds, `--input` = held-out references and
test files), so a production implementation can be checked against it
exactly.

**Metrics.** Per cell and arm: decision rate (abstentions and fit failures
stay in the denominator), mean FPR on held-out normals with its interval,
P50/P90/max of the per-replicate FPR and `P(FPR > 0.10)` next to the exact
beta-binomial value a perfect split-conformal rule would give, detection
per anomaly class, AUROC, pairwise seed disagreement (all files and normals
only), unanimity, Fleiss κ, the per-file SD of the novelty margin across
seeds, paired FPR differences to the baseline, flag rates per batch shift,
and whether the reference-only out-of-fold exceedance of the in-sample
threshold predicts the held-out FPR.

**Frozen rule.** `DECISION_RULE` (hashed into every report and pinned by
`test_novelty_benchmark.py`; `table` refuses reports under another rule)
decides on the confirmation profile only; any other profile makes every
arm ineligible. The calibration claim needs, in every summary/quantiles
cell of `exchangeable` and `dimension`: abstention below 19 references; at
20 or more, decision rate ≥ 0.95, mean FPR ≤ 0.07 and its upper bound ≤
0.10, a flip rate on normals ≤ 0.10; and in
`exchangeable` cells with ≥ 32 references, detection ≥ 0.80 for every
anomaly class. The stability claim needs, in every cell where the
baseline's flip rate is at least 0.01, half the baseline's flip rate, a
paired FPR difference whose upper bound is ≤ +0.02, detection within 0.05
of the baseline at ≥ 32 references and the baseline's decision rate within
0.02. Every applicable cell must pass (an intersection-union test, so no
multiplicity correction), and the equivalence checks must pass.
`--profile confirmation` refuses a modified benchmark source tree.

Rule v2 replaced v1 after the development profile and before any
confirmation seed was used; `DECISION_RULE["history"]` records both
changes and v1's hash. In development every calibrated arm failed only v1's
relative flip bound (`≤ baseline + 0.02`), which a baseline that flags every
normal file (0 flips) makes unattainable; v2 bounds flips absolutely. The
mclust cross-conformal arm lost detection at ≥ 32 references
(min 0.42–0.60 per class) where the Ledoit–Wolf arm kept it (1.00), so v2
makes `shrinkage_ledoit_wolf+cross_conformal` a calibration candidate.

**Equivalence checks** (any mismatch fails the run): `--stability-seeds`
rows equal separate `--seed` runs; its summary and features equal a run
without it; the example binary at 1,000 events equals the CLI byte for byte;
production's `pairwise_disagreement` equals the benchmark's; with
`--baseline-bin`, the default summary and features equal clean main's byte
for byte (also under `--stability-seeds`), the parameters and every existing
diagnostic are equal, and the run-time ratio is reported.

**Limitations.** FPR claims rest on generated cohorts (a two-normal mixture
per channel): they show what the decision rule does under exchangeability
and known shifts, not the false-positive rate of a clinical cohort. The four
real donors have 438–881 events and no spillover keyword, so no method
subsamples and every seed is identical; they are reported per donor, their
population FPR is `not_assessed`, and leave-one-out splits are not
independent replications. A reference must come from the same instrument,
panel and processing as the test files: the batch family shows how quickly
novelty reacts to a shift, which is a property of the question, not a false
positive to be calibrated away. Results are in
[`REPORT.md` §9](../REPORT.md#9-cytoscan-novelty-stability-and-calibration-2026-11-19).

## Required real data

None of the following is in a pinned dataset; until it is, AF and compensation
uncertainty claims stay within-acquisition or synthetic. New data must be
registered in `parity/real/datasets.json` with SHA-256 pins, license and
provenance (a `repeat_controls` chain for compensation), and frozen before any
acceptance bound is derived from it.

| Purpose | Minimum | Why |
| --- | --- | --- |
| Instruments | ≥ 2 instruments or sites; raw linear FCS (no applied `$SPILL`, no unmixing) with `$CYT`, `$CYTSN`, `$PnV`/`$PnG`, acquisition dates, reagent lots, protocol, fixation | between-instrument variation and lot effects are otherwise confounded |
| Matched repeat single stains | per dye ≥ 6 independently stained tubes in one session (relative SE of an SD ≈ 1/√(2(K−1)) ≈ 32%), plus ≥ 3 on another day at the same nominal voltages; ≥ 2 lots for tandems; cell and bead controls with ≥ 50,000 / ≥ 10,000 events | separates within-tube sampling, between-tube, between-day and lot variance; bootstrap intervals only cover the first |
| Unstained / AF controls | ≥ 3 unstained tubes per donor, ≥ 3 donors per tissue or cell type | AF donor variability, which `af_sensitivity.py` cannot measure |
| Identifiable multi-AF | sorted or lineage-gated unstained subsets (lymphocytes, monocytes, granulocytes) from the same donors | identifies AF components by an independent variable instead of by clustering the spectra being estimated |
| Second spectral dataset | an independent real spectral control set (different instrument, lab or tissue) with a stained sample | the AF decision rule requires ≥ 2 independent real datasets |
| Stained samples | fully stained samples with FMO controls for dim and co-expressed markers | weak-positive and co-expression truth that unstained spike-ins cannot give |
| Detector noise (any WLS/Poisson work) | per-detector calibration at the acquisition voltages: multi-intensity beads, LED pulser (Q, B), blank runs | WLS needs detector variances, Poisson needs count scale; neither is inferred from ADC values |

What each kind of evidence supports: within-acquisition bootstrap supports
sampling-noise statements only; between-tube, between-day and between-lot
claims need replicates across exactly those factors; synthetic results support
none of them.

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
