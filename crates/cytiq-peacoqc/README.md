# cytiq-peacoqc

PeacoQC-compatible, peak-based acquisition quality control: removes unstable
acquisition regions (clogs, drift, bursts) from compensated and transformed FCS
files. Backs the `cytiq peacoqc` command.

- Paper: Emmaneel et al., *PeacoQC* (<https://doi.org/10.1002/cyto.a.24501>)
- Upstream R implementation: <https://github.com/saeyslab/PeacoQC>

## Algorithm

Events are processed in file (acquisition) order:

1. optional margin removal: drop events at the detector boundaries
2. split events into overlapping bins
3. per bin and channel, find peaks of a `stats::density`-compatible KDE
   (`FindThemPeaks`), giving a bin × peak-feature matrix
4. Isolation Tree (`it`) on the peak matrix, when there are at least 150 bins
5. MAD outliers (`mad`) on `smooth.spline(spar = 0.5)`-smoothed peak series
6. drop good regions shorter than the consecutive-bins limit

`--method` selects `all` (IT + MAD), `mad`, `it`, or `peaks-only`. Tuning
values (MAD 6, IT limit 0.6, 5 consecutive bins, ...) are fixed `DEFAULT_*`
constants aligned with upstream defaults. On the parity dataset the mask
matches R PeacoQC 1.20.0 at 100% in every mode
([`parity/REPORT.md`](../../parity/REPORT.md), section `peacoqc`).

## Usage

```bash
cytiq peacoqc --input sample.fcs --output sample.peacoqc.fcs \
  --channel "355_378/28_uH-A [CD3 BUV395]" --channel "405_750/40_vB-A [CD4 BV750]" \
  --method all --remove-margins --debug-dir ./peacoqc-debug
```

From Rust:

```rust
use cytiq_peacoqc::{PeacoQcConfig, PeacoQcMethod, PeacoQcRunner, parse_channel_list};

let mut config = PeacoQcConfig::new(parse_channel_list("CD3 BUV395,CD4 BV750")?);
config.method = PeacoQcMethod::Mad;
let result = PeacoQcRunner::run("sample.fcs", "sample.peacoqc.fcs", &config)?;
println!("kept {}", result.summary.metrics.events_after);
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `PeacoQcConstants` | The fixed algorithm constants, recorded in provenance `constants` |
| `PeacoQcRunner::run` / `PeacoQcConfig` / `PeacoQcResult` | The `cytiq peacoqc` command |
| `PeacoQcMethod` | `All`, `Mad`, `It`, `PeaksOnly` |
| `PeacoQcSummary`, `PeacoQcParameters`, `PeacoQcMetrics` | `summary.json` contents |
| `EventMaskRecord` | Per-event result (`mask.csv` row) |
| `parse_channel_list` | Split a comma-separated `--channels` value |
| `peacoqc_mask` | The good-event mask of compensated, transformed channels held in memory (margins are the caller's), as `run` computes it; used by `cytiq spillqc` |
| `DEFAULT_*` | Fixed algorithm constants |
| `PeacoQcError` | Error type for every failure mode |

## Output rules

- Input should be compensated and transformed. A pending spillover matrix, or
  selected channels without `CYTIQ_TRANSFORM` coverage, are reported in
  `summary.warnings` and the log; the run continues.
- Input and output are FCS; an output path without a `.fcs` extension is an
  error. An existing output file is replaced.
- The output is FCS 3.1 with the retained events of every input parameter,
  plus an `Original_ID` column (the 1-based input row) unless the input already
  has one. It is float32, or float64 when float32 would change a value (for
  example `Original_ID` above 2^24). Values are unchanged, so every source
  keyword is kept, including the spillover matrix and the original `$PnR`.
- `<output>.masks.parquet` holds one row per input event in the
  [`cytiq-mask`](../cytiq-mask) layout: `event_id`, `source_event_id`,
  `margin_pass`, `margin_lower_pass`, `margin_upper_pass`, `peacoqc_pass` and
  `keep` (null = not assessed). Lower/upper rule columns preserve exact event
  diagnostics without changing the aggregate `margin_pass` policy contract.
- `remove_margins` uses raw detector bounds, or the bounds recorded in
  `CYTIQ_MARGINS` by `cytiq transform`. Channels transformed or compensated
  without recorded bounds are an error.
- `--remove-margins` follows `PeacoQC::RemoveMargins`: raw bounds include
  `$PnR - 1` and flowCore's low bound (−111). On float data the low bound
  can flag ordinary noise around zero; the margin mask from
  [`compensate`](../cytiq-compensate/README.md) uses the upper limit only.
  The flag is disabled by default and emits an explicit legacy-filter warning.
  Unprocessed flowCore files honor their `flowCore_$PnRmin/Rmax` metadata;
  Cytiq-recorded bounds retain priority for derived signals.
- `--remove-zeros` ignores zero values during peak detection (useful for mass
  cytometry).
- Channels match `$PnN`, then `$PnS`, then `$PnN [$PnS]`.
- With `debug_dir`, writes `summary.json`, `mask.csv` and `report.html`
  (Plotly from the CDN).
- `<output stem>.summary.json` is always published with the FCS and mask and
  recorded in provenance. Its per-channel margin diagnostics include thresholds
  in the input file's signal space, lower/upper counts and their union count.
- `summary.json` `status` is `WARN` when `warnings` is not empty, otherwise
  `PASS`. It does not judge how much was removed; panel thresholds are
  checked by `cytiq qc`.
- `threads: Some(n)` runs in a local rayon pool.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for reading and writing FCS
- [`cytiq-mask`](../cytiq-mask) for `<output>.masks.parquet`
- [`cytiq-density`](../cytiq-density) for the `stats::density` port
- `rayon`, `csv`, `serde_json`, `plotly`

## Tests

```bash
cargo test -p cytiq-peacoqc
cargo test -p cytiq-engine --test peacoqc_cli
```
