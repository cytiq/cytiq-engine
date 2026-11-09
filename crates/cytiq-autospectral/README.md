# cytiq-autospectral

Reference-based spectral unmixing from single-stain controls. Backs the
`cytiq autospectral` command.

Builds one reference spectrum per fluorophore plus one autofluorescence
spectrum, then unmixes every fully stained sample by ordinary least squares and
writes the abundances as FCS files.

## Algorithm

Follows the OLS path of AutoSpectral ([Burton et al., bioRxiv 2025](https://doi.org/10.1101/2025.10.27.684855); citation in [`CITATIONS.md`](../../CITATIONS.md#autospectral))
(parity target 1.8.7, see [`parity/REPORT.md`](../../parity/REPORT.md#23-autospectral)):

1. Load controls and samples in parallel; infer each single stain's
   `primary_channel` from `$PnN`, then `$PnS`, when the manifest omits it.
2. Resolve spectral detectors: the `--detectors` manifest, or every channel of
   the first control present in all files, excluding FSC/SSC (any `fsc*` /
   `ssc*` name) and `Time`.
3. Clean each control (`clean_controls`, the default): drop raw
   detector-margin events and keep the `AutoSpill` KDE scatter gate
   (`cytiq_density::gate_events`). CLI `--quantile-gate` keeps the 1st–99th
   percentile box on FSC and SSC instead, for controls too small for the KDE
   gate; on real cell controls it keeps debris and saturated events that bias
   the spectra.
4. Every spectrum is `get.fluorophore.spectra`'s: on its peak detector 1, on
   every other detector the slope of `MASS::rlm` (Huber, MAD scale, at most
   100 IRLS steps, `lm` when it does not converge) of that detector on the
   peak detector, divided by the largest value.
5. Autofluorescence: the slopes over the gated AF control, against the
   detector with the highest mean.
6. For each single stain, positives are events above the 99th percentile
   (R type 7) of the AF control's primary channel, keeping the brightest 500
   (at least 20). Negatives are the AF control's 3 nearest events to each
   positive on raw FSC/SSC (`clean.controls`' scatter matching), at most
   1,000, evenly spaced where upstream samples at random. The slopes run over
   positives and negatives together.
7. Check conditioning: warn when the fluorophore-only condition number exceeds
   the number of fluorophores; fail above `1e8` for the full matrix.
8. Unmix each sample with the least-squares matrix `M⁺ = R⁻¹Qᵀ` (Householder
   QR; equal to `(MᵀM)⁻¹Mᵀ` but without squaring the condition number) in event
   chunks of 8192 and record per-sample residual and negative-fraction QC.

Abundances are in units of each fluorophore's peak detector. Known gaps against
upstream (the `AutoSpill` KDE gate instead of AutoSpectral's density gates, no
autofluorescence removal from the cell controls, no WLS/Poisson/per-cell AF)
are listed in the parity report: on real Aurora controls the fluorophore
spectra still match AutoSpectral's to a cosine of 0.9998 (§2.10). Solver,
gate quantiles and event limits are fixed public constants, not
configuration.

## Usage

```bash
cytiq autospectral \
  --controls controls.csv --samples samples.csv \
  --input-dir ./fcs --output ./autospectral-out \
  [--detectors detectors.csv] [--debug-dir ./debug] \
  [--fsc FSC-A] [--ssc SSC-A] [--threads 4] [--quantile-gate]
```

From Rust:

```rust
use cytiq_autospectral::{
    AutoSpectralConfig, AutoSpectralRunner, read_controls_manifest, read_samples_manifest,
};

let controls = read_controls_manifest("controls.csv")?;
let samples = read_samples_manifest("samples.csv")?;
let config = AutoSpectralConfig::new("autospectral-out".into());
let summary = AutoSpectralRunner::run(&controls, &samples, "./fcs", &config)?;
println!("condition number {:.3e}", summary.condition_number);
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `AutoSpectralRunner::run` / `AutoSpectralConfig` | Full pipeline: load, build references, unmix, write outputs |
| `AutoSpectralRunner::input_files` | The control and sample files a run reads, for provenance |
| `AutoSpectralSummary` | Returned run summary (also `diagnostics/run.json`) |
| `read_controls_manifest`, `read_samples_manifest`, `read_detectors_manifest` | CSV readers with validation |
| `ControlManifestRow`, `SampleManifestRow`, `DetectorSpec` | Manifest rows |
| `write_outputs` | Output writer used by the runner |
| `SOLVER`, `POSITIVITY_THRESHOLD`, `POSITIVE_EVENT_LIMIT`, `NEGATIVE_EVENT_LIMIT`, `RLM_MAX_ITERATIONS`, `SCATTER_NEIGHBOURS`, `MIN_*`, `CONDITION_NUMBER_LIMIT`, `SCATTER_GATE_*`, `AUTO_EXCLUDED_CHANNELS`, ... | Fixed algorithm constants |
| `AutoSpectralError` | Error type for every failure mode |

## Manifest rules

Controls manifest:

```csv
filename,dye,primary_channel
controls/unstained.fcs,Autofluorescence,
controls/FITC.fcs,FITC,FITC
controls/PE.fcs,PE,PE
```

Samples manifest:

```csv
filename
samples/sample_01.fcs
samples/sample_02.fcs
```

The optional detectors CSV uses the [AutoSpill detector format](../cytiq-autospill/README.md#manifests).

- `filename` values must be relative; they resolve against `input_dir`.
- Exactly one control with `dye` `Autofluorescence` (or `AF`) and at least one
  single stain; dyes must be unique.
- `fluorophore` and `channel` are accepted as aliases for `dye` and
  `primary_channel`; an empty `primary_channel` means "infer".
- Channel matching ignores case and a trailing `-A` / `-H` / `-W`.
- Controls and samples must be raw acquisition files: a file with
  `CYTIQ_COMPENSATION`, `CYTIQ_UNMIXING` or `CYTIQ_TRANSFORM` is
  `ProcessedInput`, since unmixing fits raw detector values and the output
  keeps no earlier `CYTIQ_*` state.

## Outputs

```text
<output>/
  references/fluorophore_spectra.csv        component,kind,<detectors...>
  references/autospectral-references.json   detectors, spectra, condition number
  unmixed/<sample-stem>.unmixed.fcs         non-detector columns + one column per component
  unmixed/<sample-stem>.unmixed.masks.parquet  raw `margin_pass` per event (detectors and scatter)
  diagnostics/run.json                      AutoSpectralSummary
<debug-dir>/summary.json                    same summary, only with --debug-dir
```

- Unmixed files are FCS 3.1. Components are rounded to float32; copied channels
  keep exact values, so the file uses float64 when one does not fit float32
  (for example `Original_ID` above 2^24). Detector columns are replaced by component
  columns; other parameters (scatter, time, ...) are copied with their `$Pn*`
  keywords. Acquisition keywords are carried over; the spillover matrix and
  earlier `CYTIQ_*` keywords are dropped, except `CYTIQ_ACQUISITION`.
  `CYTIQ_UNMIXING` lists the components
  and detectors, and `CYTIQ_MARGINS` the bounds of the copied channels.
- Components have no detector margins: `peacoqc --remove-margins` refuses them,
  and `gate` refuses to compensate them. Use the raw `margin_pass` mask instead.
- `Time` and `Original_ID` are never inferred as detectors; explicitly selecting
  `Original_ID` is an error.
- Duplicate sample stems get a `-<row>` suffix.
- Existing output files are replaced; there is no `--overwrite` check.

## Scientific diagnostics

`diagnostics/run.json` includes `reference_diagnostics`: AF peak detector and
detector means, singular values, fluorophore-only conditioning, pairwise spectral
cosines, and `unit_noise_covariance = P Pᵀ` for the OLS inverse operator P.
This covariance assumes independent unit-variance detector noise; it measures
noise sensitivity and is not empirical spreading. Sample diagnostics also report
per-detector residual RMS, relative residual RMS (null for a zero signal norm),
and component negative fractions. High cosine, poor conditioning or a small
residual alone cannot establish biological accuracy.

OLS Householder QR remains the baseline. Controls are fitted before sample DATA
are loaded; per-event solve buffers are reused without changing operation order.
Offline AF/bootstrap and alternative-solver experiments live in
[`parity/scientific`](../../parity/scientific). WLS requires calibrated detector
variances, Poisson likelihood requires count-scale measurements, and multiple AF
components require independent identifiable references; none is inferred from
arbitrary ADC fluorescence or normalized CyTOF data.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for reading and writing FCS
- `nalgebra` (QR, SVD), `rayon` (parallel loading and unmixing),
  `csv`, `serde` / `serde_json`

## Tests

```bash
cargo test -p cytiq-autospectral
cargo test -p cytiq-engine --test autospectral_cli
```

Integration tests run on `data/fixtures/autospectral/` (unstained, FITC and PE
controls plus a stained sample).
