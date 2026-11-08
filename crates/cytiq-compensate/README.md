# cytiq-compensate

Applies a spillover or compensation matrix to the events of one FCS file.
Backs the `cytiq compensate` command.

## Usage

```bash
cytiq compensate --input sample.fcs --output sample.comp.fcs
cytiq compensate --input sample.fcs --output sample.comp.fcs \
  --matrix autospill/spillover.csv --matrix-kind spillover
```

From Rust:

```rust
use cytiq_compensate::{CompensateConfig, CompensateRunner, MatrixKind};

let result = CompensateRunner::run(&CompensateConfig {
    input: "sample.fcs".into(),
    output: "sample.comp.fcs".into(),
    matrix: Some(("spillover.csv".into(), MatrixKind::Spillover)),
    overwrite: false,
})?;
println!("{} events on a raw detector margin", result.margin_event_count);
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `CompensateRunner::run` / `CompensateConfig` / `CompensateResult` | The `cytiq compensate` command |
| `read_matrix_file`, `MatrixKind`, `CompensationMatrix` | Square autospill matrix files and the file's own `$SPILLOVER` |
| `masks_path` | `sample.comp.fcs` → `sample.comp.masks.parquet` |
| `CompensateError` | Error type for every failure mode |

## Output rules

`sample.comp.margins.json` is published atomically with the FCS and mask and
recorded in provenance. It identifies the `detector_saturation` rule, input
event count, per-channel upper bounds/counts and the union count; per-channel
counts can overlap. `margin_diagnostics_path` resolves this sidecar. Negative
fluorescence and observed extrema are not saturation rules.

- Without `--matrix`, the file's `$SPILLOVER`/`$SPILL` matrix is used.
  `--matrix` accepts a square CSV/TSV with a corner label, channel columns and
  one row per channel, as exported by AutoSpill. It requires `--matrix-kind`:
  `spillover` is inverted; `compensation` is applied as is. Rectangular matrices
  are rejected. Channels match `$PnN`, then `$PnS`; a missing channel is an error.
- Matrix channels hold `linear · compensation`, where linear values have
  `$PnE`/`$PnG` applied (`cytiq_fcs::linear_values`, as `cytiq gate` does).
  Other non-time channels whose log scaling or gain changes their values are
  written linearised, since the output declares `$PnE=0,0`; such channels lose
  `$PnG` and get a `$PnR` from their new values. Unchanged channels keep every
  keyword.
- The output records `CYTIQ_COMPENSATION` (`{source, channels}`) and keeps the
  source matrix only as `CYTIQ_SOURCE_SPILL`. Already compensated input,
  unmixed input (`CYTIQ_UNMIXING`) and input whose matrix channels were
  transformed are rejected.
- `<output>.masks.parquet` holds `margin_pass` per event: whether the raw
  values of the matrix channels and of every scatter channel stay below the
  detector limit `$PnR - 1` (`cytiq_fcs::margin_channels`,
  `cytiq_fcs::raw_margin_pass`, `MarginBounds::detector_limit`). Scatter is
  not compensated, but a saturated FSC or SSC detector is as much an artefact
  as a saturated fluorescence one. Unlike
  `PeacoQC::RemoveMargins`, observed minima and maxima and values below
  flowCore's −111 floor are not removed. Compensated and linearised channels
  are dropped from `CYTIQ_MARGINS`.
- Data is written as f32, or f64 when an unchanged channel would lose
  precision in f32 (for example `Original_ID` above 2^24).
- Existing outputs are replaced only with `overwrite`. The FCS and its mask
  are published together (`cytiq-publish`): a failed run leaves neither, and
  the previous pair stays in place.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for FCS I/O, `$SPILLOVER`, compensation and
  margin bounds
- [`cytiq-mask`](../cytiq-mask) for the margin mask
- `csv` for matrix files

## Tests

```bash
cargo test -p cytiq-compensate
cargo test -p cytiq-engine --test compensate_cli
```
