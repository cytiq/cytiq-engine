# Parity report: cytiq (Rust) vs the original tools

Date: 2026-10-22 · branch `parity-fixes` · baseline `main` (`0f4c72a`)
Updated 2026-10-24 (Stage 3): end-to-end chain (§2.9), real acquisitions (§2.10)
and pass/fail tolerances (§1, step 5); real single-stain controls from a
FACSCelesta and a FACSymphony (§2.10); the raw margin mask without flowCore's
−111 floor (§2.9, §3).
Updated 2026-10-25: `cytoscan` novelty ports mclust's default model search
(§2.7); AutoSpectral's own Aurora example as the first real spectral data,
and `autospectral` spectra built as AutoSpectral builds them (§2.3, §2.10).
Updated 2026-10-27: `spillqc`, a port of CompensAID's spillover-error
detection with flowDensity's `deGate` and flowCore's `estimateLogicle` (§2.11),
and a benchmark of what it detects on real data (§2.12).
Updated 2026-11-19: CytoScan novelty stability and calibration (§9): the
upstream novelty threshold flags 32–100% of independent synthetic normal
files; flag assessments, `--stability-seeds` and an opt-in calibrated
`--novelty-mode conformal-ledoit-wolf-v1` chosen by a pre-registered rule.

## TL;DR

| Tool | Upstream | Before fixes | After fixes |
|---|---|---|---|
| `transform` arcsinh | flowCore `arcsinhTransform` | ✅ matches (Δ ≤ 4e-7, f32 precision) | ✅ unchanged |
| `transform` logicle | flowCore `logicleTransform` | ❌ different function (Δ = 0.93 over a 5.5 range) | ✅ Δ ≤ 2.4e-7 |
| `transform` hyperlog | flowCore `hyperlogtGml2`, FlowKit | ❌ different function (Δ = 3.0 over a 1.2 range) | ✅ Δ ≤ 3.4e-8 |
| `transform` biexponential | flowWorkspace `flowjo_biexp` | ❌ different function, f(−1000) = −420 instead of 10.3 | ✅ Δ ≤ 7.6e-6 channels |
| `autospill` | autospill 0.2.0 | ⚠️ matrix close (max Δ 3.8e-4), gates 18% wider | ✅ matrix Δ ≤ 4.8e-13, identical gates |
| `autospread` | — (no upstream) | — | — (regression test only) |
| `autospectral` | AutoSpectral 1.8.7 (OLS) | ❌ abundances inflated 1.39–1.60× (L2 normalisation); on real Aurora controls spectra down to cosine 0.50 against AutoSpectral | ✅ scale 0.992–1.0002, RMSE within 4% of upstream on the true spectra; real Aurora fluorophore spectra cosine ≥ 0.9998 |
| `peacoqc` | PeacoQC 1.20.0 | ❌ 82–85% mask agreement, IT precision 0.04; 1.5× slower than R | ✅ 100% mask agreement in every mode; 11× faster than R |
| `cytoscan` | CytoScan 0.1.0 (`c97f403`) | ❌ binning off by up to 0.004, sample depends on path spelling, outlier scores 3× outside isotree noise, novelty model unlike mclust | ✅ features = R to 5e-13 on identical events; outlier flags = R; novelty ports `Mclust`'s default search: on the same features R picks the same model, G and flags in 12 of 15 cases (the 3 others are a rounding-singular fit cytiq rejects) |
| `cytoscan` novelty calibration (§9) | — (benchmark) | — | upstream threshold: FPR 0.32–1.00 on held-out synthetic normals; opt-in `conformal-ledoit-wolf-v1`: FPR 0.027–0.053, every anomaly class detected from 32 references; defaults byte-identical |
| `flowsom` | FlowSOM 2.18.0 | ⚠️ ARI vs R 0.87–0.91 (hard set), 9× slower than R | ✅ ARI vs R 0.93–0.98 (R's own seed-to-seed level), ~8× faster than R |
| `gate` | FlowKit 1.3.0 `GatingStrategy.gate_sample` | — (new command) | ✅ identical per-event membership for all 11 gates; identical counts at 1M events, ~13× faster than FlowKit |
| whole chain (§2.9) | flowCore + PeacoQC + FlowKit + FlowSOM, same steps | — (new) | ✅ every stage equals upstream per event on 200,000 golden events (values Δ ≤ 1e-7, masks and gates identical, FlowSOM ARI vs R 0.999), except the margin mask, which deliberately drops flowCore's −111 floor; against the truth: final mask F1 0.971, every population incl. a 0.2% rare one found with precision ≥ 0.997 |
| `spillqc` (§2.11) | CompensAID 0.99.7 (`db74803`) after its vignette chain, flowDensity 1.44 | — (new command) | ✅ on CompensAID's own example (raw and preprocessed) and three synthetic tubes, every cut-off, segment edge, count and merge message equals R to 7e-15, every SSI and flag is identical, the same events survive margins and PeacoQC, and both injected spillover errors are found by both tools |
| QC artifact truth (§8) | — (new contract and benchmark) | — | ✅ `qc --artifact-truth` equals an independent NumPy oracle exactly on 14 generated scenarios × 10 seeds; filtering identical to clean main; real-data removal accuracy and acceptance bounds `not_assessed` (no independent truth) |
| real data (§2.10) | flowCore, PeacoQC, FlowKit, FlowSOM, FlowJo labels, autospill, AutoSpectral | — (new) | ✅ Aurora spectral panel: fluorophore spectra cosine ≥ 0.9998 against AutoSpectral, unmixing solver equal to 4e-8; compensation/logicle Δ ≤ 8e-8, PeacoQC identical, manual Gating-ML identical to FlowKit on all 15 gates; populations match FlowJo with Jaccard 0.98–0.996 (NK T 0.86, a FlowJo export effect); AutoSpill on FACSCelesta (11 colours) and FACSymphony (24 colours) controls: max \|Δ\| 5e-13, gates identical |

**44 deviations** from the upstream algorithms were found and fixed (8 of them in `cytoscan`), plus
two performance defects (section 2.6). The Stage 3 chain validation found two
more defects, both fixed: raw margin masks removed every event at a channel's
observed extremes (half of a bead sample), and FCS output lacked HEADER DATA
offsets, so strict readers such as FlowKit rejected it. Real FACSDiva
controls found a third: `cytiq autospill` did not cap values above `$PnR` as
`flowCore::read.FCS` does, which moved one row of a real matrix by 1.3e-4. A
real FACSCelesta sample showed that the low bound of the raw margin mask,
flowCore's fixed −111, flags the ordinary noise of negative populations (31%
of the sample); the mask now uses the upper detector limit only. All key
ports (`stats::density`, `smooth.spline`, logicle, hyperlog, `flowjo_biexp`)
are pinned by unit tests against reference values produced by R.

## 1. Method

1. **Static review.** Each crate was read line by line and compared with the
   upstream sources of the installed packages (`autospill`, `MASS`,
   `FlowSOM` + `src/som.c`, `PeacoQC`, `flowCore`, `flowWorkspace`/`cytolib`,
   `AutoSpectral`, `stats::density`/`smooth.spline` from `r-source`).
2. **Synthetic data with known ground truth** (`parity/gen/generate.py`,
   fixed seed `20261022`):

   | Dataset | Contents | Ground truth |
   |---|---|---|
   | `transform` | grid of 999 values from −1000 to 262144 | — |
   | `autospill` | 6 single-colour controls × 20,000 events, 15% debris, Poisson + electronic noise | 6×6 spillover matrix, debris mask |
   | `autospectral` | 16 detectors, 6 fluorophores + AF, 20,000-event controls, 30,000-event sample | spectra (peak = 1), per-event abundances |
   | `peacoqc` | 200,000 events, 6 channels, clog (4%), drift (6%), burst (1%) | bad-event mask (9.8%) |
   | `flowsom` | 100,000 × 10D, 15 well-separated populations | labels |
   | `flowsom_hard` | 100,000 × 10D, 20 overlapping populations, rare ones (≥0.2%) | labels |
   | `pipeline` (`gen/pipeline.py`, seed `20261024`) | 200,000 raw events, 6×6 spillover, 7 populations (one 0.3%), debris, doublets, dead cells, an unstable interval, saturation | per-event population and artefact flags |
   | `pipeline_beads` | 3,000 events of one discrete bead population (zero-MAD scatter) | — |

3. **Runs.** Rust (`parity/run_rust.sh`) for the current branch and for `main`
   as the baseline; the upstream R tools (`parity/run_upstream.R`) with the
   same parameters. FlowSOM in R runs with `nClus = 20`, `scale = FALSE/TRUE`,
   seeds 1–3 (R's RNG cannot be reproduced in Rust, so the comparison is
   statistical).
4. **Metrics** (`parity/compare.py` → `parity/results/*.json`): max |Δ| for
   transforms; MAE/max |Δ|/Frobenius for matrices; RMSE/scale/correlation for
   unmixing; precision/recall/Jaccard/agreement for QC masks; ARI/NMI/purity
   for clustering; wall time and peak memory (`/usr/bin/time -l`).
5. **Tolerances.** `parity/tolerances.json` (and `parity/real/tolerances.json`
   for §2.10) turns the results into pass/fail rules, each with its reason;
   `compare.py` exits with 1 on any violation or on a rule that matches no
   metric, so `parity/run_all.sh` fails. The same chain also runs in CI on a
   15,000-event golden fixture (`crates/cytiq-engine/tests/pipeline_cli.rs`,
   thresholds in `data/fixtures/pipeline/thresholds.json`), and a smoke test
   checks autospill, AutoSpectral, PeacoQC and CytoScan against R outputs
   committed with small synthetic datasets (`data/fixtures/parity`,
   `crates/cytiq-engine/tests/parity_smoke.rs`), with no R in CI.

Environment (`parity/versions.txt`): R 4.5.3, flowCore 2.22.1, flowWorkspace
4.22.1, FlowSOM 2.18.0, PeacoQC 1.20.0, autospill 0.2.0 (`1e60e86`),
AutoSpectral 1.8.7 (`292dfea`), MASS 7.3.66, FlowKit 1.3.0, rustc 1.99.1.
Apple Silicon, macOS.

## 2. Results by tool

### 2.1 `transform`

Maximum deviation of Rust from the reference over the 999-point grid:

| Method | Reference | Before | After |
|---|---|---|---|
| arcsinh (cofactor 150) | flowCore `arcsinhTransform(b = 1/150)` | 4.4e-7 | 4.4e-7 |
| logicle (T=262144, W=0.5, M=4.5, A=0) | flowCore `logicleTransform` (0..M scale) | **0.934** | 2.4e-7 |
| logicle | FlowKit / Gating-ML (0..1 scale) | 3.30 | ×M of flowCore; 2e-7 after affine alignment |
| hyperlog | flowCore `hyperlogtGml2` | **3.00** | 2.9e-8 |
| hyperlog | FlowKit `HyperlogTransform` | 3.00 | 3.4e-8 |
| biexponential | flowWorkspace `flowjo_biexp(256, 262144, 4.4185, 0, −100)` | **430** | 7.6e-6 |

Residual deviations of ~1e-7 come from writing FCS as float32.

**What was wrong.**
- Logicle was `asinh(x/s)/ln10` instead of the Parks logicle: f(0) = 0
  instead of 0.5, f(T) = 4.30 instead of 4.5.
- Parameter A compressed the negative range instead of adding decades.
- Hyperlog was a signed `log10(1 + |x|/s)`.
- Biexponential was a two-sided log1p whose negative-branch gain was 4×
  the positive one: f(−1000) = −419.8, f(+1000) = 106.2. Output went
  negative, whereas FlowJo output lies in [0, 256] with 0 at channel 57.

**Fix.** New module `crates/cytiq-transform/src/scales.rs`:
- `Logicle`: Moore–Parks 2012, with an inverse and validation of the
  constraints `2W ≤ M`, `−W ≤ A ≤ M − 2W`.
- `Hyperlog`: Gating-ML.
- `FlowJoBiexp`: a port of `cytolib::biexpTrans::computCalTbl`, including
  the float divisions and `abs((int64_t)…)` in `logRoot`, plus R's natural
  spline (`natural_spline`/`spline_eval`).

### 2.2 `autospill`

| Metric | Before | After | R autospill |
|---|---|---|---|
| Spillover vs truth, MAE | 1.68e-4 | 5.45e-5 | 5.45e-5 |
| Spillover vs truth, max \|Δ\| | 4.82e-4 | 2.17e-4 | 2.17e-4 |
| Rust vs R, max \|Δ\| | 3.83e-4 | **4.8e-13** | — |
| Rust vs R, ‖ΔC‖_F (compensation) | 9.7e-4 | 1.6e-12 | — |
| Gated events, BV421 (of 17,000 non-debris) | 13,954 | 11,837 | 11,837 |
| Refinement iterations | 5 | 3 | — |

After the fixes the gates of all 6 controls match R event for event. Neither
Rust nor R lets any debris into the gate.

**Fixed:**
1. KDE kernel width: σ = h, but `MASS::kde2d` uses h/4. The kernel was 4×
   too wide and the density oversmoothed (`gating.rs`).
2. The KDE grid spanned the trim/MAD box instead of the range of the points
   passed in.
3. FSC/SSC were not clamped to [0, max(`$P1R`)] before trimming.
4. When every peak lay in the lower-left corner, Rust silently took peak 0,
   while R stops with an error.
5. Stale `delta_change` in the refinement state machine: R resets it to −1
   when switching linear → bi-exp and λ 1 → 0.1. Without the reset, a plateau
   on the linear scale produced `NonConvergent` (`runner.rs`).
6. The refinement bi-exp transform is now `flowjo_biexp`, as in
   `autospill::get.transformation`.
7. The `rlm` scale is now `median(|r|)/0.6745` (as in MASS) instead of a
   centred MAD.
8. `rlm` convergence: now the `irls.delta ≤ 1e-4` criterion, falling back
   to OLS when it does not converge, as in `fit.robust.linear.model`.
9. Linear-stage skewness is computed on the trimmed data.
10. `trim_n` now rounds half to even.

**AutoSpread.** Neither `autospill` nor `AutoSpectral` computes a spread
matrix: AutoSpectral only consumes one, in σ²/F units and in
fluorophore × fluorophore layout. The Rust implementation is therefore a
Nguyen-2013-style heuristic without a reference. After the gate fix the
coefficients on the synthetic set moved by 4% at the median and 24% at most.
The golden file `data/fixtures/autospread/expected_spread.csv` was
regenerated.

### 2.3 `autospectral`

| Fluorophore | Scale vs truth: before | after | RMSE: before | after | RMSE: R `unmix.ols` (true spectra) |
|---|---|---|---|---|---|
| BV421 | 1.396 | 0.992 | 6217 | 862 | 860 |
| BV510 | 1.548 | 1.000 | 8553 | 341 | 339 |
| FITC | 1.439 | 1.000 | 6710 | 263 | 263 |
| PE | 1.540 | 1.000 | 8330 | 130 | 130 |
| PE-Cy5 | 1.604 | 1.000 | 9275 | 119 | 119 |
| APC | 1.393 | 1.000 | 5993 | 89 | 89 |

- Spectrum shapes matched the truth even before the fixes (cos = 1.0, max
  |Δ| after peak normalisation ≤ 5e-4). Only the scale was wrong.
- Independent solver check: R `unmix.ols` applied to the spectra extracted
  by Rust gives the same abundances as Rust (relative |Δ| ≤ 5e-8).

**Fixed:**
- L2 spectrum normalisation replaced by peak (L∞) normalisation, as in
  `get.fluorophore.spectra`. The AF spectrum is divided by `max |x|`.
- Positive events are now selected as in R: above the 99th percentile of
  the negative (AF) control on the primary channel, top 500, at least 20.
  Previously the top 20% of the control itself was used.
- Quantiles are now R type 7 instead of nearest rank.
- The `max(0)` background clamp was removed.
- A warning is emitted when the condition number (fluorophores only)
  exceeds the number of fluorophores, as in `calculate.condition.number`.

**Real data (2026-10-25, §2.10)** showed that the mean difference against
the whole unstained control does not survive real cell controls: the
autofluorescence of a different cell mix and saturated debris in the 1–99%
box entered every spectrum (cosine 0.50–0.99 against AutoSpectral). Now:
- each spectrum is `get.fluorophore.spectra`'s: on every detector the slope of
  `MASS::rlm` (Huber, MAD scale, 100 steps, `lm` when it does not converge)
  against the peak detector, divided by the largest; pinned against `rlm` by
  unit tests, the non-converging case included;
- over the positives and their negatives: the unstained control's 3 nearest
  events on raw FSC/SSC (`clean.controls`' scatter matching, at most 1,000);
- the AF spectrum is the same slopes over the gated unstained control;
- controls are cleaned by default (detector margins, the AutoSpill KDE gate);
  `--quantile-gate` keeps the box.

On the synthetic set the spectra stay at max |Δ| ≤ 4.5e-4 after peak
normalisation; abundance RMSE is within 4% of R `unmix.ols` on the true
spectra (BV510 352 against 339, BV421 880 against 860, the others within
1.1%), and the scale against the truth is 0.992–1.0002.

### 2.4 `peacoqc`

| Mode | Metric | Before | After | R PeacoQC |
|---|---|---|---|---|
| all | fraction removed | 27.8% | 20.5% | 20.5% |
| all | mask agreement with R / Jaccard of removed | 83.8% / 0.50 | **100% / 1.00** | — |
| MAD | agreement / Jaccard | 85.3% / 0.50 | **100% / 1.00** | — |
| IT | precision vs truth | **0.036** | 1.00 | 1.00 |
| IT | agreement / Jaccard | 82.3% / 0.00 | **100% / 1.00** | — |
| — | number of bins | 399 | 400 | 400 |
| — | time, 200k events × 6 channels | 2.30 s | **0.14 s** | 1.55 s |

Recall of bad events is the same for Rust and R: 1.0 in `all` mode.
Precision in `all` mode is 0.48 for both: upstream with `MAD = 6` is
aggressive on this synthetic set.

**Fixed:**
1. The peak detector was a histogram with [1,2,1] smoothing. It is now a
   port of `stats::density`: bw.nrd0, `BinDist`, convolution, `approx`.
   Matches R to 1e-9.
2. MAD smoothing was a 5-point moving average. It is now a port of
   `smooth.spline(spar = 0.5)`, including `.nknots.smspl` and the `0.3330`
   constant from `sgram.f`. Matches R to 1e-6.
3. `SplitWithOverlap` dropped the last bin.
4. IT picked the last column whose gain exceeded the limit instead of the
   best one.
5. On equal leaf sizes IT took the last leaf, while `which.max` takes the
   first.
6. `RemoveShortRegions` did not remove short good runs at the file edges.
7. `RemoveMargins`: the `max(0, …)` lower threshold removed every event ≤ 0
   in compensated data; the upper threshold was `$PnR` instead of
   `$PnR − 1`.
8. `FindEventsPerBin` used integer division with `remove_zeros`.
9. The single-peak cluster centre was not computed over all peaks.
10. The `TooSmallClusters` denominator was wrong.

The CLI test fixture of 32 events (a single bin) was expanded to 1,600
events. Under R semantics a file with fewer than 5 bins is removed entirely.

### 2.5 `flowsom`

| Dataset | Metric | Before | After | R FlowSOM (3 seeds × scale F/T) |
|---|---|---|---|---|
| flowsom (easy) | ARI vs truth | 0.987 | 1.000 | 0.9997–1.000 |
| flowsom_hard | ARI vs truth | **0.885** | 0.953 | 0.948–0.966 |
| flowsom_hard | NMI vs truth | 0.903 | 0.961 | 0.949–0.963 |
| flowsom_hard | ARI Rust vs R | 0.87–0.91 | **0.93–0.98** | R seed-to-seed: 0.93–0.95 |
| — | time, 100k × 10D | **36.7 s** | 0.52 s | 4.0–4.4 s |

After the fixes Rust differs from R no more than R differs from itself when
the seed changes.

**Fixed (checked against `som.c`):**
- Radius: 5 → 1 per epoch replaced by 6 → 0 per step (0.67 quantile of grid
  distances). Below 1 only the BMU is updated (threshold 0.5).
- Grid distance: Euclidean replaced by Chebyshev (`method = "maximum"`).
- Gaussian neighbourhood replaced by bubble: full α inside the radius.
- The α schedule is linear per step, not per epoch.
- Events are drawn at random with replacement (`rlen · n` steps) instead of
  in file order.
- Initial codes are sampled without replacement; the generator uses the top
  53 bits instead of `% n` on the LCG's low bits.
- The scaling SD is computed as in `base::scale` (n − 1).
- Removed the per-event rayon dispatch inside SOM training, which caused
  the ~60× slowdown.

**Deliberately unchanged** (documented in the README as fixed MVP choices):
- `scale = TRUE` by default (R: `FALSE`);
- 20 metaclusters (R: `nClus = 10`);
- metaclustering by plain average linkage instead of ConsensusClusterPlus.

This does not affect the metrics: Rust-vs-R ARI is in the same range for
scale T and F.

### 2.6 Performance

Wall time on the parity datasets (Apple Silicon, all cores):

| Command | Before (`main`) | After | Upstream R |
|---|---|---|---|
| `peacoqc` (200k events, mode `all`) | 2.30 s | **0.14 s** | 1.55 s |
| `flowsom` (100k × 10D) | 36.5 s | **0.52 s** | 4.0 s |
| `autospectral` (6 controls + 30k sample) | 0.53 s | **0.03 s** | — |
| `autospill` (6 controls × 20k) | 2.31 s | 2.19 s | 8.83 s |

**Fixed:**
1. **Unbuffered FCS writer** (`cytiq-fcs/src/writer.rs`). The data segment
   was written straight to `File` one float at a time: one `write` syscall
   per event and channel (2M syscalls for the PeacoQC output). Profiling
   showed ~70% of PeacoQC time in `write`, with sys time 2.4 s against user
   time 0.85 s, independent of `--threads`. Output now goes through a
   `BufWriter` with an explicit `flush`, so I/O errors still surface. This
   speeds up every command that writes FCS (`peacoqc`, `transform`,
   `autospectral`).
2. **`stats::density` used an O(n²) direct convolution**
   (`cytiq-peacoqc/src/density.rs`). It is now an FFT convolution
   (in-crate radix-2, 1024 points, no new dependency), the same
   `fft(y) * Conj(fft(kords))` scheme R uses. The R reference unit tests
   still pass at 1e-9, and the PeacoQC masks are still identical to R.
3. Per-event rayon dispatch inside FlowSOM SOM training (section 2.5).

The new Rust 1.99 toolchain (CI installs `@stable`) adds clippy lints that
pre-existing code in `cytiq-fcs` failed (`with_capacity(0)`, `chunks_exact(2)`
→ `as_chunks`). These were fixed so `clippy -D warnings` stays green.

### 2.7 `cytoscan`

Upstream: CytoScan 0.1.0 (`AUMC-HEMA/CytoScan@c97f403`) with isotree 0.6.1.5,
mclust 6.1.3, transport 0.15.4, flowFP 1.68.0. Baseline: the cytoscan branch
before these fixes (`7b1e2f6`).

**2026-10-26 dependency migration.** `rand` 0.10.3 and `rand_distr` 0.6.0 are
updated together. The `StdRng` stream and floating-point uniform draws are
unchanged, but integer range sampling changed. `sampling.rs` preserves rand
0.8's event selections and tree-feature choices; golden tests cover every
sampling algorithm, and 720 independent comparisons against rand 0.8.8 match
exactly. On all 15 dataset/method combinations below, feature values and
outlier scores are identical to the pre-upgrade outputs (maximum absolute
change 0). The statistical tolerances and upstream fixtures are unchanged.
All 15 comparisons against R satisfy the existing CytoScan tolerance rules.

**Datasets** (`parity/gen/cytoscan.py`):
- Cohorts follow `CytoScan::generateDemo`: 6 channels, each a two-normal
  mixture.
- Every file carries `$PnR` and a non-trivial `SPILL`.
- Anomalies were injected into known test files: gain shift, wrong `SPILL`,
  population loss, signal compression and debris.

| Dataset | Test / reference files | Events per file | Purpose |
|---|---|---|---|
| `cytoscan_exact` | 10 / 10 (3 anomalous) | 1,000 | Both tools use every event (`which.lines = 1000`, `nAgg = 1000`), so features compare without subsampling |
| `cytoscan` | 20 / 20 (5 anomalous) | 20,000 | Realistic cohort; subsamples differ, so the comparison is statistical |
| `cytoscan_ref100` | 20 / 100 (5 anomalous) | 20,000 | More reference files than features (the full-covariance regime) |

**Features.** Max |Δ| Rust vs R.

| Method | `cytoscan_exact` before | after | `cytoscan` mean \|Δ\| after | R subsample noise (seed 42 vs 1, 2) |
|---|---|---|---|---|
| summary | 5.0e-13 | 5.0e-13 | 0.103 | 0.095 |
| quantiles | 5.0e-13 | 5.0e-13 | 0.112 | 0.104 |
| EMD | 5.2e-13 | 5.2e-13 | 0.077 | 0.073 |
| binning | **4.4e-3** | 5.0e-13 | 0.011 | 0.011 |
| fingerprint | 0 | 0 | 0.021 | 0.021 |

- On identical events all five feature methods match upstream to rounding.
- On subsampled files the Rust–R difference equals R's own difference between
  subsample seeds. The draws come from `rand` rather than R's Mersenne
  Twister, so they are not bit-identical.

**Outlier (isotree).**
- Monte Carlo noise: isotree seeds tree `t` with `seed + t`, so seeds 42/43/44
  share 999 of 1000 trees. The noise band is therefore measured with
  independent seeds 42/1042/2042: 0.009–0.017 in score.

| Dataset | max \|Δscore\| before → after | isotree noise | flags agree with R | Rust TP/FP | R TP/FP |
|---|---|---|---|---|---|
| `cytoscan_exact` (5 methods) | 0.011–0.036 → **0.005–0.012** | 0.009–0.014 | 100% | 2–3 / 0 | 2–3 / 0 |
| `cytoscan` | 0.029–0.070 → 0.021–0.080 | 0.007–0.014 | 95–100% | 3–5 / 0 | 3–5 / 0 |
| `cytoscan_ref100` | 0.029–0.067 → 0.021–0.056 | 0.008–0.017 | 95–100% | 4–5 / 0 | 3–5 / 0–1 |

- On identical features the scores are now within isotree's own noise.
- On subsampled cohorts the larger Δ comes from the different feature values,
  not from the forest.

**Novelty (GMM).**
- Upstream calls `Mclust(refFeatures)` with mclust's defaults: G = 1..9, every
  covariance structure, best BIC. With no more reference files than features
  mclust only tries the spherical and diagonal structures (`EII`, `VII`,
  `EEI`, `VEI`, `EVI`, `VVI`) and starts EM from `hc(modelName = "EII", use =
  "SVD")`, Ward's clustering of the scaled, SVD-whitened features.
- `crates/cytiq-cytoscan/src/mixture.rs` ports that search: the closed-form
  one-component fits, `me*` EM with mclust's singularity rules, the `hcEII`
  start and `pickBIC`'s tie order. It adds `EEE` and `VVV` with more files
  than features; mclust's other six full-covariance structures are not
  ported, and there mclust starts from `hcVVV`.
- One deliberate difference: mclust keeps a one-component full covariance
  that is singular up to rounding (`mvnXXX` only rejects an exact zero
  pivot). Binning and fingerprint features sum to 1 per channel, so their
  covariance is singular, the BIC of that fit is rounding noise, and mclust
  then flags every test file. cytiq applies the `G >= 2` rule (`rc <=
  sqrt(eps)`) to it too.
- Unit tests against mclust: the BIC of every structure and G (n > d with the
  `hcEII` start, and n <= d) to 1e-6, the `hclass` partitions, the chosen
  model and its densities.
- `parity/run_upstream.R` also fits Mclust on cytiq's own features
  (`upstream_on_rust_features`), which separates the model search from the
  subsample difference. It found a comparison defect: CytoScan's
  `log(dens())` is −Inf where `dens()` underflows, which `compare.py` had
  read as "Mclust returned no model" (the earlier "fails for 4 of 5 methods"
  on `cytoscan_exact`); the differences now use `dens(logarithm = TRUE)`.

| Dataset | Method | cytiq model | R on R's features: model, flag agreement | R on cytiq's features: model, flag agreement, max \|Δ log-density\| |
|---|---|---|---|---|
| `cytoscan_exact` (n_ref 10) | summary, quantiles, EMD, binning | EEI, G = 9 | EEI, 9; 100% | EEI, 9; 100%; ≤ 0.009 (features read back at 12 decimals) |
| | fingerprint | EII, 9 | EII, 9; 100% | EII, 9; 100%; 4e-12 |
| `cytoscan` (n_ref 20) | summary | EEI, 2 | EEI, 2; 65% | EEI, 2; 100%; 3e-9 |
| | quantiles | VEI, 5 | VEI, 6; 90% | VEI, 5; 100%; 3e-9 |
| | EMD | XII, 1 | XII, 1; 80% | XII, 1; 100%; 4e-11 |
| | binning | VII, 2 | VII, 3; 70% | VII, 2; 100%; 4e-10 |
| | fingerprint | EII, 3 | XXX, 1; 50% | XXX, 1 (rounding-singular); 50% |
| `cytoscan_ref100` (n_ref 100) | summary | XXX, 1 | XXX, 1; 65% | XXX, 1; 100%; 3e-9 |
| | quantiles | XXX, 1 | XXX, 1; 95% | XXX, 1; 100%; 2e-8 |
| | EMD | XII, 1 | XII, 1; 95% | XII, 1; 100%; 8e-11 |
| | binning | VII, 4 | XXX, 1; 30% | XXX, 1 (rounding-singular); 30% |
| | fingerprint | EII, 6 | XXX, 1; 100% | XXX, 1 (rounding-singular); 95% |

- On the same features the search reproduces mclust in 12 of 15 cases; the
  other 3 are the rounding-singular `XXX`. There R flags all 20 test files in
  two of them (precision 0.25) where cytiq flags 10 (recall 0.8, precision
  0.4) and 6 (recall 1.0, precision 0.83); on `cytoscan_ref100` fingerprint
  both flag the same 6.
- Against R's own features the remaining differences come from the
  subsamples (`rand`, not R's Mersenne Twister): with 20 reference files a
  slightly different feature matrix moves G (VEI 5 against 6, VII 2 against 3).
- With no more reference files than features the run warns: only spherical
  and diagonal structures fit, and the 5% threshold rests on a handful of
  log-densities.
- These comparisons measure agreement with R, not calibration. What the
  in-sample 5% threshold does on independent normal files, its seed
  stability, and the opt-in calibrated mode are in §9.

**Fixed:**
1. The subsample used a hand-written LCG seeded with `42 ^ FNV(path)`, so a
   relative and an absolute path gave different samples. It is now `rand`'s
   `StdRng` reseeded with 42 for every file, as `set.seed(42)` in `readInput`.
2. The aggregate was truncated to 10,000 events. It now takes
   `ceiling(10000 / n_files)` from every file, as `getAggregate` does.
3. Binning used `value <= boundary` and kept out-of-range values. It now
   follows `table(cut(x, breaks))`: right-closed bins, and values outside the
   aggregate range are dropped from counts and denominator.
4. Fingerprint is now a port of `flowFP`:
   - `dequantize` (event i gets i·1e-8);
   - one-pass variance for axis choice, first maximum from `DBL_MIN`;
   - median split, with ties going to the lower bin;
   - children numbered 2b / 2b+1;
   - an error on empty bins instead of an index-halving fallback.
5. Isolation forest:
   - `max_depth = ceil(log2 n)`; it was one deeper;
   - `c(n) = 2(H_n − 1)` with exact harmonic numbers; it was the logarithmic
     approximation;
   - column drawn uniformly among non-constant columns without replacement;
   - threshold from `rand_distr::Uniform`;
   - deterministic summation over rayon-parallel trees.
6. Novelty: the self-written z-scored diagonal EM (k ≤ 4) was replaced by
   `linfa-clustering` full-covariance mixtures (since replaced by the mclust
   port above). Features are not z-scored,
   as in upstream, and G is chosen by mclust's BIC (`2 loglik − npar ln n`)
   over G = 1..9. G values with components of ≤ d members are skipped. With
   no valid G it falls back to a single diagonal Gaussian and writes a
   warning.
7. `aggregate_events` in the diagnostics now reports the aggregate size,
   not the total number of events in all files.
8. The README stated the wrong preprocessing order. The code was already
   right: truncate to `$PnR` on read, compensate, then `asinh(x/150)`.

**Time.** Rust takes 0.04–0.3 s per run. R CytoScan takes 0.8–7.3 s for the
features alone.

### 2.8 `gate`

New command, so there is no baseline. The dataset is `parity/gen/gating.py`
(seed `20261023`): 4,000 events with `$SPILLOVER` and `$TIMESTEP`, and 11
events placed exactly on rectangle and polygon boundaries. The Gating-ML 2.0
strategy covers:
- polygon gates, one of them self-intersecting
- range, rectangle, ellipsoid and Boolean (`and`/`or`/`not`, complement) gates
- the `flin`, `flog`, `fasinh`, `logicle` and `hyperlog` transformations
- `FCS` compensation and spectrum-matrix compensation where the dimension is
  named by fluorochrome
- a four-level hierarchy

| Check | Result |
|---|---|
| Per-gate counts vs FlowKit (4,000 events) | 11 / 11 identical |
| Per-event membership vs FlowKit | identical for every gate (`crates/cytiq-gating/tests/flowkit_parity.rs`) |
| Per-gate counts vs FlowKit (same strategy, 1,000,000 events) | 11 / 11 identical |
| Wall time at 1M events | cytiq 0.17 s (includes the FCS read) vs FlowKit 2.2 s (includes `Sample` load) |

Semantics were taken from the FlowKit and flowutils sources, and the
flowutils C polygon routine was checked directly:
- rectangles use `min <= x < max`
- ellipsoids include their boundary
- polygons use Sunday's winding number with `wn % 2 != 0`. Points on
  left/bottom edges are inside and points on right/top edges are outside,
  despite the flowutils docstring, which says edges are inclusive
- raw events get `$PnE`, `$PnG` and `$TIMESTEP` pre-processing, as in flowio
- Boolean gates combine the parent-intersected membership of their references

### 2.9 End-to-end chain

The documented conventional chain runs on the golden `pipeline` data:
`preflight → compensate → transform (arcsinh 150) → peacoqc → clean (raw) →
mask combine → gate --mask → flowsom --mask`. Upstream runs the same steps
(`parity/pipeline.R`): `flowCore::compensate`, `arcsinhTransform`,
`PeacoQC::RemoveMargins` and `PeacoQC`, FlowSOM; FlowKit evaluates the
population strategy on the *raw* file with `FCS` compensation and `fasinh`
inside Gating-ML, and `clean`'s exported gates. Everything is joined per event
through `source_event_id`.

| Stage | cytiq vs upstream | cytiq vs truth |
|---|---|---|
| compensate | max relative Δ 6.0e-08 (f32 storage) | — |
| compensate → transform | max relative Δ 9.1e-08 | — |
| margins (raw) | detector limit 100%; `RemoveMargins` removes every flagged event and 14,810 more (at or below −111) | recall 1.0, no good events lost |
| PeacoQC | 100% mask agreement | recall 1.0, 5.6% of good events (bins around the artefact; R removes the same) |
| clean | 100% agreement with FlowKit on the exported gates | debris 0.9988, doublets 1.0, dead 0.9953 recall; ≤ 0.12% good events lost |
| combined mask | — | F1 0.9706, 0.04% of artefacts kept |
| gates | 0 events differ from FlowKit, every gate | precision ≥ 0.997, recall ≥ 0.998; Rare 429 of 430 |
| FlowSOM (7 metaclusters) | ARI 0.9990 vs R | ARI 0.9996 (R 0.9987) |
| FCS output | opens in flowio without `ignore_offset_discrepancy` | — |

On `pipeline_beads` every stage completes and keeps the beads: the singlet
band uses the relative-spread fallback (area equals height, so MAD and IQR are both zero), the margin mask removes nothing, while
`PeacoQC::RemoveMargins` removes all 3,000 events (it drops every event
equal to a channel's observed minimum or maximum). This found the first Stage 3
defect: cytiq's raw margin masks used that rule too; they now use detector
limits only.

The raw margin mask also used to cut at flowCore's −111 floor, as
`RemoveMargins` does. FCS declares no low detector limit, and on float data
the floor falls inside the noise of negative populations: here it removed
7.4% of the events, including 0.77% of the good ones, and unevenly across
populations (the final mask lost 1.5% of B and NK cells but 0.03% of CD4 T
cells). The mask now flags `$PnR − 1`
only; real FACSDiva data shows the same effect at a larger scale (§2.10).

### 2.10 Real acquisitions

`parity/real/run_real.sh` runs on the datasets of `parity/real/datasets.json`:
samples that ship with the R packages, public acquisitions from BD
instruments downloaded from Zenodo and AutoSpectral's own Cytek Aurora
example from Mendeley Data (checked by SHA-256), plus any files in
`CYTIQ_REAL_DATA` (`parity/real/README.md`).

| Dataset | Check | Result |
|---|---|---|
| FlowSOM `68983.fcs` (19,225 events, 18 colours) | compensate, logicle vs flowCore | Δ 5.9e-08, 7.8e-08 |
| | manual Gating-ML (15 gates, rectangles and polygons) vs FlowKit | 0 events differ |
| | gated populations vs FlowJo labels (Jaccard) | B 0.9959, CD4 T 0.9949, CD8 T 0.9908, γδ T 0.9804, NK 0.9778, NK T 0.8616 |
| | FlowSOM (10 metaclusters) | ARI vs R 0.9395 (R seed to seed 0.9051); vs FlowJo labels 0.9869 (R 0.9718) |
| PeacoQC `111.fcs` (10,000 events, 13 detectors) | compensate, logicle | Δ 5.9e-08, 7.9e-08 |
| | PeacoQC vs R | 100% agreement (12.5% removed) |
| | margins | detector limit 100% (27 events at `$PnR − 1`); `RemoveMargins` removes those and 307 more (observed extremes and values at or below −111) |
| FACSCelesta, 11 single-stain controls (5,000 events each; Zenodo 21384789) | `cytiq autospill` vs R autospill | spillover max \|Δ\| 4.9e-13, ‖ΔC‖_F 4.1e-12; gates identical on all 11 controls |
| FACSCelesta `Myeloid SP_042.fcs` (56,458 events, 11 detectors, FACSDiva compensation) | compensate, logicle | Δ 6.0e-08, 7.9e-08 |
| | PeacoQC vs R | 100% agreement (1.3% removed) |
| | margins | detector limit 100%: 36 events (0.06%) at `$PnR − 1`; `RemoveMargins` removes those and 17,701 more (31% in all), 17,700 of them at or below flowCore's −111 floor: the noise of negative populations in PE-Texas Red (13% of events), PerCP-Cy5.5 (10%) and Alexa Fluor 700 (9%), whose 1st percentiles lie at −367, −252 and −223 |
| FACSymphony A3/A5, 24 single-stain controls of a 24-colour OMIP (27,000–60,000 events each; Zenodo 22994607) | `cytiq autospill` vs R autospill | spillover max \|Δ\| 5.0e-13, ‖ΔC‖_F 1.7e-11; gates identical on all 24 controls (713,549 events); cytiq 31 s, R 34 min (25 min on 12 cores) |
| Cytek Aurora (5 lasers, 64 detectors), 9-colour human PBMC panel, AutoSpectral's own example: unstained and 9 single-stain cell controls (60,000–81,000 events), a 418,152-event sample (Mendeley xzt3h3gnx9) | `cytiq autospectral` spectra vs AutoSpectral (`define.flow.control`, `clean.controls`, `get.fluorophore.spectra`) | fluorophores: cosine 0.99983–1.0, max \|Δ\| ≤ 0.018 (AutoSpectral without `clean.controls`: 0.9993–0.999996); AF: 0.61 (peak detector V7-A against UV7-A) |
| | unmixing of the sample (every 10th event, 41,816) | solver vs `unmix.ols` on the same spectra: max relative Δ 4.1e-8; abundances vs AutoSpectral's: correlation 0.976–0.99998 (slope 0.92–1.007), with AutoSpectral's AF spectrum swapped in 0.9993–0.999999 (slope 0.993–1.004) |

Running FlowKit on cytiq's compensated output found the second Stage 3 defect:
the FCS writer left the HEADER DATA offsets at zero, which strict readers
reject; it now writes them whenever they fit. The NK T difference is
FlowJo's: its workspace counts 535 events, its exported Gating-ML gives 487 in
both cytiq and FlowKit, because FlowJo evaluates that gate at
`gateResolution="256"` on biex axes under a parent drawn on biex axes.

The FACSCelesta controls found the third defect. FACSDiva writes float data,
and 8 events of the PE control lie above `$PnR` (262,144) in PE, PE-Texas Red
and PerCP-Cy5.5. Upstream reads controls with `read.FCS(transformation =
NULL)`, whose default `truncate_max_range = TRUE` caps them at `$PnR`;
`cytiq autospill` read them as they are. The R trace already differed at
refinement iteration 0 (δ 0.00073397 vs 0.00073478), and the final PE →
PE-Texas Red coefficient by 1.3e-4; every other entry agreed to 1e-6.
`cytiq autospill` now caps control values at `$PnR`, and the synthetic
generator, which clips at `$PnR - 1`, could not have shown it.

The Aurora panel is the first real spectral data, and it found a fourth
defect. The fluorophore spectra were the mean of the positives minus the mean
of the whole unstained control, over a 1–99% FSC/SSC box. On real cell
controls both halves fail: the positives of a CD8 control are T cells while
the unstained control is all PBMC, so the difference in autofluorescence
enters the spectrum, and the box keeps debris and events saturated on many
detectors.
Against AutoSpectral the cosines were 0.50 (PerCP Fire 806), 0.60 (Spark UV
387), 0.66 (BV605) and 0.82–0.99 for the rest.

An ablation in R located the cause. AutoSpectral's spectra hardly depend on
its cleaning: the `rlm` slopes over the gated controls alone give a cosine of
0.9993 or more against the full workflow, so the estimator matters, not the
autofluorescence removal. cytiq now fits the same `rlm` slopes over the
positives and their scatter-matched negatives, and cleans controls (detector
margins, KDE gate) by default (§2.3). With the 1–99% box instead, the new
estimator still reaches only 0.42 on PerCP Fire 806, whose spectrum turns
flat at 0.71 across 20 detectors from saturated events: the gate matters
too.

The AF spectrum is the one difference left. AutoSpectral fits it against the
detector with the highest mean of its gated unstained events, over 1,000
random ones; here UV7-A and V7-A have means within 1% (3,695 and 3,660), and
cytiq's gate tips it to V7-A. Fitted against V7-A over all of AutoSpectral's
own gated events, AutoSpectral's estimator gives cytiq's AF spectrum (cosine
0.9995); against UV7-A, three random draws of 1,000 events give 0.91–0.94
against AutoSpectral's reported one. The abundances differ through that
spectrum alone: with AutoSpectral's AF spectrum, cytiq's fluorophore spectra
give AutoSpectral's abundances to a correlation of 0.9993 or better.

### 2.11 `spillqc`

`cytiq spillqc` ports `CompensAID::CompensAID()` (Olsman et al. 2026) and the
preprocessing of its vignette:

1. `PeacoQC::RemoveMargins`.
2. `flowCore::compensate` with `$SPILL`.
3. `estimateLogicle` + `transform`.
4. `PeacoQC`.
5. The marker channels.

Two new ports were needed:

- `flowDensity::deGate(all.cuts = TRUE, upper = TRUE)` in `cytiq-density`, on
  the `smooth.spline` port moved there from `cytiq-peacoqc`;
- flowCore's `estimateLogicle` width in `cytiq-transform`.

Three supporting changes:

- `cytiq-fcs` now reads flowCore's `flowCore_$PnRmin`/`Rmax` keywords for
  `minRange`/`maxRange`.
- `cytiq-peacoqc` exposes its mask computation for in-memory data. The
  `peacoqc` masks are unchanged (`parity_smoke`).
- The vignette runs PeacoQC over every column. Both sides here run it over the
  spillover channels.

| Dataset | Rows (pair × segment) | Cut-off max \|Δ\| | Segment values max \|Δ\| | SSI max \|Δ\| | Count / NA / message mismatches | Flag agreement | Flags |
|---|---|---|---|---|---|---|---|
| CompensAID `68983.fcs` (`--preprocessed`, algorithm only) | 360 | 4.4e-15 | 4.9e-15 | 0 | 0 / 0 / 0 | 100% | 2 = 2 |
| CompensAID `raw.68983.fcs` (full chain; 19,225 → 18,905 events, both) | 360 | 4.2e-15 | 6.9e-15 | 0 | 0 / 0 / 0 | 100% | 2 = 2 |
| synthetic `tube_1..3`, 30,000 events, 6 markers, two injected errors | 120 each | ≤ 5.3e-15 | ≤ 5.3e-15 | 0 | 0 / 0 / 0 | 100% | 9, 7, 9 |

Recovery of the injected errors:

- Both tools flag PE→PerCP-Cy5.5 (spillover halved) as `under` and
  APC→APC-Cy7 (×1.6) as `over` in every tube.
- With the true matrix (`--matrix truth/spillover.csv`) cytiq flags neither.
- The batch view calls both `recurrent_*` across the three tubes.

The other flags come from the same two errors, propagated through the
inverse: for example CD3→CD8 `over`, a consequence of the wrong PE row.
CompensAID's paper reports a precision of 0.16–0.27 for the same reason.

**What the synthetic data showed:**

1. **Empty density gaps.** The first synthetic tubes had perfectly separated
   populations. In 5 of 18 channel cut-offs (PE-Cy7 in every tube, APC in
   two), cytiq and R chose different points (0.008 to 0.40 apart) across a gap
   where the smoothed density is exactly zero; all flags still agreed. `.getIntersect` ranks the grid points by
   `splinefun(dens)`. Inside a run of zeros the spline values are round-off of
   about 1e-21, set by every earlier step and by how R's C code was compiled,
   so no port can reproduce R's pick. Reproducing R's FMM `spline_eval`
   exactly, including its interval reuse, still picked differently. cytiq
   takes the first zero. The generator now adds 6% intermediate events, as
   real samples have, after which the cut-offs agree to 5e-15.
2. **Strong under-compensation cannot be assessed.** Halving the large FITC→PE
   coefficient (0.22) lifts the CD3⁺CD4⁻ cells above PE's first valley. They
   leave the secondary-negative population, and both tools report the pair
   `No positive/negative population`. This is a property of the method,
   documented in the crate README.

### 2.12 `spillqc` on real data: what it detects

Parity says nothing about how useful the method is.
[`parity/benchmark/`](benchmark/README.md) (`just benchmark-spillqc`) measures
that on the FACSCelesta stained sample (11 colours, 56,458 events).

**Matrix accuracy.** The lab's FACSDiva matrix agrees with the AutoSpill matrix
from the matching controls: off-diagonal MAE 0.0021, max |Δ| 0.029. Yet spillqc
flags 47 of 79 assessable pairs with one and 45 with the other, 43 of them
shared. The flags reflect the sample, not the matrix.

The FACSymphony controls carry an identity `$SPILL` and come without a stained
sample, so they cannot be scored.

**Detection.** One coefficient of the FACSDiva matrix is changed at a time
(772 runs), and runs are scored against the unchanged-matrix flags:

- Recall is 0–11% for relative errors of ±5–20%, 22–28% at ±50%, and 35% for
  +0.05 absolute.
- Precision is 0.07–0.29; CompensAID reports 0.16–0.27.
- About a third of the changed pairs are not assessable.

Under-compensation is the harder direction: 13 of 36 pairs are already
flagged `under`, and the SSI moves less (median 0.09 at −0.05 against 1.2 at
+0.05).

## 3. Known remaining differences

| Tool | Difference | Impact | Recommendation |
|---|---|---|---|
| FlowSOM | RNG: no bit-exact match with R (MT19937 + `R_unif_index`) | statistical agreement (ARI ≈ seed-to-seed) | port MT19937 if bit-exact reproducibility is needed |
| FlowSOM | ConsensusClusterPlus → average-linkage hc | not visible on the tests | port if needed |
| FlowSOM | `scale` and `nClus` defaults differ from R | product decision | settable with `--no-scale`/`--metaclusters` |
| AutoSpectral | the AutoSpill KDE gate instead of AutoSpectral's density gates (`do.gate`, large gates); no autofluorescence removal from cell controls | fluorophore spectra still match to cosine ≥ 0.9998 on real Aurora controls (§2.10) | port `do.gate` and `remove.af` if a panel needs them |
| AutoSpectral | AF spectrum against the detector with the highest mean of a different gate, over every gated event instead of 1,000 random ones | on the Aurora panel cytiq's peak is V7-A, AutoSpectral's UV7-A (means within 1%); AF spectrum cosine 0.61, fluorophore abundances correlate 0.976–0.99998 (≥ 0.999 with AutoSpectral's AF spectrum); AutoSpectral's own AF spectrum moves to 0.91–0.94 between random draws; the gated unstained events hold two AF spectral types (V7-A and UV7-A peaks, 67/33) and the argmax switches with probability 0.31 under resampling (§7) | keep: no alternative met the predeclared rule (§7); exact ties now resolve to the first detector, as `which.max` |
| AutoSpectral | negatives capped at 1,000 evenly spaced instead of a random `sample` | reproducible runs | — |
| AutoSpectral | no WLS/Poisson/per-cell AF (R's default `AutoSpectral` method) | comparable only to `method = "OLS"` | separate task |
| AutoSpread | no upstream; units are σ/√F while AutoSpectral expects σ²/F | format incompatibility | document the units |
| transform | biexponential parameters were fixed to the defaults until 0.5 | functionality | resolved: `--biexponential-*` flags, checked against `flowjo_biexp` at non-default values (`data/fixtures/transform/flowjo_biexp_params.rs`) |
| cytoscan | subsamples come from `rand`, not R's MT19937, so features on large files agree only statistically | Rust–R Δ equals R's seed-to-seed Δ | port R's RNG if bit-identical features are needed |
| cytoscan | isotree uses its own C++ RNG | scores agree within isotree's Monte Carlo noise (≈0.014 at 1000 trees) | — |
| cytoscan | the mixture search has mclust's spherical and diagonal structures, `EEE` and `VVV`, not `VEE`, `EVE`, `VVE`, `EEV`, `VEV`, `EVV`; with more reference files than features EM starts from `hcEII` where mclust uses `hcVVV` | on the parity cohorts mclust picks none of the missing structures; on the same features the model, G and flags equal mclust's in 12 of 15 cases | port the rest if a cohort needs them |
| cytoscan | a one-component full covariance singular up to rounding is rejected; mclust keeps it | binning and fingerprint features (they sum to 1): mclust flags every test file, cytiq picks a diagonal model (flag agreement 30–100%) | — |
| cytoscan | upstream errors without `SPILL`; Rust warns and keeps raw values | only files without spillover metadata | `--strict` errors like upstream |
| autospill | on a perfectly regular lattice the KDE produces exact ties, broken in a different order than R | degenerate data only | — |
| gate | compensation multiplies by an explicit inverse (Gauss-Jordan); FlowKit uses `np.linalg.solve` | round-off of about 1e-16 relative, so only events lying exactly on a boundary in compensated space could flip; none did at 1M events | — |
| gate | quadrant gates, `fratio` and `new-dimension` are not supported | rejected with an explicit error | add when a real strategy needs them |
| gate | Gating-ML without the required `compensation-ref` is read as `uncompensated`; FlowKit rejects it | FlowJo exports omit it | — |
| margins | `compensate`/`autospectral` masks flag the upper detector limit `$PnR − 1` only; `peacoqc --remove-margins` keeps `PeacoQC::RemoveMargins`, which also removes each channel's observed extremes and values at or below flowCore's −111 floor | RemoveMargins removes a superset: noise below −111 on float data (7.4% of the golden chain, §2.9; 31% of a FACSCelesta sample, §2.10), far more on discrete data | use the `compensate` mask, not `--remove-margins`, on float data |
| PeacoQC | on small files (15,000 events) the bins are coarse, so ~10% of good events around an artefact are removed (5.6% at 200,000) | same as R | prefer whole acquisitions |
| real data | FlowRepository (AutoSpill datasets) serves an expired certificate (March 2023) | not fetched automatically | download by hand into `CYTIQ_REAL_DATA` |
| spillqc | across an empty density gap `deGate`'s valley is the first zero-density grid point; R's pick follows ~1e-21 round-off of `splinefun` | different cut-off in the gap (events inside it move between populations); flags agreed even so on the synthetic data | none: R's choice is not reproducible |
| spillqc | inputs on which CompensAID errors (`if (NA)` in `.WithinLimit`, `numeric(0)` in `.GetClosestLimit`) or counts `NA` rows as events | cytiq marks the pair `not_assessed` or keeps the old cut-off (crate CONTRIBUTING) | — |

## 4. Changed files

- `crates/cytiq-transform/src/scales.rs` (new), `lib.rs`, `error.rs`
- `crates/cytiq-autospill/src/{gating,regression,runner,transform}.rs`,
  `Cargo.toml` (dependency on `cytiq-transform`)
- `crates/cytiq-autospectral/src/{runner,lib}.rs`
- `crates/cytiq-peacoqc/src/{density,smooth_spline}.rs` (new), `runner.rs`,
  `lib.rs`
- `crates/cytiq-flowsom/src/{runner,lib}.rs`
- `crates/cytiq-cytoscan/src/{runner,novelty,lib,error}.rs`, `Cargo.toml`
  (`linfa`, `linfa-clustering`, `ndarray`, `rand`, `rand_distr`,
  `rand_xoshiro`); `crates/cytiq-engine/tests/cytoscan_cli.rs`;
  `data/fixtures/cytoscan/reference.rs`
- `crates/cytiq-fcs/src/writer.rs` (buffered output), `data.rs`, `text.rs`
  (Rust 1.99 clippy lints)
- Tests and fixtures:
  - `data/fixtures/peacoqc/{density,smooth_spline}_reference.rs`;
  - `data/fixtures/transform/flowcore_reference.rs`;
  - regenerated `data/fixtures/autospread/expected_spread.csv`;
  - `crates/cytiq-engine/tests/peacoqc_cli.rs`.
- `README.md`: transform semantics, AutoSpectral, Algorithm Notes.
- 2026-10-25: `crates/cytiq-cytoscan/src/mixture.rs` (new; `linfa`,
  `linfa-clustering`, `ndarray`, `rand_xoshiro` dropped),
  `crates/cytiq-autospectral/src/spectrum.rs` (new), `runner.rs`;
  `data/fixtures/cytoscan/reference.rs`,
  `data/fixtures/autospectral/rlm_reference.rs` (new),
  `data/fixtures/autospectral/unstained.fcs`; `parity/real/` (`autospectral`
  chain).

- 2026-10-27: `crates/cytiq-spillqc/` (new); `crates/cytiq-density/src/degate.rs`
  (new), `smooth_spline.rs` (moved from `cytiq-peacoqc`, plus an `x` variant);
  `crates/cytiq-peacoqc/src/runner.rs` (`compute_mask`, `peacoqc_mask`);
  `crates/cytiq-transform/src/scales.rs` (`estimate_logicle_width`);
  `crates/cytiq-fcs/src/margins.rs` (`from_flowcore_parameter`);
  `parity/gen/spillqc.py`, `parity/compensaid.R` (new); `parity/real/`
  (`compensaid` chain); `data/fixtures/density/degate_reference.rs`,
  `data/fixtures/transform/estimate_logicle_reference.rs`,
  `data/fixtures/parity/spillqc/` (new).

Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace` — all green.

## 5. Reproducing

```bash
parity/setup_env.sh        # once (also `just parity-setup`): R library and uv venv in parity/.env (git-ignored)
parity/run_all.sh main     # synthetic data → Rust (branch + main) → R → parity/results/*.json; fails on a tolerance violation
parity/real/run_real.sh    # real acquisitions → parity/results/metrics_real.json (also `just parity-real`)
just parity-ci-fixture     # regenerate data/fixtures/pipeline (byte-identical)
just parity-smoke-fixture  # regenerate data/fixtures/parity: small datasets and the R outputs on them
R_LIBS_USER=parity/.env/Rlib Rscript parity/make_fixtures.R   # regenerate R reference values for unit tests
```

A full run takes about 3 minutes. Raw outputs are in `parity/.out/`, data in
`parity/.data/`.

The suites run locally, not in CI: the R environment takes too long to
install on hosted runners. CI runs the smoke test on the committed R outputs
instead. A violated tolerance exits non-zero, and the
timing wrappers read both BSD (macOS) and GNU `time`. `setup_env.sh`
(`just parity-setup`) pins the Bioconductor release, the git commits of
autospill, AutoSpectral, CytoScan and CompensAID, and the Python packages
(`parity/requirements.txt`); CRAN packages come from `PARITY_CRAN`, and the
installed versions are recorded in `versions.txt`.

## 6. CytoNorm integration (2026-10-28)

`cytiq-cytonorm` independently implements population-specific quantile mappings
and monotone Hermite interpolation, with persisted FlowSOM and consensus
metaclustering. The separate GPL R reference is CytoNorm 2.0.12 at
`278f352d8edffd7a5d3e6b0b76c402d991a1e7a9`, with FlowSOM 2.18.0 and
ConsensusClusterPlus 1.74.0. No upstream implementation source is copied into
the Rust crate. The existing FlowSOM command retains its default average linkage.

`just parity-cytonorm` generates two training and two independent validation
FCS files with three populations, two backbone markers, a batch shift and a
biological shift on a marker excluded from clustering. It forces upstream
recomputation instead of reusing an old RDS model. Fixed-assignment tests isolate
the normalization from RNG differences; full-model comparison checks population
labels with ARI rather than equating stochastic node IDs.

| Check | Result | Acceptance |
| --- | --- | --- |
| Quantiles/splines, same assignments, all train/validation events | max absolute difference `3.55e-15` | `<= 1e-9` |
| Independent Rust/R SOM and consensus, all four files | minimum ARI `1.0` | `>= 0.95` on this synthetic cohort |
| Held-out biological shift retained | `3.001` transformed units (injected 3 plus 0.001 validation offset) | `>= 2.7` |
| Rust workflow tests | technical quantile W1 reduced by more than 80%; 0.2% population retained with 120 reference events per batch | explicit synthetic assertions |

CI uses committed R numerical outputs from `parity/cytonorm_numeric.R` for
type-7 quantiles, ties, flat/negative knots, extrapolation and consensus with
identical codebook/resampling schedules. Regeneration is explicit via
`just parity-cytonorm-fixture`, never a way to make Rust changes pass.

The real-control check, `just parity-cytonorm-real`, also compares intermediate
derivatives with `stats::splinefun(method="monoH.FC")`. The radius-three condition
is sufficient but does not cover the full monotone Hermite region. Applying it
unconditionally caused reproduced errors of `0.0570019504` within knots and
`0.3544738411` in linear tails. Testing the derivative minimum before radial
correction restores the R coefficients on admissible curves. The six real
1,000-cell excerpts now give coefficient error `3.55e-15`, interior error
`1.38e-14`, and tail error `2.63e-12`, below the unchanged `1e-9` value tolerance.

R's sequential adjacent corrections can themselves produce a non-monotone
curve: the CD19 mapping for PTLG021/metacluster 1 has a negative derivative.
The engine rejects such a fit by default. With the existing explicit
`sparse_policy: identity`, this pilot retains 288 original values for that
mapping and records the unsafe-fit reason. The harness independently certifies
the R derivatives and checks exact identity for this exception; it does not
count the exception as numerical equivalence. There are 223 identity mappings
out of 555 (222 sparse, one unsafe). Quantile-distance reduction is 37.75% for
training and 7.63% for the held-out donor. These are descriptive cohort results,
not evidence that normalization improves every population. Saved older spline
coefficients are validated without refitting and keep their numerical results.

These checks do not establish clinical transferability between arbitrary panels
or instruments. Aggregate mode assumes comparable biology. QC reports occupancy,
CV, identity fallback, extrapolation and a quantile approximation to 1-D EMD;
that reported approximation is not upstream's histogram `emdEvaluation` score.
Classifier v3 pins the exact normalizer; v1/v2 reject normalized inputs.

## 7. AF reference and compensation uncertainty (2026-11-18)

No production value changes in this section. The AF peak and runner-up now
resolve exact ties to the first detector, as R's `which.max` does (Rust's
`max_by` took the last); every pinned dataset has distinct means, so no output
moved. `--debug-dir` additionally writes `reference_events.json`, the rows
behind every spectrum. With and without it, references, unmixed FCS, masks and
`run.json` (except `run_id`) are byte-identical to `main` on the parity fixture
and on the Aurora panel. Methods and commands are in the
[scientific guide](scientific/README.md#af-reference-benchmark).

### AF reference benchmark

**Oracle.** A NumPy port of the estimator and the event selection
(`af_estimators.py`) refits production from the exported rows. AF means, peak,
runner-up, margin, runner-up cosine, every positive and matched negative row,
and every spectrum are bitwise equal on `data/fixtures/autospectral`
(`--quantile-gate`), `data/fixtures/parity/autospectral` and the real Aurora
controls (KDE gate, 64 detectors). The tolerance is 1e-9 and never widened.
The baseline arm reproduces the Rust unmixed FCS within f32 rounding (relative
5.5e-8).

**Aurora PBMC** (seeds 1–20 for stability, 2 folds × 5 seeds held out, every
10th sample event):

| Arm | Cosine to production | Split-half min cosine | Peak switch, 1k / 10k events | Held-out SD ratio (median; BV510) | Held-out residual | κ | Max AF↔fluorophore cosine | Sample Δ max (robust SD) | Residual outside AF gate | r to R (min) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `peak_argmax` (production) | 1 | 0.800 | 0.35 / 0.20 | 1 (1) | 0.318 | 6.36 | 0.912 | 0 | 0.043 | 0.976 |
| `runner_up` | 0.812 | 0.804 | 0.35 / 0.20 | 0.999 (0.998) | 0.315 | 4.33 | 0.745 | 0.95 | 0.040 | 0.998 |
| `total_signal` | 0.976 | 0.9994 | – | 1.007 (0.949) | 0.266 | 5.78 | 0.873 | 0.39 | 0.042 | 0.962 |
| `mean_ratio` | 0.985 | 1.0000 | – | 0.995 (0.943) | 0.261 | 5.87 | 0.888 | 0.43 | 0.042 | 0.985 |
| `principal_axis` | 0.941 | 0.998 | – | 0.998 (0.976) | 0.283 | 5.05 | 0.850 | 0.69 | 0.042 | 0.987 |
| `spectral_clusters_k2` joint | 0.9996 | 0.9975 | – | 0.998 (1.015) | 0.233 | 8.31 | 0.913 | 0.23 | 0.026 | 0.983 |
| `spectral_clusters_k2` min-residual | 0.9996 | 0.9975 | – | 1.050 (1.816) | 0.255 | 8.31 | 0.913 | 0.42 | 0.035 | 0.984 |
| `scatter_strata_k2` joint | 0.9989 | 0.775 | – | 0.995 (0.944) | 0.270 | 7.20 | 0.908 | 0.23 | 0.026 | 0.977 |

- **The peak is a coin flip.** The event bootstrap of the gated means gives
  V7-A a margin of −0.49% to +0.73% (95%) and a 31% probability that another
  detector takes the argmax. Production's own AF spectrum has split-half cosine
  0.80, and 1,000-event subsamples go down to 0.78. The end-to-end
  `af_sensitivity.py` run (gate recomputed) reproduces #205's table exactly.
- **Two AF types, not one.** Seeded spherical k-means (k = 2, identical labels
  for init seeds 1–5, ARI 1.0) splits the gated unstained events 67/33. One
  component peaks on V7-A and matches production (cosine 0.9996; 0.616 to R).
  The other peaks on UV7-A and matches R's AF (0.951). FSC and SSC medians of
  the two differ by under 3%, so scatter cannot identify them (both
  `scatter_strata` arms are unstable), and no cell-type label is claimed. cytiq
  and AutoSpectral each fit one of the two; `runner_up` reproduces R's
  abundances to r ≥ 0.998.
- **Peak-free spectra are reproducible but not better downstream.**
  `mean_ratio` reaches split-half cosine 1.0000 and `total_signal` 0.9994,
  against 0.80, and both lower the held-out relative residual by about 17%.
  They do not lower the held-out fluorophore spread: median ratio 0.995–1.007,
  and only BV510, whose spectrum is closest to AF (cosine 0.912) and which has
  the largest unit-noise variance, improves 5–6%. They move stained-sample
  abundances by up to 0.4 robust SD.
- **Two components fit better and stay identifiable, but fail the rule.**
  `spectral_clusters_k2` passes every identifiability gate on Aurora. Joint
  unmixing lowers the held-out residual 27% and the residual of sample events
  outside the AF gate's scatter hull 40%; 68% of sample events lie outside that
  hull. Its fluorophore spread is unchanged (0.998), its condition number is
  1.31× production (rule ≤ 1.25) and its split-half cosine 0.9975 (rule ≥
  0.999).

**Synthetic truth** (`af_scenarios.py`, 3 seeds each; ranges over seeds):

| Scenario | Production split-half / switch probability | Best single-AF arm: held-out SD ratio, abundance RMSE / production | `spectral_clusters_k2` min-residual: SD ratio, RMSE / production, κ ratio, identifiable |
|---|---|---|---|
| single AF | 1.000 / 0 | all 1.00, 1.00 | 1.00, 1.00, 19–25, no (κ) |
| two near-tied sources (D03/D07) | −0.97 / 0.05–0.46 | `principal_axis` 0.95–0.98, 1.04–1.07 | 0.65, 0.54–0.59, 1.59, no (κ) |
| 5% source, scatter-linked | 1.000 / 0 | `runner_up` 1.00, 0.99 | 0.99, 0.54–0.70, 1.11, no: only 86–102 of its events pass the AF gate |
| 5% source, scatter-independent | 1.000 / 0 | `runner_up` 1.00, 0.99 | 0.95, 0.54–0.70, 1.13, 1 of 3 seeds (607–687 events against a 629–649 minimum) |
| dim positives (single AF) | 1.000 / 0 | all 1.00, 1.00 | 1.00, 1.00, 19–25, no (κ) |

- With one AF source every arm is equivalent downstream. The production
  `rlm`-on-peak spectrum sits 0.27–0.36° from the true AF (regression
  attenuation from noise on the peak detector) against 0.03° for
  `mean_ratio`, which changes no abundance (RMSE ratio 1.00). Multi-AF arms are
  correctly rejected (κ 19–25× production).
- With two nearly orthogonal sources of similar brightness the production
  estimator is not merely unstable but wrong in kind. Regressing every detector
  on one source's peak gives a negative slope for the other source, so the
  "AF" is roughly afA − 1.1·afB, with a −1.10 lobe. The runner-up spectrum is
  its mirror (cosine −0.968), and `af_peak_sensitivity` flags it as
  `stable: false`. No single spectrum repairs it (`mean_ratio` is 2.4× worse).
  Two components recover both sources (cosine ≥ 0.999 each), with 35% less
  spread and 41–46% less abundance error, but κ is 1.59× and fails the
  predeclared identifiability gate (≤ 1.5). The rule was not changed after the
  results. Aurora's sources overlap (runner-up cosine +0.81), so its production
  AF has no negative lobe.
- A 5% minority source leaves the production spectrum stable. Its error lives
  in the minority cells: two components cut abundance RMSE 30–46% while the
  held-out robust SD (p16–p84) barely moves. **The held-out spread criterion is
  blind to minority AF populations**, which the RMSE against truth shows.
  When the minority source also has higher SSC, the AF control's KDE gate keeps
  only 86–102 of its roughly 1,000 events: an AF population with distinct
  scatter barely reaches the AF reference at all.
- `weak_positive_spike_in` is uninformative by construction: a noise-free
  fluorophore vector lies in the fluorophore span, so no residual and no AF
  assignment changes, every solver recovers it without bias, and its AUC
  restates the unstained spread. Synthetic dim populations (AUC 0.79–0.93) are
  unchanged by single-AF arms and lowered by joint multi-AF (FITC 0.92 → 0.78 in
  one seed, k = 3): extra AF columns absorb dim signal.

**Decision.** `DECISION_RULE` (sha256
`aa5533e0114922bec592572f7cd80992b9f3454a313c5ba96ce297fd497e6eba`, recorded
before any arm ran) leaves 0 of 16 arm/solver variants eligible. Every variant
fails for having 1 independent real dataset (< 2). Each also fails at least
one Aurora criterion: none lowers the held-out spread by 5%, and most fail
stability, conditioning or identifiability. The production estimator stays,
and its parity with AutoSpectral's estimator (#202) is unchanged.

Measured, not recommended: the AF instability on PBMC is two AF spectral types
of near-equal brightness, not noise. A future candidate is a two-component AF
with per-event assignment. It would need identification by an independent
variable (sorted or lineage-gated unstained subsets), a second real spectral
dataset, a decision on the conditioning bound, and a downstream criterion that
sees minority populations. None of these is available (required data:
[scientific guide](scientific/README.md#required-real-data)).

### Compensation uncertainty on synthetic repeat controls

No pinned dataset has repeated single-stain controls, so `repeat_controls.py`
generates them. They are labelled "synthetic; not biological evidence", with
`real_repeat_controls: not_assessed`. The study ran 6 predeclared scenarios ×
20 sessions × 8 independently stained tube sets (5,000 events per tube). Every
matrix came from production `cytiq autospill` and every compensated sample from
`cytiq compensate`, which reproduced raw·S⁻¹ within f32 rounding throughout. The
event bootstrap used 100 resamples of 3 sessions (exploratory). Coverage is
over 600 session intervals or 90 bootstrap intervals, with Wilson 95% bounds in
the report.

| Scenario | Between-tube / within-tube SD (max cell) | Between-session SD (max) | t-interval → session truth | t-interval → sample truth | Prediction interval → sample truth | Bootstrap → tube truth | Bootstrap → sample truth | FITC single → PE false positives: truth; worst Δ | Co-expressed PE+ Δ (range) |
|---|---|---:|---:|---:|---:|---:|---:|---|---|
| null | 2e-4 / 2e-4 | 1e-4 | 0.918 | 0.918 | 1.000 | 0.944 | 0.944 | 9.8%; ±1.0 pt | −2.2 to +2.8 pt |
| brightness (σ 0.3) | 2e-4 / 2e-4 | 1e-4 | 0.937 | 0.937 | 1.000 | 0.900 | 0.900 | 9.9%; +2.0 pt | −2.5 to +3.6 pt |
| tandem (σ 0.01) | 0.0108 / 1.1e-4 (×96) | 0.0032 | 0.948 | 0.943 | 1.000 | 0.933 | 0.878 | 10.2%; −1.6 pt | −2.6 to +2.5 pt |
| AF level/shape | 2e-4 / 2e-4 | 1e-4 | 0.938 | 0.938 | 1.000 | 0.922 | 0.922 | 10.5%; +1.4 pt | −1.9 to +2.5 pt |
| detector gain (σ 0.05) | 2e-4 / 2e-4 | 0.0225 | 0.913 | 0.352 | 0.478 | 0.933 | 0.500 | 10.4%; +88 pt | −57 to +43 pt |
| combined | 0.0104 / 1.1e-4 (×101) | 0.0179 | 0.930 | 0.398 | 0.550 | 0.944 | 0.456 | 9.6%; +90 pt | −56 to +44 pt |

- **One tube's bootstrap measures one tube.** When only brightness or AF
  varies between tubes, a single tube's event bootstrap matches the
  between-tube spread (ratio 0.94–0.98) and covers the truth 0.90–0.94. A
  degrading tandem breaks this. Independently stained PE-Cy7 and APC-Cy7 tubes
  differ in their donor coefficient with SD 0.009–0.011, about 90–100× what
  any tube's bootstrap reports (1.1e-4). The bootstrap interval of a single
  control is then far too narrow, and only replicate tubes reveal it.
- **Session truth is not sample truth.** With per-session detector gain drift
  (5%), intervals still cover their own session's matrix (0.91–0.93). They
  cover the matrix the sample was acquired with only 35–50% of the time, and
  a prediction interval over the session's tubes only 48%. Controls and
  samples acquired under different gains are a different estimand, which no
  amount of within-session replication can fix.
- **Downstream.** Brightness, AF and tandem drift at these magnitudes move no
  population by more than 3.6 points. The double-negative false-positive rate
  stays at 0.50–0.53% against the gate's 0.5%. A 5% gain mismatch turns
  FITC single positives into PE false positives in up to 99% of events (truth:
  10%, mostly spreading), and moves the dim co-expressed PE+ fraction by up to
  ±57 points. That dim population sits at the gate, so even the null
  scenario's coefficient noise (2e-4) moves it by ±2–3 points. The FITC→PE coefficient error orders the false-positive change:
  Spearman ρ 0.96 (null), 0.69 (gain), 0.45 (combined, where several
  coefficients err at once).
- **Estimator bias.** AutoSpill's 160-tube mean differs from the truth by at
  most 9.7e-5 (BV421→FITC, truth 0.045; up to 9 standard errors in 5 of 30
  cells). That is why 95% t-intervals cover the truth only 0.92–0.94 even in
  the null scenario. It is two orders of magnitude below the tandem and gain
  effects. Leave-one-out prediction intervals, which compare estimates with
  estimates, reach a nominal 0.94–0.95.
- **First-order propagation.** For every session matrix the first-order error
  `−latent·E·S⁻¹` stayed within its exact bound ‖E·S⁻¹‖₂ (largest relative
  error 0.029 against a bound of 0.050; no violation). The Rust tests now pin
  the exact identity `x̂ − x = −x·E·(S+E)⁻¹`, the row-norm bounds, and that
  events grow by up to κ₁(Sᵀ), not κ₁(S). `condition_number_1`'s doc comment
  said the latter. The preflight warning (`cytiq-qc`, κ₁ of the matrix as
  given, threshold 100) is unchanged and recorded as a follow-up.

These are properties of the estimators under declared synthetic variability,
not of any instrument. The real repeat controls that would turn them into
evidence are specified in the
[scientific guide](scientific/README.md#required-real-data).

## 8. QC artifact truth and population retention (2026-11-19)

No filter, mask, policy or release decision changes in this section. Every
existing tolerance, golden and frozen reference is unchanged. Clean main
`5f426a3` and this branch produce identical filtering counts on all 42
benchmark cases.

**New:**
- **Contract.** `cytiq.qc.truth.v1` ([contract](../docs/qc-truth.md)) holds
  independent per-event artifact truth with its provenance.
- **`qc.json` `artifact_removal`.** `cytiq qc --artifact-truth` adds this
  section: every mask column and the final keep against that truth. It is
  `not_assessed` without truth.
- **Clean-event population retention.** `--populations` gains clean-event
  retention and the frequency error against clean events.
- **Benchmark.** `parity/scientific/qc_truth.py` runs 14 predeclared scenarios
  with an exact NumPy oracle and a frozen clean-main reference. Methods are
  in the [scientific guide](scientific/README.md#qc-artifact-truth-benchmark).

### What has independent ground truth

| Result | Truth | Truth class | Supports |
| --- | --- | --- | --- |
| Debris, doublet, dead-cell and time-anomaly removal (14 scenarios × 10 seeds) | the generator's per-event flags | `synthetic` | algorithm behaviour on simulated data; not biology |
| Saturation (`margin_pass`) | value ≥ `$PnR − 1` recomputed from the raw file | `definitional` | that the filter implements the definition |
| Golden chain (`pipeline_cli.rs`) | `data/fixtures/pipeline/truth` | `synthetic` | the CLI's metrics equal the test's own per-event arithmetic |
| Weak CD19 (`weak_cd19`, `weak_population.py`) | in-silico attenuation of generated cells | `synthetic`, counterfactual | filtering sensitivity to a dimmer marker; not weak-positive biology |
| CyMAE population retention | published manual gating labels | population labels only | descriptive retention; removal accuracy `not_assessed` |
| Real-data artifact removal, every domain | none pinned | `not_assessed` | nothing |
| Population acceptance bounds | none eligible | `not_assessed` | nothing: no bound is proposed |

The CyMAE vocabularies include `Beads` and `Cleanup1` to `Cleanup6`. These are
likely manual technical exclusions, covering up to a third of an excerpt's
events. They are not mapped, because their gating parameters are unpublished
([provenance](../data/fixtures/scientific/README.md)).

### Results (synthetic; 10 seeds, median [min, max])

| Domain, column | Scenario | Sensitivity | Specificity | Precision | Clean loss |
| --- | --- | ---: | ---: | ---: | ---: |
| debris, `non_debris` | baseline | 1.000 [0.997, 1.000] | 1.000 | 1.000 [0.999, 1.000] | 0.000 |
| | `rare_small` | 0.993 [0.985, 1.000] | 0.999 [0.991, 1.000] | 0.992 [0.901, 0.996] | 0.0005 [0.0003, 0.0101] |
| doublets, `singlets` | baseline | 1.000 | 0.956 [0.952, 0.959] | 0.572 [0.547, 0.592] | 0.0013 [0.0006, 0.0019] |
| time, `peacoqc_pass` | baseline (6%) | 1.000 | 0.878 [0.816, 0.904] | 0.344 [0.257, 0.400] | 0.122 [0.095, 0.182] |
| | `time_long` (20%) | 1.000 | 0.896 [0.896, 0.917] | 0.706 [0.706, 0.750] | 0.104 [0.083, 0.107] |
| | `time_short` (1%) | **0.000** | 1.000 [0.966, 1.000] | 0.000 | 0.000 [0.000, 0.033] |
| saturation, `margin_pass` | baseline, `saturation_high` | 1.000 | 1.000 | 1.000 | 0.000 |
| viability, `live` | baseline | 0.995 [0.994, 0.999] | 0.988 [0.984, 0.991] | 0.884 [0.861, 0.908] | 0.000 |
| | `dead_dim` (7 assessed seeds) | 0.855 [0.827, 0.879] | 0.962 [0.958, 0.966] | 0.677 [0.659, 0.699] | 0.000 [0.000, 0.0003] |

| Scenario | Combined artifact recall | Combined clean retention | `rare` clean retention |
| --- | ---: | ---: | ---: |
| baseline | 0.9996 [0.9990, 1.0000] | 0.877 [0.818, 0.904] | 0.855 [0.815, 0.958] |
| `events_50k` | 0.9997 [0.9996, 0.9999] | 0.956 [0.930, 0.961] | 0.959 [0.920, 0.989] |
| `rare_small` | 0.9990 [0.9983, 0.9998] | 0.869 [0.866, 0.893] | **0.810 [0.089, 0.871]** |
| `rare_0.1pct` (8–13 rare cells) | 0.9996 [0.9990, 0.9998] | 0.877 [0.866, 0.887] | 0.894 [0.750, 1.000] |
| `dead_dim` | **0.966 [0.747, 0.976]** | 0.877 [0.835, 0.904] | — |
| `weak_cd19` | 0.9996 [0.9990, 0.9998] | 0.885 [0.867, 0.903] | 0.858 [0.815, 0.958] |

Wilson intervals for `rare` (conditional on each acquisition's fitted gates):
- In the baseline, 27 of 30 rare cells are retained, Wilson [0.744, 0.965].
- With 8 to 13 rare cells, `rare_0.1pct` intervals are about 0.4 wide. That
  is too few cells to bound any rare-cell loss usefully.

### Findings

- **Small rare cells and the adaptive debris valley.**
  - In 2 of 10 `rare_small` seeds the FSC valley lands at 41.3k–41.7k: above
    the small cells (FSC 35k ± 6k), between them and the lymphocytes. In the
    other eight it lands at 24.8k–27.1k, between debris and the small cells.
  - In those two seeds `non_debris` alone keeps 14 of 112 and 15 of 124 clean
    rare cells. Debris sensitivity stays 1.0, and the filter's clean loss is
    only 0.91% and 1.01%.
  - Overall removal therefore cannot show this loss. Population retention
    shows it, and artifact truth proves the removed events were clean cells.
- **PeacoQC resolution.**
  - At 15,000 events PeacoQC uses 60 bins of 500 events. A 150-event anomaly
    (`time_short`) is missed in every seed.
  - The 900- and 3,000-event anomalies are always found, at a clean loss of
    10–12% around them. At 50,000 events that clean loss falls to 4.3%
    [3.8%, 6.9%].
  - This is the upstream algorithm, which cytiq matches exactly (§2.4).
- **Dim viability dye.**
  - With the dead dye at 2,000 ± 800 against 300 ± 200, the density valley
    yields sensitivity 0.85 and precision 0.68.
  - In 3 of 10 seeds no valley is found. `live` is then `not_assessed`, its
    target is `not_assessed`, and combined artifact recall drops to 0.75.
  - Because the panel policy marks `live` as required, `qc` fails (exit 3)
    instead of passing. The nullable assessment does what it was designed to
    do.
- **Cross-class removal.**
  - `singlets` precision for doublets is 0.57 because the same gate also
    removes most debris (`removed_by_class`). Its clean loss is 0.13%.
  - Specificity and precision against one class do not measure harm. Clean
    loss does.
- **Weak CD19.** A 20-fold dimmer CD19 does not change the clean retention of
  B or rare cells beyond seed variation (B: 0.876 against 0.873 at baseline).

### Decisions

- **Confirmed improvements (software).**
  - Removal accuracy is now measured against independent truth, separately
    from retention, and is `not_assessed`, never passed, without truth.
  - Clean-event retention distinguishes removed artifacts from lost cells.
  - The truth reader is fuzzed, and its metrics and retention core are under
    mutation testing.
- **No filter algorithm change.**
  - The three weaknesses above reproduce across seeds, but only on generated
    data whose scatter and dye models this benchmark chose.
  - Moving the debris valley, shrinking PeacoQC bins (which would break exact
    R parity) or changing the viability threshold each trades against cases
    real data must decide.
  - Under the predeclared rule, none is adopted without independent real
    truth.
- **No acceptance bound.**
  - `POLICY_PROTOCOL` requires independent or expert-reference truth from at
    least two real derivation datasets and a disjoint held-out one. None
    exists, so `policy_derivation` is `not_assessed`.
  - Descriptive retention changes no status, exit code or release decision.
  - The data that would be needed is listed in the
    [scientific guide](scientific/README.md#required-real-data-for-artifact-truth).

## 9. CytoScan novelty stability and calibration (2026-11-19)

Without the new options, `cytoscan_summary.csv` and `cytoscan_features.csv` are
byte-identical to clean main (`be8a831`) on all 15 parity cohort/method
combinations at seeds 42 and 7 and on the benchmark's first replicate; the
existing parameters and diagnostics are equal and the run time is unchanged
(median ratio to main 0.91–1.01). §2.7 does not move. Methods are in the
[scientific guide](scientific/README.md#cytoscan-novelty-benchmark).

**New in production:**
- **Flag assessments.** `diagnostics.outlier_flag` and `novelty_flag` state each
  flag's scope (within the test cohort / against the reference), threshold
  rule, calibration status, exact seed dependence and stability status;
  `population_false_positive_rate` is `not_assessed`.
- **`--stability-seeds N`.** Repeats the analysis with `seed + j·2^32`
  (isolation-tree seeds provably disjoint) and writes per-file flag agreement
  and every seed's values; seed `j` equals a separate `--seed` run.
- **`--novelty-mode conformal-ledoit-wolf-v1`.** Opt-in and versioned: the arm
  the frozen rule made eligible (below). The default `upstream` mode is
  unchanged.

**Benchmark.** `parity/scientific/novelty_benchmark.py` draws, per replicate, a
64-file reference pool, 40 held-out normal files and 10 anomalous files
(`gain_shift`, `wrong_spill`, `population_loss`, `compression`, `debris`, two
each; 5,000 events) from predeclared generator seeds, and scores production
runs. Development used seeds 20261201–20261220; confirmation 20261301–20261350
(50 replicates, 8,340 production invocations, 13 min). Intervals are
percentile bootstraps over replicates (the independent units).

### Confirmation results (synthetic; 50 replicates, 6 channels)

FPR is the mean share of the 40 held-out normal files flagged, with its 95%
interval; `P(>0.1)` is the share of replicates whose FPR exceeds 0.10 (a
perfect split-conformal rule gives 0.12 / 0.05 / 0.07 at 20 / 32 / 64
references); flips are the probability that two algorithm seeds disagree on a
normal file; detection is the lowest per-class rate.

| Features, references | Upstream (production) FPR | `P(>0.1)` | flips | mclust + cross-conformal FPR | min detection | **Ledoit–Wolf + cross-conformal FPR** | `P(>0.1)` | flips | min detection |
|---|---|---|---|---|---|---|---|---|---|
| summary, 8 | 1.000 | 1.00 | 0.000 | abstains | – | abstains | – | – | – |
| summary, 16 | 0.560 [0.480, 0.640] | 1.00 | 0.419 | abstains | – | abstains | – | – | – |
| summary, 20 | 0.336 [0.286, 0.390] | 0.96 | 0.387 | 0.037 [0.026, 0.051] | 1.00 | **0.053** [0.035, 0.072] | 0.16 | 0.058 | 0.90 |
| summary, 32 | 0.997 [0.994, 1.000] | 1.00 | 0.003 | 0.001 [0.000, 0.003] | **0.54** | **0.028** [0.017, 0.043] | 0.04 | 0.033 | 1.00 |
| summary, 64 | 0.637 [0.607, 0.664] | 1.00 | 0.405 | 0.035 [0.026, 0.043] | 1.00 | **0.039** [0.030, 0.050] | 0.02 | 0.058 | 1.00 |
| quantiles, 8 | 1.000 | 1.00 | 0.000 | abstains | – | abstains | – | – | – |
| quantiles, 16 | 0.952 [0.928, 0.973] | 1.00 | 0.084 | abstains | – | abstains | – | – | – |
| quantiles, 20 | 0.773 [0.720, 0.828] | 1.00 | 0.269 | 0.026 [0.017, 0.038] | 0.98 | **0.051** [0.036, 0.067] | 0.12 | 0.063 | 1.00 |
| quantiles, 32 | 0.518 [0.469, 0.563] | 1.00 | 0.345 | 0.015 [0.009, 0.023] | 1.00 | **0.027** [0.017, 0.037] | 0.02 | 0.038 | 1.00 |
| quantiles, 64 | 1.000 [1.000, 1.000] | 1.00 | 0.000 | 0.000 [0.000, 0.000] | **0.42** | **0.049** [0.038, 0.061] | 0.08 | 0.067 | 1.00 |

Two to four channels (`dimension` family, quantiles, 30 replicates):
Ledoit–Wolf + cross-conformal FPR 0.044–0.053 (upper bounds ≤ 0.078), mclust
+ cross-conformal 0.023–0.045, upstream 0.32–1.00. Detection is low there for
every arm because some anomalies act only on channels outside the selected
set (`gain_shift` on channel 5, `population_loss` on channel 3).

### Findings

- **The upstream threshold is not a 5% rate.** The 5% quantile of the
  reference files' own log-densities under a mixture fitted on them flags
  32–100% of independent normal files in every summary/quantiles cell, at
  every reference size from 8 to 64. It is worst where mclust fits full covariances (summary, 32
  references > 24 features; quantiles, 64 > 54): the in-sample threshold
  sits above every held-out density. The reference-only out-of-fold
  exceedance of that threshold predicts the held-out FPR (Spearman 0.89 over
  480 replicate cells, bias +0.03), so a cohort can detect this without
  independent normals.
- **"Stable" upstream flags are often degenerate.** Where every normal file
  is flagged, flips are 0; elsewhere two seeds disagree on 8–42% of normal
  files (Fleiss κ 0.01–0.41), and the novelty margin's per-file SD across
  seeds reaches 1e13 log units where covariances are near-singular.
- **Seed ensembles and larger samples stabilise but do not calibrate.**
  Averaging 5 seeds' margins or using 2,500 / 5,000 events per file reduces
  flips (to 0 when every event is used), but FPR stays 0.36–1.00 and rises in
  some cells (`seed_ensemble_5`, `events_*` not eligible: paired FPR
  difference and flip-rate criteria). Seed variation is not the main error;
  the threshold is.
- **Cross-conformal thresholds calibrate both models; only Ledoit–Wolf keeps
  power.** With mclust, held-out references score so low under each fold's
  overfitted mixture that anomalies must fall below all of them: detection
  drops to 0.42–0.54 for some classes at 32–64 references. One shrunk
  Gaussian keeps detection at 1.00 for every class from 32 references.
- **The outlier flag is relative to its cohort.** In 40 normal files alone,
  23–25% are flagged; add 10 anomalies and none are (Wilson upper 0.003). A
  batch shift of the whole cohort does not change its rate (0.23–0.24 at
  every level).
- **Batch and instrument differences are novelty, not noise.** Against a
  control-batch reference (quantiles, 20 references, 20 replicates), the
  cross-conformal threshold on the mclust model flags shifted normal files at
  0.03 (same batch), 0.04 (+5% gain), 0.06 (+10%), 0.22 (+20%) and 1.00 for a
  ×1.5 true spillover under the nominal `SPILL` (0.51 at 64 references);
  doubled electronic noise is not detected (0.04). Upstream flags 0.78–1.00
  at every level, so it cannot tell a shift from its own miscalibration. A
  reference must match the instrument, panel and processing of the files it
  judges.
- **Aggregate methods** (`emd`, `binning`, `fingerprint`, 10 replicates):
  upstream FPR 0.09–0.51, mclust + cross-conformal 0.008–0.057; descriptive
  only.
- **Real donors.** In all 40 leave-one-out runs (4 donors × 5 methods × 4 / 10
  markers) the held-out normal donor is flagged and every seed gives the
  identical value (438–881 events, nothing subsampled). With 3 reference
  donors the conformal mode abstains. Population FPR is `not_assessed`.
- **The earlier seed study understated forest noise.** Its seeds 1/7/42/101
  shared 900–994 of 1,000 isolation trees; `run.py` now uses
  `--stability-seeds`.

### Decision

The rule (`DECISION_RULE` v2, `7a643c44…`) was frozen in `f3a41a0` before any
confirmation seed was used. It replaced v1 (`7ef5527a…`) after the
development profile: v1's flip bound (`≤ baseline + 0.02`) cannot be met
where the baseline flags every file, and the Ledoit–Wolf arm became a
calibration candidate because development showed the mclust arm losing
detection; both changes are recorded in the rule's `history`.

| Arm | Claim | Eligible | Failed criteria (confirmation) |
|---|---|---|---|
| `shrinkage_ledoit_wolf+cross_conformal` | calibration | **yes** | none, in 14 cells |
| `cross_conformal` (mclust) | calibration | no | detection at summary-32, quantiles-64 |
| `seed_ensemble_5` | stability | no | paired FPR difference, flip rate (15) |
| `events_2500`, `events_5000` | stability | no | paired FPR difference, flip rate (18, 12) |

The eligible arm is production's opt-in `--novelty-mode
conformal-ledoit-wolf-v1`
([crate README](../crates/cytiq-cytoscan/README.md#calibrated-novelty-mode)).
A port of scikit-learn's `LedoitWolf` matches it to 1e-12 in the shrinkage.
Re-running the confirmation profile with the final binary (`a57129d`,
`--rust-calibration`) reproduced every family metric above exactly, and
production's mode equalled the benchmark's composition in all 155,000
seed × file comparisons over 620 cells (decision and conformal count; no
near-ties). `events_per_file` stays library-only; no other default changes.

### Performance

Release build, 114 files of 5,000 events, 4 threads, median of 5 runs:
- **Default runs** take the same time as clean main (ratio 0.91–1.01).
- **`--stability-seeds N`** loads the files once, so its cost is sublinear:
  10 seeds take 2.2–3.6× a single run, 20 seeds 3.3–6.2×.
- **`--novelty-mode conformal-ledoit-wolf-v1`** takes 0.80–1.00× the
  upstream mode (ten Ledoit–Wolf fits cost less than mclust's model search).

### Limitations

- **FPR claims are synthetic.** The generator draws each channel from a
  two-normal mixture with per-file proportions; the benchmark shows what each
  decision rule does under exchangeable references and known shifts, not a
  clinical false-positive rate.
- **Marginal guarantee only.** Cross-conformal FPR is near 1/20 on average
  over reference cohorts; one cohort's rate varies (`P(FPR > 0.10)` 0.02–0.16)
  and there is no per-cohort guarantee. Below 19 references the mode
  abstains.
- **Real data.** Four donors from one CyTOF study, no spillover, too few
  events to subsample: they show the upstream flag's behaviour but cannot
  estimate a population FPR, and leave-one-out splits are not independent
  replications. A pinned real cohort with ≥ 20 independent normal donors,
  ideally across instruments, is needed for a real-data claim.
- **Scope of the mode.** Summary and quantiles features with 2–6 channels
  only; aggregate methods were descriptive and are refused.
