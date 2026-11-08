# Real-data validation

`parity/real/run_real.sh` (or `just parity-real`) runs cytiq and the upstream
R tools on real acquisitions and compares them per event. It needs
`parity/setup_env.sh` once. The datasets are listed in
[`datasets.json`](datasets.json): each file comes from an installed R package,
a URL, or a member of the dataset's zip `archive` (downloads and extracted
members are cached in `parity/.data/real-cache`), and must match its SHA-256.
A dataset's `chain` picks the comparison:

- `qc` works for any file with `$SPILL`;
- `autospill` takes single-stain controls, listed in the entry's `controls`
  table (filename, detector `$PnN` as `dye`, antigen, wavelength), and
  compares `cytiq autospill` with R autospill;
- `autospectral` takes spectral single-stain controls under `controls/`, the
  AutoSpectral control file as `control_file.csv` (cytiq reads it as its
  controls manifest) and stained samples under `sample/`, runs AutoSpectral's
  workflow (`define.flow.control`, `clean.controls`,
  `get.fluorophore.spectra`, `unmix.ols`) for the cytometer named by
  `autospectral_cytometer`, and compares spectra and unmixed abundances;
- `flowsom_gating` is specific to FlowSOM's 68983 sample;
- `classify` runs `cytiq classify` with the cytiq-ml bundle in
  `CYTIQ_CLASSIFY_BUNDLE` on every file and compares it with that model's
  native probabilities in `CYTIQ_CLASSIFY_GOLDEN` (written by cytiq-ml's
  `scripts/export_classify_golden.py`, which also records the model and file
  hashes the comparison checks). Skipped when either variable is unset.

To add a dataset, add an entry with its source, licence, instrument, SHA-256,
cytometry type and panel. A downloaded dataset is marked `optional`: it is
skipped with a warning when the download fails, and the tolerance rules that
only it can satisfy are marked `optional` too.

| Dataset | Source | What is compared |
| --- | --- | --- |
| `flowsom_68983` | FlowSOM `extdata/68983.fcs`: 19,225-event, 18-colour mouse sample; `manualGating.xml` and the FlowJo labels of every event (`gatingResult.csv`, from `gating.wsp`) | `cytiq compensate` and `transform --method logicle` against flowCore; the manual Gating-ML strategy in `cytiq gate` against FlowKit and against the FlowJo labels; `cytiq flowsom` against R FlowSOM (two seeds give R's own spread) and against the FlowJo labels |
| `peacoqc_111` | PeacoQC `extdata/111.fcs`: 10,000-event, 20-colour LSRII sample from the PeacoQC vignette | detector-limit margins, compensation, logicle and PeacoQC against flowCore and R PeacoQC |
| `celesta_mdsc_controls` | [Zenodo 21384789](https://doi.org/10.5281/zenodo.21384789) (CC-BY-4.0, 92 MB archive): 11 single-stain mouse cell controls, 5,000 events each, BD FACSCelesta (4-Blue 3-Red 5-Violet, FACSDiva 9.0) | `cytiq autospill` against R autospill: spillover matrix and the gate of every control |
| `celesta_mdsc_sample` | the same archive: `Myeloid SP_042.fcs`, 56,458-event mouse spleen sample with the FACSDiva compensation | as `peacoqc_111` |
| `symphony_omip24_controls` (slow) | [Zenodo 22994607](https://doi.org/10.5281/zenodo.22994607) `Table 8.zip` (CC-BY-4.0, 193 MB): 24 single-stain human PBMC controls of a 24-colour OMIP, 27,000–60,000 events each, BD FACSymphony A3 or A5 (`5B-3R-8V-7UV-3YG`, FACSDiva 9.1; the record does not name the model) | as `celesta_mdsc_controls`; R autospill needs about 25 minutes, so it runs only with `CYTIQ_REAL_SLOW=1` |
| `aurora_pbmc_spectral` | [Mendeley Data xzt3h3gnx9](https://doi.org/10.17632/xzt3h3gnx9.1) (CC-BY-4.0, 340 MB): AutoSpectral's own example, a 9-colour human PBMC panel on a 5-laser Cytek Aurora (64 detectors): an unstained and 9 single-stain cell controls (60,000–81,000 events each), one fully stained sample (418,152 events) | `cytiq autospectral` against R AutoSpectral: spectra per fluorophore (cosine, and AutoSpectral's own spread without `clean.controls`), the AF spectrum apart, the unmixing solver on the same spectra, and abundances of every 10th sample event, also with AutoSpectral's AF spectrum swapped in |
| `flowcyt_case27`, `flowcyt_case4` (slow) | [Yareta FlowCyt archive](https://doi.org/10.26037/yareta:54dmqf5nj5d2hoyaprnzh52rz4) (CC-BY-4.0, 3.4 GB): `Case27_A.fcs` (209,970 events) and `Case4_A.fcs` (624,323), human bone marrow, 10 colours, Beckman Coulter Navios exported by Kaluza (FCS 3.0, big-endian float32, `$SPILLOVER`) | as `peacoqc_111`; PeacoQC removes 4.8% and 10.5% of their events |
| `flowcyt_classify` (slow) | the same archive: case 27's five labelled Kaluza gates (`O/N/G/P/K`) and all its events (`A`); case 27 is in cytiq-ml's FlowCyt test split | `cytiq classify` against the native cytiq-ml model, with `CYTIQ_CLASSIFY_BUNDLE` and `CYTIQ_CLASSIFY_GOLDEN` set |
| `user_<name>` | every `*.fcs` with `$SPILL` in `$CYTIQ_REAL_DATA` | as `peacoqc_111`, for data that cannot be committed |

The run also executes the ignored >100 MB FCS round-trip test of
`cytiq-fcs`. Metrics go to `parity/results/metrics_real.json`; the run exits
with 1 when a rule in `parity/real/tolerances.json` is violated.

Notes:
- FlowJo exports channel names as `Comp-<detector>` and omits the
  `compensation-ref` Gating-ML 2.0 requires; the run compensates with
  `cytiq compensate` first and feeds both cytiq and FlowKit the same corrected
  strategy (`uncompensated` dimensions on the compensated file).
- FlowJo's own counts for `NK T cells` (535) differ from evaluating its
  exported Gating-ML (487, in cytiq and FlowKit alike): FlowJo evaluates that
  gate at `gateResolution="256"` on biex axes. See `parity/REPORT.md` §2.10.
- Downloads use `curl`, which trusts the system certificate store (Python's
  `urllib` rejects TLS-inspecting proxies).
- The FlowCyt datasets need the 3.4 GB Yareta archive, so they run only with
  `CYTIQ_REAL_SLOW=1`. Yareta builds the ZIP on request, so a fresh download
  may not match the pinned archive digest even though every member does. With
  a local copy, seed the cache instead of downloading:
  `ln FlowCyt.zip parity/.data/real-cache/9d588510a90dab3ddc6929cb5a2af2e7a5b038b64a3857dcb534092dd1af73a2`.
  For the classify dataset, export the golden from cytiq-ml with
  `uv run --extra boosting python scripts/export_classify_golden.py <bundle> <golden-dir> Case27_{O,N,G,P,K,A}.fcs`
  and set `CYTIQ_CLASSIFY_BUNDLE=<bundle> CYTIQ_CLASSIFY_GOLDEN=<golden-dir>`.
- AutoSpectral's AF spectrum is its least stable output: it is the `rlm`
  slope against the AF control's brightest detector on average, over 1,000
  random events. Between random draws it moves to a cosine of 0.91–0.94, and
  on this panel UV7-A and V7-A have means within 1%, so cytiq's gate picks
  V7-A where AutoSpectral's picks UV7-A. The AF spectrum is reported apart;
  with AutoSpectral's AF spectrum swapped in, cytiq's fluorophore spectra
  give AutoSpectral's abundances (correlation ≥ 0.999).
- On FACSDiva float data flowCore's −111 floor flags noise around zero, not
  saturation: `PeacoQC::RemoveMargins` removes 17,737 events of
  `celesta_mdsc_sample` (31%), 17,700 of them at or below −111, when assessing
  the fluorescence channels. The upper detector limit `$PnR − 1` flags 36 on
  those channels. The current compensation mask also assesses scatter
  saturation, giving 1,222 events in total; upstream `RemoveMargins` on the
  same extended channel set removes 18,859. Every physically saturated event
  is also removed by `RemoveMargins` (`kept_by_RemoveMargins` = 0).
  See `parity/REPORT.md` §2.10 and §3.
- The AutoSpill datasets on FlowRepository (FR-FCM-Z2SS/Z2ST/Z2SV) are not
  fetched automatically: flowrepository.org serves a certificate that expired
  in March 2023 with an incomplete chain, so it cannot be downloaded over
  verified TLS. Download them by hand and point `CYTIQ_REAL_DATA` (QC chain) or
  `cytiq autospill` at them.

## CytoNorm pilot

```bash
just parity-setup                 # once, if parity/.env is absent
just parity-cytonorm-real
```

This separate pilot uses the six real CyTOF control excerpts included in
CytoNorm 2.0.12, pinned by SHA-256 in `parity/cytonorm_real.R`. The upstream
[dataset description](https://github.com/saeyslab/CytoNorm/tree/278f352d8edffd7a5d3e6b0b76c402d991a1e7a9#identifying-the-data)
identifies three batches (PTLG021, PTLG028, PTLG034), donor 1 as training
controls and donor 2 as validation. Each file contains the first 1,000 cells
of an acquisition from FR-FCM-Z247. No data downloads are needed after setup.

The pilot uses the README's 37 biological markers and arcsinh cofactor 5.
The strict cytiq input contract requires two explicit adapters on prepared
copies: missing marker labels are filled with their physical channel names,
and `cytiq compensate` applies a saved identity matrix because these CyTOF
files have no spillover matrix. The script asserts exact value preservation
through both steps. This matrix is a test adapter, not an estimate of mass
cytometry spillover; the original package data remain unchanged.

For this small cohort, settings are a 5×5 SOM, five metaclusters, 3,000 training
events, the default minimum 100 events per mapping, and explicit `sparse_policy: identity`.
Unsupported mappings retain their original values; their counts and reasons
remain visible. These are pilot settings, not a recommended production panel.

The run fails on checksum mismatch, CLI errors, nonfinite output, changed
untouched channels/event order/acquisition identity, validation affecting any
fitted artifact, or differences from independent R targets (>1e-12) and
upstream splines (>1e-9). The numerical comparison independently determines
eligible training distributions, then holds Rust cluster assignments fixed.
It also checks exact identity behavior for unsupported mappings. It reports
independent full-pipeline R clustering ARI and within-donor pairwise batch
distances before/after normalization; neither has an automatic pass threshold.
The latter is an equal-weight mean across cluster/marker/batch-pair comparisons
of mean absolute distances on the 1–99% quantile grid, not upstream histogram EMD.
Validation distances compare donor 2 across batches, rather than to donor 1.

Generated files live in the ignored `parity/.out/cytonorm-real/` directory:

- `batches.json`, `prepared/`, and `identity-compensation.csv`: reproducible inputs;
- `model/`: portable model, training/validation QC, and provenance;
- `normalized/`: all six normalized FCS files with QC and provenance;
- `diagnostics/report.html`: interactive spline and distribution plots;
- `metrics.json` and `batch-distances.csv`: checks and descriptive measurements.
- `spline-errors.csv` and `spline-comparison.pdf`: numerical differences from R.

The compact metrics are also saved in `parity/results/metrics_cytonorm_real.json`,
including when the numerical check fails.

Run from the repository root. An optional binary and output directory can be
passed directly: `R_LIBS_USER=parity/.env/Rlib Rscript parity/cytonorm_real.R
target/release/cytiq parity/.out/cytonorm-real-release`. This pilot is separate
from `just parity-real`; it does not establish biological comparability or
performance on full acquisitions.

### Observed result (2026-10-28)

The first real-data run fails numerical parity: targets agree to 1.8e-15,
but normalized signals differ from R by up to 0.0570 inside the knot range
and 0.3545 in the tails. The largest difference is CD45, PTLG028, donor 2,
metacluster 5, event 171: input 1.919912 maps to 1.491151 in cytiq and
1.136678 in R. Inspecting the fitted coefficients against R `splinefun`
shows matching knots with different derivatives. For this batch/cluster/marker,
the lower endpoint slope is 1.414269 in cytiq versus 1.907556 in R.
The stricter circular slope bound in `MonotoneSpline::fit` needs investigation
before claiming numerical parity on real data. The harness retains the 1e-9
tolerance and exits non-zero after saving all measurements.

Clustering agrees closely (ARI 0.991–1.000), and value preservation,
validation exclusion, and thread determinism pass. Mean pairwise quantile
distance falls 38.2% on training and 8.0% on validation. Of 555 mappings,
222 (40%) use identity fallback for the two small populations; these account
for 9,694 of 222,000 event/marker values (4.37%). These descriptive distances
include unchanged sparse mappings and do not establish biological validity.
