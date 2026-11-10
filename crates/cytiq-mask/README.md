# cytiq-mask

Per-event masks shared by cytiq stages, and the `cytiq mask combine` command.

Stages never delete events from the raw file. They write their per-event
results as a mask: a Parquet file with one row per event of the file they ran
on. `source_event_id` ties each row to the raw acquisition, so a mask applies
to any file derived from it, whatever rows that file kept.

## Schema

| Column | Type | Meaning |
| --- | --- | --- |
| `event_id` | `UInt64` | 0-based row in the file the mask was computed on |
| `source_event_id` | `UInt64` | 0-based row in the raw file |
| filter columns | nullable `Boolean` | pass/fail; null means "not assessed" |
| `keep` | `Boolean` | whether later stages should use the event |

Parquet key-value metadata ties the mask to its acquisition:

| Key | Value |
| --- | --- |
| `cytiq.mask.schema_version` | `2` (`SCHEMA_VERSION`) |
| `cytiq.acquisition_sha256` | SHA-256 of the raw FCS (`cytiq_fcs::acquisition_id`) |
| `cytiq.event_id_base` | `0` |

Written by `cytiq compensate`, `cytiq peacoqc`, `cytiq clean`,
`cytiq autospectral` and `cytiq gate --membership`; read by `--mask` on
`classify`, `gate`, `flowsom`, `clean` and `plot`.

Every stage that writes FCS records the raw acquisition hash in
`CYTIQ_ACQUISITION`. A file without that keyword is raw, and its own SHA-256
is its acquisition identity.

See the [pipeline guide](../cytiq-engine/PIPELINE.md#conventional-chain) for a
complete chain that combines margin, PeacoQC and cleaning masks.

## Usage

```bash
cytiq mask combine --mask a.masks.parquet --mask b.masks.parquet --output analysis.parquet
```

From Rust:

```rust
use cytiq_mask::EventMask;

let raw = EventMask::read_parquet("sample.comp.masks.parquet".as_ref())?;
let qc = EventMask::read_parquet("sample.peacoqc.masks.parquet".as_ref())?;
let analysis = EventMask::combine(&[raw, qc])?;

let path = std::path::Path::new("sample.peacoqc.fcs");
let dataset = cytiq_fcs::FcsReader::open(path)?.read()?;
let acquisition = cytiq_fcs::acquisition_id(path, &dataset.text)?;
let keep: Vec<bool> = analysis.keep_for(&dataset, &acquisition)?; // one entry per dataset row
```

## Public API

| Item | Purpose |
| --- | --- |
| `EventMask` | In-memory mask: `new`, `with_filter`, `read_parquet`, `write_parquet`, `combine`, `keep_for`, `keep_by_source`, `kept` |
| `SourceKeep` | `keep` for events by raw-file row, from `keep_by_source(acquisition)`, for inputs read in batches |
| `read_combined` | Read and combine the masks passed with `--mask` (`None` for none) |
| `MaskCombineRunner::run` / `MaskCombineConfig` | The `cytiq mask combine` command |
| `EVENT_ID_COLUMN`, `SOURCE_EVENT_ID_COLUMN`, `KEEP_COLUMN` | Reserved column names |
| `SCHEMA_VERSION`, `SCHEMA_VERSION_KEY`, `ACQUISITION_KEY`, `EVENT_ID_BASE_KEY` | Parquet metadata |
| `MaskError` | Error type for every failure mode |

## Rules

- Every mask belongs to one acquisition. `combine` rejects masks of different
  acquisitions, and `keep_for` rejects a dataset of another acquisition.
- Reading rejects a mask without acquisition metadata (written before schema
  version 2): regenerate it with the current cytiq.
- `write_parquet` publishes through `cytiq-publish` (a synced temporary file
  beside the target, renamed into place). `stage_parquet` stages the mask in a
  caller's `Publication`, so it is published together with the outputs it
  describes.
- `combine` keys rows by `source_event_id` and follows the first mask's rows.
  An event is kept only when every mask has it and keeps it. Filter columns
  are concatenated; a repeated name is prefixed with the 1-based index of its
  mask (`2.margin_pass`). Recombining prefixed masks adds another prefix when
  needed to keep every column unique (`2.2.margin_pass`); nulls remain null.
- `keep_for` matches dataset rows to mask rows through the dataset's
  `Original_ID` (or row number when it has none). A dataset row missing from
  the mask is an error. `keep_by_source` checks the acquisition and builds the
  lookup once; its `SourceKeep::keep` answers for any batch of raw-file rows,
  and `keep_for` is that lookup over the whole dataset.
- Reading rejects masks without `keep`/`source_event_id`, with nulls in them,
  with non-Boolean filter columns, or with a repeated `source_event_id`.

## Bounded Parquet input

`parquet::ScalarParquetReader` is shared by masks, population observations and
CytoNorm spline tables. It accepts flat Boolean/UInt64/Float64/Utf8 columns and
preserves Arrow metadata. Defaults cap files at 8 GiB, metadata at 16 MiB,
compressed and decoded pages at 64 MiB, cumulative decoded allocations at 8 GiB,
pages at 2^20, rows at 2^30 and columns at 1,024. Dictionary storage and Arrow
arrays share one allocation budget; repeated dictionary strings are checked
before materialization, with exact-capacity string buffers. A bounded compact
Thrift preflight checks the flat schema before upstream metadata parsing, so a
forged child count cannot trigger an allocation or recursion in that parser.
Population observations apply their existing tighter row budget; callers can
override the defaults with `table::read_from_with_limits`. Library callers can
supply `ParquetReadLimits` for the configurable byte/row/page budgets.
Nested schemas and over-budget inputs return errors. Page sizes and dictionary
ordering/payloads are validated before the upstream decoder is called, including
regressions for malformed packed definition levels, truncated byte dictionaries
dictionary-encoded data without a dictionary, and an excessive schema child
count. The observation fuzz target uses smaller limits: 4 MiB input, 1 MiB
metadata, 8 MiB pages, 64 MiB decoded allocations, 100,000 rows and 10,000 pages.

Compression features remain disabled in the workspace. Even if another crate
enables them through Cargo feature unification, GZIP, Brotli and legacy LZ4 are
rejected before decompression: their upstream decoders do not bound output by
the declared page size. Only codecs with bounded output buffers may be enabled
(Snappy, Zstd and LZ4_RAW). An independently generated GZIP fixture with a false
page size permanently tests this fail-closed boundary.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for event lineage (`source_event_ids`,
  `acquisition_id`)
- `arrow` / `parquet`

## Tests

```bash
cargo test -p cytiq-mask
cargo test -p cytiq-engine --test mask_cli
```
