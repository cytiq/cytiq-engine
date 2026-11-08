# cytiq-fcs

Strict FCS 3.0 / 3.1 / 3.2 reader, a compact FCS 3.1 float writer, and
spillover / compensation helpers. It has no command of its own: every other
`cytiq` crate that touches FCS files goes through it.

Used by `cytiq-autospill`, `cytiq-autospectral`, `cytiq-clean`,
`cytiq-cytoscan`, `cytiq-flowsom`, `cytiq-gating`, `cytiq-inspect`,
`cytiq-peacoqc`, `cytiq-transform` and `cytiq-engine`.

## What it does

Reading (`FcsReader`):

- parses the 58-byte HEADER and the primary TEXT segment, then the
  supplemental TEXT segment when `$BEGINSTEXT`/`$ENDSTEXT` point to one
- keeps TEXT keywords both as ordered `[key, value]` pairs per segment and as a
  lookup map; escaped delimiters and whitespace-padded layout keywords are
  handled
- resolves DATA from `$BEGINDATA`/`$ENDDATA` and checks it against the HEADER
  offsets; ANALYSIS is optional and returned as raw bytes
- validates that all segments lie inside the file and do not overlap
- decodes list-mode (`$MODE=L`) DATA of type `A`, `I`, `F` or `D`, with
  per-parameter widths and `$BYTEORD`, into one column per parameter
  (`ColumnData::UInt`, `F32` or `F64`)

`assess_detector_saturation` returns a physical upper-limit mask and per-channel
diagnostics; `raw_margin_pass` retains its existing API and delegates to it.
Negative signal and observed extrema do not define physical saturation.
Legacy flowCore/PeacoQC thresholds remain explicitly separate. Invalid bounds,
non-finite detector values and invalid channel indices return errors.

The reader is strict by design: anything that does not match the spec is an
error rather than a best-effort guess (duplicate keywords, HEADER/TEXT offset
mismatch, non-UTF-8 TEXT, unsupported `$MODE`/`$BYTEORD`/widths, DATA length
mismatch).

It also never lets a size declared in the file drive an allocation or a loop
before that size is checked. Every segment is checked against the file length
before it is read, and DATA against `$TOT` × row width. TEXT is capped at
16 MiB and `$PAR` at 65,535 (`FcsError::LimitExceeded`). `FcsReadLimits` caps
what a read allocates for DATA and ANALYSIS, checked before either is read:
the decoded columns at 8 GiB (8 bytes per `A`, `I` or `D` value, 4 per `F`)
and ANALYSIS at 16 MiB by default; `read_with_limits` takes other limits.
DATA is decoded 1 MiB at a time, so a read holds the columns and one chunk,
never the raw segment as well.

`FcsReader::stream(batch_size, limits)` checks a file exactly as
`read_with_limits` does, then returns its `FcsMetadata` (everything but the
events and ANALYSIS) and `EventBatches`, an iterator that decodes DATA
`batch_size` events at a time in file order. A batch decodes to the columns
`read` gives for those rows, bit for bit; memory holds one batch, not the file.
A stream never reads ANALYSIS, so `analysis_bytes_max` does not apply to it.
After a batch fails to decode, the iterator ends.

Writing (`write_fcs3_1_f32*`, `write_fcs3_1_f64*`): FCS 3.1, little-endian
32-bit or 64-bit float, one column per parameter, optional `$PnS` labels and
extra TEXT keywords. `write_fcs3_1_exact_with_options` takes f64 columns and
writes f32 unless a value would change (event identifiers above 2^24, 32-bit
integer data). This is the layout used for every FCS file `cytiq` generates;
it is not a general writer. TEXT is delimited by `|`, escaped by doubling, so a
key or value that is empty or starts with `|` is refused rather than written
ambiguously.

Acquisition identity: `acquisition_id` returns the `CYTIQ_ACQUISITION` keyword
of a derived file, or the SHA-256 of a raw file. Stages that write FCS record
it with `set_acquisition`, and event masks carry it, so masks only apply to
files of their own acquisition.

Compensation: `Spillover` parses `$SPILLOVER` / `$SPILL` / `SPILLOVER` /
`SPILL` (case-insensitive), `invert_matrix` inverts it with Gauss-Jordan
elimination and partial pivoting, and `compensate` applies the inverse to event
columns.

## Usage

```rust
use cytiq_fcs::{
    FcsReader, FcsWriteOptions, FcsWriteParameter, Spillover, compensate,
    write_fcs3_1_f32_with_options,
};

let dataset = FcsReader::open("raw.fcs")?.read()?;
println!("{} {} events", dataset.version.as_str(), dataset.event_count());

let fsc = dataset.find_parameter("FSC-A").expect("FSC-A present"); // $PnN, then $PnS
let values: Vec<f64> = dataset.data.columns[fsc].to_f64_vec();

if let Some(spillover) = Spillover::from_dataset(&dataset)? {
    let inverse = spillover.compensation_matrix()?;
    // One Vec<f64> per channel, in spillover.channels() order.
    let columns: Vec<Vec<f64>> = load_channels(&dataset, spillover.channels());
    let compensated = compensate(&columns, &inverse);
}

write_fcs3_1_f32_with_options(
    "out.fcs",
    &[FcsWriteParameter::with_label("FSC-A", "Forward scatter")],
    &[vec![1.0_f32, 2.0, 3.0]],
    &FcsWriteOptions::new().with_text_keyword("CYTIQ_EXAMPLE", "1"),
)?;
```

## Public API

| Item | Purpose |
| --- | --- |
| `FcsReader::open` / `FcsReader::new` / `FcsReader::read` | Read a file or any `Read + Seek` into `FcsDataset` |
| `FcsReader::read_with_limits`, `FcsReadLimits` | Read with other bounds on decoded DATA and ANALYSIS bytes (`Default` is what `read` uses) |
| `FcsReader::stream`, `FcsStream`, `FcsMetadata`, `EventBatches` | Check a file as `read` does, then decode its events in batches; `EventBatches::empty_batch` gives the columns with no rows |
| `FcsDataset` | `version`, `header`, `text`, `parameters`, `data`, `analysis`; `event_count`, `find_parameter` |
| `Text` (`FcsText`), `TextSegment` | TEXT keywords: `get` lookup plus ordered pairs of primary and supplemental segments |
| `Parameter`, `ParameterDataType`, `ParameterWidth` | Per-parameter metadata including all `$Pn*` keywords |
| `EventColumns`, `ColumnData` | Decoded event columns; `ColumnData::to_f64_vec` |
| `FcsVersion` | `V3_0` / `V3_1` / `V3_2`, `as_str()` gives `FCS3.x` |
| `find_parameter_index` | Match `$PnN`, then `$PnS`, then `"$PnN [$PnS]"` |
| `write_fcs3_1_f32`, `..._with_options`, `..._to_writer`, `..._to_writer_with_options` | FCS 3.1 float32 writer |
| `write_fcs3_1_f64_with_options`, `write_fcs3_1_f64_to_writer_with_options` | FCS 3.1 float64 writer, for values f32 cannot hold exactly |
| `write_fcs3_1_exact_with_options`, `needs_f64` | Writer that picks f32 or f64 so every value is stored exactly |
| `acquisition_id`, `set_acquisition`, `is_sha256`, `ACQUISITION_KEYWORD` | Raw acquisition a file belongs to (`CYTIQ_ACQUISITION`) |
| `FcsWriteParameter` (`from_source`, `with_range`, `with_keyword`), `FcsWriteOptions`, `carried_keywords` | Writer parameters, extra TEXT keywords, and source keywords a derived file can keep |
| `Spillover`, `invert_matrix`, `compensate`, `is_spillover_keyword` | Spillover parsing and compensation |
| `condition_number_1` | 1-norm condition number of a spillover matrix |
| `linear_values` | One parameter on a linear scale (`$PnE`, `$PnG`, `$TIMESTEP`), as FlowKit reads it |
| `log_amplified_parameters`, `ensure_linear_scale` | Parameters stored on a log scale (`$PnE` other than `0,0`); commands that read stored values instead of `linear_values` refuse them (`FcsError::LogAmplified`) |
| `MarginBounds`, `dataset_margin_bounds`, `recorded_margin_bounds`, `encode_margin_bounds`, `MARGINS_KEYWORD` | Detector margin bounds (flowCore `minRange`/`maxRange`), recorded in `CYTIQ_MARGINS`; `MarginBounds::from_flowcore_parameter` also honours the `flowCore_$PnRmin`/`Rmax` keywords of flowCore-written files |
| `raw_margin_pass` | Per-event pass flags for the raw detector limits of some channels (used before compensation and unmixing; the upper detector limit `$PnR - 1` only, unlike the `PeacoQC` rule in `MarginBounds::thresholds`, which also cuts the observed extremes and flowCore's low bound) |
| `margin_channels` | The channels those flags judge: the detectors compensation or unmixing mixes, plus every scatter channel, since saturated scatter is as much an artefact |
| `is_scatter_channel` | Whether a `$PnN` is light scatter (`FSC…`/`SSC…`, a Navios's `FS INT`/`SS INT`/`SS PEAK`/`SS TOF`), so `transform`, `plot` and the margin mask agree |
| `transform_state`, `compensation_state`, `unmixing_state`, `TransformState`, `CompensationState`, `UnmixingState`, `TRANSFORM_KEYWORD`, `COMPENSATION_KEYWORD`, `UNMIXING_KEYWORD`, `SOURCE_SPILLOVER_KEYWORD` | Signal state written by cytiq stages |
| `SignalState` (`of`, `is_raw`, `transformed`, `compensated`, `unmixed`, `Display`) | Every recorded step at once, so stages check their input the same way |
| `NormalizationState`, `normalization_state`, `NORMALIZATION_KEYWORD` | Validated `CYTIQ_NORMALIZATION` v1: model/panel SHA-256, batch and normalized channels |
| `TransformContract::of` | Complete comparable transform method and parameters, requiring explicit selected-channel coverage |
| `source_event_ids`, `ORIGINAL_ID_PARAMETER` | Raw-file rows of a derived file's events, from `Original_ID` |
| `Error` (`FcsError`) | Error type for every failure mode |

## Writer rules

- At least one parameter and one event; all columns the same length.
- Every value must be finite; `NaN`/`inf` is an error.
- Parameter names must be non-empty.
- Extra TEXT keywords must not override generated keywords (`$Pn*`, `$PAR`,
  `$TOT`, `$MODE`, `$DATATYPE`, `$BYTEORD`, segment offsets, `$NEXTDATA`).
  Dataset keywords that merely start with `$P` (`$PROJ`, `$PLATEID`) are fine.
- Per-parameter extras (`$PnV`, `$PnG`, ...) go through
  `FcsWriteParameter::keywords`; `$PnN`, `$PnS`, `$PnB`, `$PnE`, `$PnR` and
  `$PnDATATYPE` are generated. `$PnR` comes from `range` when set, otherwise
  from the column maximum.
- `$BEGINDATA`/`$ENDDATA` are zero-padded to 20 digits in TEXT and the HEADER
  DATA fields are 0, so DATA segments of any size are written. Only the TEXT
  segment must end within the 8-digit HEADER field (`99_999_999` bytes).

## Dependencies

`log`, `thiserror`, `serde` / `serde_json` for the `CYTIQ_*` JSON keywords,
and `sha2` for acquisition identity.

## Tests

```bash
cargo test -p cytiq-fcs
```

`tests/reader.rs` reads the fixtures in `data/fixtures/fcs/` and synthetic
files built in the test, and round-trips the writer. Spillover and inversion
unit tests live in `src/compensation.rs`.

## Optional input resolution

Enable the `inputs` feature for `cytiq_fcs::inputs::resolve_inputs`. It accepts
explicit files, nonrecursive directories and filename-only `*`/`?` patterns,
rejects duplicate canonical paths, and preserves original path spellings.
`ResolveOptions` selects global or per-input-group ordering and whether explicit
paths must be regular files. Directory-entry read failures are reported.
