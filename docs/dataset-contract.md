# Dataset contract: `cytiq.events.v1` and dataset manifest v1

cytiq describes an analytical dataset with two versioned contracts. Both are
implemented in [`cytiq-population`](../crates/cytiq-population/README.md),
and their machine-readable form lives in `crates/cytiq-population/contracts/dataset/v1/`.

- **`cytiq.events.v1`** has one Parquet table per acquisition and signal layer.
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
| `cytiq.events.row_set` | `all` (the raw rows in order) or `subset` |
| `cytiq.events.raw_events` | The raw count, with `row_set=all` |
| `cytiq.events.values_sha256` | Digest of keys and values, independent of encoding |

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

Engines that rewrite files without Arrow drop field metadata, so the footer
descriptor is the source of truth.

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

Supported contracts:
- `cytiq.events.v1`
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
| `panel_mismatch` | document, objects | Unknown detector; reporter on two channels; sample and acquisition on different panels; table channels not the panel's; marker equal to its reporter (warning) |
| `incompatible_signal_state` | document, objects | Steps out of order or repeated; compensation and unmixing; normalization without transform; base state contradicting the panel; a step the cytometry forbids; different recipes in one panel × state; a parent that is not a step prefix |
| `duplicate_event_identity` | document, objects | A layer listed twice; non-increasing or out-of-range `source_event_id`; an event twice in a mask |
| `duplicate_acquisition` | document | An acquisition listed twice or bound to two samples |
| `duplicate_identifier` | document | Repeated sample, panel, specimen, instrument, detector or location; a SHA-256-shaped sample id (warning) |
| `invalid_reference` | document, objects | Unknown panel, specimen, acquisition or parent; a parent or table of another acquisition; a mask not covering every raw event; a provenance record not listing the object |
| `inconsistent_channel_order` | document, objects | Indices out of `$Pn` order; repeated or reserved names; table columns in another order |
| `missing_object` | objects | Absent object |
| `corrupt_artifact_hash` | objects | Size, digest, values digest or layer id mismatch; unreadable table |
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
