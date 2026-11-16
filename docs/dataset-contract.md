# Dataset contract: `cytiq.events.v1` and dataset manifest v1

cytiq describes an analytical dataset with two versioned contracts. Both are
implemented in [`cytiq-population`](../crates/cytiq-population/README.md),
and their machine-readable form lives in `crates/cytiq-population/contracts/dataset/v1/`.

- **`cytiq.events.v1`** has one Parquet table per acquisition and signal layer.
  A large layer may be split into part tables listed by a
  `cytiq.events.parts.v1` manifest.
- **Dataset manifest v1** is one canonical JSON document. It lists the
  acquisitions, panels and artifacts, with their identities, checksums and
  scientific metadata.

Scope: these are lakehouse-ready files, not a lakehouse. Iceberg tables,
object storage and catalogs are out of scope.

## Identity of a cell event

An event is identified by **(`acquisition_id`, `source_event_id`)**.
- `acquisition_id` is the SHA-256 of the raw FCS file (`CYTIQ_ACQUISITION`).
  Every cytiq stage carries it into the files it derives.
- `source_event_id` is the event's 0-based row in that raw file. A cleaned
  file records it in `Original_ID`.

A row number alone means nothing: two acquisitions both have an event 0, and
they are different events. Every events table therefore stores both key
columns in every row, and masks and annotations bind to an acquisition
through `cytiq.acquisition_sha256`.

The value of a channel is further identified by its **signal layer**:
`layer_id` is the digest of the layer descriptor (below). The descriptor
covers:
- the acquisition;
- every processing step with its method, parameters and definition digest;
- the channel order.

## `cytiq.events.v1` tables

| Column | Type | Content |
| --- | --- | --- |
| `acquisition_id` | Utf8, not null | 64 lowercase hex characters |
| `layer_id` | Utf8, not null | Digest of the layer descriptor |
| `source_event_id` | UInt64, not null | Raw row, strictly increasing |
| `$PnN` per channel | UInt64, Float32 or Float64, not null | Channel values in `$Pn` order, as the FCS stores them, bit-exact |

Rules for channel columns:
- `Original_ID` is never a channel.
- Channel names may not collide ignoring case.
- Channel names may not be `acquisition_id`, `layer_id`, `source_event_id`,
  `event_id`, `keep` or `original_id`, and may not start with `cytiq.` or `__`.
- A table holds at most 1021 channels.

Footer key-value metadata:

| Key | Value |
| --- | --- |
| `cytiq.events.schema_version` | `1` |
| `cytiq.acquisition_sha256` | The acquisition |
| `cytiq.event_id_base` | `0` |
| `cytiq.events.layer` | Canonical JSON of the layer descriptor (authoritative) |
| `cytiq.events.layer_id` | Its digest |
| `cytiq.events.channels` | JSON channel dictionary of the stored channels (`events-channels.schema.json`) |
| `cytiq.events.column_set` | `all` (every channel of the layer) or `subset` (a projection) |
| `cytiq.events.source_sha256` | SHA-256 of the FCS file the table was exported from |
| `cytiq.events.row_set` | `all` (the raw rows in order) or `subset` |
| `cytiq.events.raw_events` | The raw count, with `row_set=all` |
| `cytiq.events.values_sha256` | Digest of keys and values, independent of encoding |

For a raw input, `source_sha256` equals the acquisition. For a derived file it
is the digest of that file, so the table names both its raw acquisition and the
exact file it came from. The scientific provenance of the values is the layer
descriptor: steps, methods, parameters, definition digests and origins. The
execution provenance (tool version, inputs, settings) is the v3 record written
beside the table. The table does not record the cytiq version, so the same
input and settings give the same bytes across releases that keep the encoding.

Per-channel Arrow field metadata (`cytiq.channel.*`) repeats the channel's
FCS keywords:

| Field metadata | FCS keyword |
| --- | --- |
| `index` | `$Pn` index |
| `name` | `$PnN` |
| `label` | `$PnS` |
| `analyte` | `$PnANALYTE` |
| `reporter` | `$PnTAG` |
| `detector` | `$PnDET` |
| `feature` | `$PnFEATURE` |
| `kind` | `$PnTYPE` |
| `excitation` | `$PnL` |
| `filter` | `$PnF` |
| `voltage` | `$PnV` |
| `gain` | `$PnG` |
| `range` | `$PnR` |
| `amplification` | `$PnE` |
| `bits` | `$PnB` |

It also records the channel's `value_type` and the `steps` applied to it.
Detector, marker and reporter stay separate fields.

Engines that rewrite files without Arrow drop field metadata, and SQL engines
do not read it. `cytiq.events.channels` therefore holds the same fields per
stored channel as JSON (`index`, `name`, `value_type`, `kind`, `steps` and the
keyword fields above). The verifier requires the dictionary, the field metadata
when present, and the descriptor to agree. The footer descriptor remains the
source of truth for the layer.

### Projections

`--channels`/`--channel` store a subset of the layer's channels, always in
`$Pn` order and always with the three keys:
- the descriptor still describes the whole layer, so `layer_id` is unchanged;
- `column_set` is `subset`;
- `values_sha256` covers only the stored columns.

A projection is a standalone export. A dataset manifest refuses it as a signal
layer (`panel_mismatch`), because a signal layer stores every channel.

### Precision

Channel values are stored exactly as the FCS stores them:
- `$DATATYPE I` as `UInt64`, whatever `$PnB`;
- `F` as `Float32`;
- `D` as `Float64`.

Bit patterns are kept, including NaN payloads, signed zeros, infinities and
subnormals. No value is converted, widened or narrowed, and export has no
option that would do so. Every exported table is read back through the bounded
reader before publication, and its values digest must equal the one computed
from the decoded FCS events. An export therefore never publishes a table whose
values differ from the FCS, or one the verifier would refuse.

All raw events are kept. A file whose rows are a subset of the acquisition
(it has `Original_ID`, as QC-filtered and normalized files do) is refused
unless `--allow-subset` is given. Express filtering as masks over the full
layer instead. A file that records cytiq processing without
`CYTIQ_ACQUISITION` is refused, since its raw acquisition is unknown. A file of
no events has no DATA segment; `cytiq-fcs` refuses it, so export does too.

### Layers and signal states

Raw, compensated or unmixed, transformed and normalized values are separate
layers, and each layer is a separate table. A layer's descriptor lists its
steps in pipeline order:

| Step | Method | Origin | Definition |
| --- | --- | --- | --- |
| `compensation` | `spillover` | `recorded` when cytiq used the file's own `$SPILLOVER` | Digest of that matrix |
| `compensation` | `spillover` | `verified` / `declared` for a matrix file | Digest of the matrix file (never its path) |
| `unmixing` | `autospectral` | `verified` / `declared` | Digest of the reference |
| `transform` | `arcsinh`, `logicle`, `hyperlog`, `biexponential` | `recorded` | Parameters in the descriptor |
| `normalization` | `cytonorm` | `recorded` | Model digest; batch and panel digest as parameters |
| any | `unknown` | `external` | Processing by other software (`--base-state`) |

`verified` means the digest matches the provenance record of the run that
produced the input; a mismatch is refused.

Export refuses:
- compensation or unmixing applied twice, or both together;
- normalization without a transform;
- steps over channels the file lacks;
- a compensation matrix or unmixing reference that the file records only by
  path, unless its digest is supplied.

`recipe_id` is the layer without its acquisition, origins, per-file
compensation or unmixing definitions and normalization batch. Layers that
may be stacked in one lakehouse table share it. Store one table per
panel × layer state × `recipe_id`, and union files by column name.

Technical masks (`cytiq.mask.v2`) and biological annotations (gate
membership, classifier predictions) keep their own formats. They join an
events table on (`acquisition_id`, `source_event_id`).

## Export

```bash
cytiq export events --input sample.fcs --output events.parquet --layer raw
```

`--layer raw|compensated|unmixed|transformed|normalized` is required. It names
the layer the input holds, read from its signal keywords and `--base-state`, and
a file holding another layer is refused. A processed layer is exported from the
FCS file the cytiq stage wrote, with these definitions:
- `--compensation-matrix` or `--unmixing-reference`, when the file records
  them only by path;
- verified against the producing run's provenance record when the input has one.

The input's own record is an input of the export's record.

| Option | Default | Effect |
| --- | --- | --- |
| `--channels`, `--channel` | every channel | Projection (above) |
| `--batch-size` | as many events as fit in 16 MiB, at most 65,536 | Events decoded per batch; a batch may not exceed 256 MiB |
| `--row-group-size` | 131,072 | Most events per row group; a row group is also cut at 64 MiB encoded |
| `--compression`, `--compression-level` | `zstd`, level 1 | `zstd` (levels 1–22) or `snappy` |
| `--max-file-bytes` | none | Split into parts of at most this many uncompressed value bytes (`512MiB`); `--output` is then a directory |
| `--allow-subset` | off | Accept an input whose rows are a subset |

None of these changes a value: the values digest of a layer is the same
whatever the batch size, row groups, codec or parts.

**Before writing**, export refuses settings whose tables the bounded verifier
could not read. The limits per table are 2^30 rows, 8 GiB decoded (keys included),
16 MiB of footer and 2^20 pages. The refusal names
`--max-file-bytes` or `--row-group-size`.

**Memory** is bounded whatever the event count:
- one batch of decoded events and keys (16 MiB by default);
- the row group being encoded (at most 64 MiB);
- one batch read back by the verifier;
- the codec state of each column (about 1.5 MiB per channel with zstd level 1,
  less with Snappy; see [Physical layout](#physical-layout)).

The input is hashed before the export (`source_sha256`) and again after it. It
is refused if its TEXT or its bytes changed in between.

**Determinism**: the same input, settings and writer give the same bytes. Rows
are in raw order (`source_event_id` strictly increasing) and channels in `$Pn`
order. Row group boundaries depend on the batch size, so different batch
sizes give the same values in different bytes.

### Parts

With `--max-file-bytes`, `--output` is a directory published atomically. Each
part holds `floor(max / row bytes)` events, where a row is 8 bytes of
`source_event_id` plus each stored channel's width. Part boundaries therefore
do not depend on the codec. The directory holds:

| File | Content |
| --- | --- |
| `part-00000.parquet` … | `cytiq.events.v1` tables of consecutive rows, `row_set=subset` (a lone part is the whole layer) |
| `parts.json` | `cytiq.events.parts.v1` (`events-parts.schema.json`), canonical JSON |
| `provenance.json` | The v3 record of the export, listing every part and `parts.json` |

`parts.json` lists, in row order:
- the acquisition, `layer_id`, `row_set`, `raw_events` and `rows`;
- the layer's `values_sha256` over every row of every part. It equals the
  digest of the same layer exported as one table.
- each part's `location` (`part-NNNNN.parquet` by position), `sha256`,
  `bytes`, `rows`, first and last `source_event_id`, and its own values digest.

`events::verify_parts` checks:
- every part's bytes and table;
- one layer across parts;
- strictly increasing ids, contiguous `0..raw_events` for `row_set=all`;
- the totals and the layer digest.

A layer holds at most 10,000 parts.

### Reading the tables

```sql
-- DuckDB: one channel, projected; parts read through a glob.
SELECT avg("CD3 FITC") FROM read_parquet('events/*.parquet');
-- The channel dictionary, without Arrow.
SELECT decode(value)::JSON FROM parquet_kv_metadata('events.parquet')
WHERE decode(key) = 'cytiq.events.channels';
-- Events that pass a QC mask.
SELECT e.* FROM read_parquet('events.parquet') e
JOIN read_parquet('qc.mask.parquet') m USING (source_event_id)
WHERE m.keep;
```

Join a mask on `source_event_id` within one acquisition. When tables of several
acquisitions are stacked, also compare `e.acquisition_id` with the mask's
`cytiq.acquisition_sha256`. With PyArrow,
`pyarrow.parquet.read_table(path, columns=["CD3 FITC"])` reads one channel, and
`pyarrow.dataset.dataset("events/")` reads the parts. `parity/lakehouse` checks
both readers on every value.

### Physical layout

The canonical layout is **wide**: one typed column per channel. The
alternative was a **list** layout, with one fixed-size list column per value
type and a separate channel dictionary table; nothing was widened. Both were
measured before the choice with `performance/events.py` (see
[`performance/README.md`](../performance/README.md#events-export)). The
decision rule was fixed before the first run.

Environment:
- Apple M2 Pro (12 cores, 16 GB), macOS;
- release build;
- DuckDB 2.0.0 and PyArrow 26.0.0;
- each export the median of three runs, each query the median of five.

10M events × 30 float32 channels (1.2 GB of FCS DATA):

| Layout, codec, row group | File | Export | Peak RSS | DuckDB, one channel | DuckDB, every channel | PyArrow, one column |
| --- | --- | --- | --- | --- | --- | --- |
| wide, zstd 1, 128k | 1083 MB | 17.4 s | 126 MiB | 12 ms | 313 ms | 12 ms |
| wide, zstd 1, 256k | 1083 MB | 28.9 s* | 148 MiB | 27 ms* | 430 ms* | 93 ms* |
| wide, zstd 1, 1M | 1083 MB | 18.0 s | 212 MiB | 17 ms | 347 ms | 33 ms |
| wide, zstd 3, 256k | 1083 MB | 21.5 s | 168 MiB | 15 ms | 320 ms | 29 ms |
| wide, Snappy, 256k | 1241 MB | 16.3 s | 135 MiB | 8 ms | 172 ms | 12 ms |
| list, zstd 1, 256k | 1091 MB | 9.1 s† | 145 MiB | 864 ms | 1161 ms | 2602 ms |

\* An outlier run; the same variant took 20.0 s on 10M mixed-type events.
† The list writer does not hash, verify or record provenance; a wide export does all three.

The table predates hashing the input a second time at the end of the export.
That second read of the 1.2 GB input adds about 0.8 s to a 10M-event export on
the same machine.

Findings:
- **Layout.** In SQL, wide was 10–70× faster for one channel and 2–4× faster
  for a scan. In PyArrow it was 4–340× faster for one column. The list
  layout was never smaller, in any workload: 10M mixed types, 1M × 200
  channels, a 22 MB Aurora spectral control and an 8.8 MB conventional file.
  Wide types each column as the FCS does, so no value needs widening, and the
  bounded reader reads it without nested types. The list layout would need
  nested types in that reader, which reads untrusted masks too.
- **Compression.** Cytometry floats are nearly incompressible: zstd stores
  86–91% of the DATA bytes, and Snappy 98–110%.
  zstd levels 3 and 6 gained under 1%, except 8% at level 6 on the spectral
  control. Higher levels cost per-column memory (below), so the default is
  level 1. Snappy scans fastest in DuckDB but stores about 15% more.
- **Row groups.** DuckDB parallelizes across row groups. 128k events per row
  group was within 10% of the fastest projection and scan in every workload,
  with the lowest memory, so it is the default. A single 1M-event row group
  scanned 2.8× slower at 1M events.
- **Memory** does not grow with the event count (the heap test checks
  1.2M against 300k events). It grows with the channel count, because
  parquet keeps a zstd context per column. At 1M events × 200 channels the
  peak RSS was 375 MiB at level 1, 512 MiB at level 3, 556 MiB at level 6 and
  252 MiB with Snappy, against 126 MiB for 30 channels. Expect about
  1.5 MiB per channel at level 1, on top of the batch and row group.
- **Throughput.** About 0.6M events (70 MB of DATA) per second for 30
  channels. The export is single-threaded and includes:
  - hashing the input;
  - decoding the FCS;
  - hashing the values;
  - zstd;
  - the read-back verification;
  - the provenance hashes;
  - an fsync before publication.

## Dataset manifest v1

Header: `schema_version: 1` and `artifact_type: cytiq_dataset_manifest`.

Sections:

| Section | Holds |
| --- | --- |
| `contracts` | Contracts the artifacts use |
| `privacy` | The disclosure profile (below) |
| `study`, `instruments`, `specimens` | MIFlowCyt metadata (from run manifest v3) |
| `panels` | Cytometry, base signal state, instrument, and ordered `channels`, each with `index`, `name` (`$PnN`), `kind`, `detector`, `marker`, `reporter` and `reagent` |
| `samples` | Pseudonymous id, panel, specimen, acquisition |
| `acquisitions` | `id` (SHA-256), panel, raw event count, role and exported keywords |
| `controls` | Control acquisitions with role and reporter |
| `knowledge` | cytiq-tree artifacts by canonical digest |
| `artifacts` | Each artifact's role, contract, `semantic_id`, acquisition, panel, `parents`, layer summary, `content` (`sha256`, `bytes`, `values_sha256`), `location` (relative object key) and optional `execution` |

Artifact roles: `signal_layer`, `technical_mask`, `annotation`,
`observations`, `model`, `gating`, `knowledge` and `provenance`.

A signal layer is a `cytiq.events.v1` table, or a `cytiq.events.parts.v1`
manifest whose `location` is the `parts.json` of its parts directory. Either
way its semantic id and values digest are the same. The dataset id covers each
artifact's contract, so it differs between the two.

Supported contracts:
- `cytiq.events.v1`
- `cytiq.events.parts.v1`
- `cytiq.mask.v2`
- `cytiq.classify.predictions.v2` … `v5`
- `cytiq.population.annotations.v1`/`v2`
- `cytiq.population.observations.v1`
- `gating-ml.v2`
- `cytiq.classify.bundle`
- `cytiq-tree.v2`
- `cytiq.provenance.v3`

### Three identities

1. **Semantic**: what an artifact is. It is path-free and stable when a
   dataset moves.
   - Signal layer: `layer_id`.
   - Knowledge: the canonical cytiq-tree digest.
   - Other artifacts: the digest of contract, role, acquisition, parents and
     content digest (`identity::semantic_id`).
2. **Content**: `sha256` and `bytes` of the stored object, for integrity.
   It changes with the encoding, for example after a Parquet upgrade.
3. **Execution**: a reference to the v3 provenance record and its
   `execution_fingerprint`. These include paths, so they are referenced,
   never recomputed. `cytiq-provenance` is unchanged.

`dataset_id` is the canonical digest of the manifest without `dataset_id`
and without every `content`, `location`, `execution` and acquisition
`source`. The canonical form is sorted keys, two-space indent, ASCII,
Python float repr and a trailing newline (`serialization::canonical`).
`contracts/dataset/v1/identity-vectors.json` lets other implementations
check their digests.

## Validation

`cytiq dataset validate --manifest M --output R [--verify-objects] [--profile structural|miflowcyt-minimal] [--root DIR]`
writes a report (`validation-report.schema.json`):
- **Exit 0:** no error.
- **Exit 3:** errors; the report is written first.
- **Exit 4:** the manifest is missing, or the report exists without
  `--overwrite`.

Every issue has a code, a severity and a JSON pointer.

| Code | Tier | Examples |
| --- | --- | --- |
| `unsupported_schema` | document, objects | Unknown manifest, contract or table version; contract not allowed for the role |
| `malformed_document` | document | Not JSON, duplicate keys, unknown fields |
| `missing_metadata` | document | Profile fields; a step without its definition digest; `external`/`declared` origins (warning) |
| `panel_mismatch` | document, objects | Unknown detector; reporter on two channels; sample and acquisition on different panels; table channels not the panel's; a projection as a signal layer; marker equal to its reporter (warning) |
| `incompatible_signal_state` | document, objects | Steps out of order or repeated; compensation and unmixing; normalization without transform; base state contradicting the panel; a step the cytometry forbids; different recipes in one panel × state; a parent that is not a step prefix |
| `duplicate_event_identity` | document, objects | A layer listed twice; non-increasing or out-of-range `source_event_id`; parts out of order, overlapping or with a gap; an event twice in a mask |
| `duplicate_acquisition` | document | An acquisition listed twice or bound to two samples |
| `duplicate_identifier` | document | Repeated sample, panel, specimen, instrument, detector or location; a SHA-256-shaped sample id (warning) |
| `invalid_reference` | document, objects | Unknown panel, specimen, acquisition or parent; a parent or table of another acquisition; a mask not covering every raw event; a provenance record not listing the object |
| `inconsistent_channel_order` | document, objects | Indices out of `$Pn` order; repeated or reserved names; table columns in another order |
| `missing_object` | objects | Absent object or part |
| `corrupt_artifact_hash` | objects | Size, digest, values digest or layer id mismatch; unreadable table or parts manifest; a part of another layer; channel dictionary or column set inconsistent with the table |
| `privacy_violation` | document | Undisclosed keyword; absolute path, URI, user-info; source FCS or provenance record without a disclosure |

The `miflowcyt-minimal` profile also requires:
- the study purpose;
- each panel's instrument manufacturer and model;
- the detector and reporter of every marked channel;
- the organism and material of every sample's specimen.

## Privacy

FCS TEXT identifies people and sites: file names, operators, dates, sample
numbers, comments and vendor keywords. A dataset exports only:
- the acquisition keywords `$CYT $MODE $DATATYPE $BYTEORD $PAR $TOT
  $TIMESTEP $VOL $FLOWRATE $ABRT $LOST $ORIGINALITY $UNICODE`;
- the `$Pn` keywords `N S B E R G V L O P F T D DATATYPE DET TAG ANALYTE
  FEATURE TYPE CALIBRATION`;
- `CYTIQ_ACQUISITION`.

Everything else is denied unless a disclosure lists it. That includes `$FIL`,
`$SMNO`, `$OP`, `$SRC`, `$INST`, `$COM`, `$PROJ`, `$EXP`, `$DATE`,
`$BTIM`, `$ETIM`, `$LAST_MODIFIER`, `$CYTSN`, `$PLATEID`, `$WELLID`,
`$CELLS`, `$SYS` and every `@` or vendor keyword.

A disclosure has this shape:

```json
"privacy": {"profile": "controlled", "disclosures": [
  {"contract": "DUA-2026-014", "purpose": "multi-site QC", "keywords": ["$CYTSN"],
   "source_locations": false, "provenance_records": false}]}
```

The following are never exported verbatim, not even under a disclosure:
- spillover matrices and `CYTIQ_*` signal keywords, which can contain paths;
  they enter only as digests and descriptors;
- free FCS TEXT in events tables.

Sample and specimen ids must be study pseudonyms. Locations are relative
object keys, so no path, host, user name or credential is ever recorded.

## Evolution

Each contract has an integer version, and readers reject versions they do
not know.
- A released version's files never change. `contracts/dataset/v1.lock.json`
  pins them.
- A new version adds a contract directory, a row in `dataset::migrate`, a
  fixture and migration tests.
- Run manifest v3 (`cytiq-manifest`) follows the same rule: v1 and v2 keep
  loading and serializing unchanged.

## Reference fixtures

`data/fixtures/dataset` holds a three-panel reference dataset, built from
existing FCS fixtures with the real commands. It has 20 malformed variants,
one or more per issue code. See its
[README](../data/fixtures/dataset/README.md).
