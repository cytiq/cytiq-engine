# cytiq-population

Versioned population observations and streaming adapters for native cytiq event
annotations. No model inference, specimen policy or orchestration dependency.

## Public contracts

`EventAnnotations` v1 is a descriptor of an existing gate membership (mask v2) or
classifier predictions (bundle v2–v4) Parquet. It pins acquisition identity,
source definition, data, final combined mask, technical QC and producer records.
Technical QC must pin the exact run manifest frozen by the specimen plan.
It records assessed and unknown event counts. Native files are never rewritten.

`PopulationObservation` v1 distinguishes `observed_count` (`value_u64`) from
`estimated_count`, `fraction` and `concentration` (`value_f64`). Only counts and
fractions are produced by the adapters. Integer event counts never pass through
floating-point serialization. A target is a population, Boolean phenotype or
scoped residual; class IDs and ontology population IDs are not interchangeable.

| Status | Usable value |
| --- | --- |
| `measured` | A measured count, including zero, or a fraction of assessed events |
| `estimated` | A finite derived estimate with its method and source identities |
| `not_measured` | Null; missing acquisition/source or no assessed events |
| `qc_rejected` | Null; original event count may remain in `supporting_count` |
| `below_detection` | Null with an explicit reason; no automatic LoD inference |
| `not_applicable` | Null, including a fraction with a zero denominator |

Fractions use units `fraction` (0–1), not percentages. Every denominator names its
scope and count; a bound gate denominator also names its target. Gate membership
must be inside the declared denominator. Overlapping gates remain separate:
there is no deepest-gate selection or automatic addition of children.

The long Parquet table exposes identity, target, measure, unit, status, exact
UInt64 and Float64 value columns. `target`, `denominator`, `method` and `lineage`
carry structured JSON; `target_id` and `target_kind` are checked against it.
Each row has a content identity and table metadata pins schema version 1.

## Events tables (`cytiq.events.v1`)

`events::export` writes one Parquet table per acquisition and signal layer;
`events::verify` checks one. Every row carries `acquisition_id` (the
SHA-256 acquisition identity), `layer_id` and `source_event_id` (the raw
row, strictly increasing), then one column per FCS channel named `$PnN` in
`$Pn` order, typed as the FCS stores it (`UInt64`, `Float32`, `Float64`)
and bit-exact. `Original_ID` becomes `source_event_id`. Events of two
acquisitions never share an identity, even with equal rows.

The layer descriptor (key-value `cytiq.events.layer`) lists the steps that
produced the values (compensation or unmixing, transform, normalization),
each with its method, parameters, channels, definition digest and origin:
`recorded` (from the file's own keywords), `verified` (a supplied digest
matched against the producing run's provenance), `declared` or `external`
(processing by other software). `layer_id` is its canonical digest;
`recipe_id` drops the acquisition and per-file definitions, so layers that
may share one lakehouse table share it. `cytiq.events.values_sha256` is a
digest of keys and values that does not depend on the Parquet encoding.
Field metadata (`cytiq.channel.*`) keeps the detector (`$PnDET`), marker
(`$PnS`, `$PnANALYTE`) and reporter (`$PnTAG`) apart. The footer key
`cytiq.events.channels` repeats it as a JSON channel dictionary
(`ChannelEntry`) that SQL engines read without Arrow. No other FCS TEXT is
written: file names, operators and dates never reach the table.

`events::export_with` takes `WriteSettings`:
- `batch_rows`;
- `row_group_rows`, with a 64 MiB encoded cap per row group;
- `codec`: `Codec::Zstd(level)` or `Codec::Snappy`;
- `channels`: a projection stored in `$Pn` order;
- `max_file_bytes`: split into parts.

It also takes the `ParquetReadLimits` the result must satisfy; settings
whose tables the bounded reader would refuse are rejected before writing. No
setting changes a value. `ExportOptions::layer` must be the layer the file
holds, and `allow_subset` must be set for a file with `Original_ID`.

The export streams with `FcsReader::stream`. Memory holds one batch, the
encoded row group, one read-back batch, and the codec state of each column.
It reads every table back through the bounded reader and publishes it only
when the values digest matches. It records the input's digest
(`cytiq.events.source_sha256`) and refuses an input whose TEXT or file stamp
changed during the export.

A split layer is a directory of `part-NNNNN.parquet` tables and `parts.json`
(`PartsManifest`, `cytiq.events.parts.v1`), whose layer digest equals the
single-table one. `events::verify_parts` checks it.

`ExportRunner` (`cytiq export events`) publishes the table or the directory
through `cytiq-publish`.

## Dataset manifest v1

`dataset::DatasetManifest` describes an analytical dataset in one canonical
JSON document (`schema_version: 1`, `artifact_type: cytiq_dataset_manifest`):
- **Metadata:** the privacy contract, the study, instruments and detectors,
  and specimens, all after MIFlowCyt.
- **Panels:** each panel's ordered channels, with detector, marker, reporter
  and reagent kept as separate fields.
- **Samples, acquisitions and controls.** An acquisition is identified by the
  SHA-256 of its raw FCS file and exports only allowlisted keywords.
- **Knowledge references** by canonical digest.
- **Artifacts:** signal layers, technical masks, annotations, observations,
  definitions and provenance records. Each artifact has a role, a contract, a
  path-free `semantic_id`, its acquisition, parents, content digest and size,
  a relative object key and an optional execution reference.

`dataset_id` is the digest of the manifest without locations, contents and
executions, so moving or re-encoding a dataset keeps its identity.

`dataset::validate` collects typed issues in two tiers.
- **Document rules (no I/O):** unsupported contracts, missing metadata,
  panel mismatches, incompatible signal states (step order, base state,
  cytometry, recipes within a panel × state), duplicate event identities,
  acquisitions and ids, invalid references, inconsistent channel order and
  privacy violations.
- **Object verification (opt-in):** missing objects, size and digest
  mismatches, events tables, masks, predictions, observations and provenance
  records against the manifest.

Profiles: `structural`, and `miflowcyt-minimal`, which also requires the
study purpose, instruments, detectors and reporters of marked channels, and
specimen organism and material.

JSON Schemas of the manifest, the layer descriptor and the validation report
are published in `contracts/dataset/v1/`, pinned by
`contracts/dataset/v1.lock.json`, with identity vectors for other
implementations.

## Knowledge and limits

`knowledge::KnowledgeArtifact` validates the six upstream v2 artifact kinds
against pinned schemas, expands upstream defaults and checks hierarchy,
projection status and internal digests. Schema copies and their source revision
are listed in `contracts/cytiq-tree.lock.json`. Definitions are owned by
`cytiq-tree`; this crate does not generate ontology content. `KnowledgeRef`
uses its canonical identity; `ArtifactRef` uses exact file bytes.

`serialization` is shared with classify and retains Python-compatible canonical
JSON and the legacy bundle digest policies. Documents are bounded to 16 MiB,
64 levels and one million expanded values. Arrow reads project only required
columns in batches of 65,536 rows. Raw IDs use compact bitmaps; at most one
billion raw events and one million summary observations are accepted.

The producer and technical QC must use **one identical final combined mask**.
Coverage is checked by `source_event_id`, including reordered/filtered inputs.
Missing eligible rows, duplicate IDs and disagreement with `keep`/`assessed`
are errors. Classifier fractions retain abstentions in the assessed denominator;
`unknown` is not turned into a biological population. Bundle v1 remains available
to classify but is outside specimen import.

An event's `keep`/`assessed` flag expresses eligibility/inference, not successful
QC or biological assignment. Publication requires an effective pinned panel QC
policy and its mandatory assessments. `release-qc.json` records the assessment
bound to the manifest, acquisition and mask. Required failures retain diagnostic
observations as `qc_rejected`, with supporting counts and null usable values.

Policy-enabled predictions schema 5 carry an explicit decision status and OOD
assessment; the bundle remains v1–v4. Such sources pin `decision_policy` in their
definition. Their annotation descriptor uses schema 2 and records
`abstained_events` and `ood_events` separately from legacy `unknown_events`.
Any classifier abstention, including legacy binary `unknown`, makes full target
totals `not_measured` with reason `classifier_abstained`; accepted counts remain
supporting evidence. Denominators retain all assessed events.

## API and CLI

Use `observe(&ObserveConfig)` for a descriptor and validated observations,
`table::{read,write}` for summary Parquet, and `artifact::{write_package,
verify_package}` for checked publication. The CLI obtains source configuration
from a specimen plan; see [cytiq-specimen](../cytiq-specimen/README.md).

```sh
cargo test -p cytiq-population
```

Package publication holds an OutputLock through recovery and commit.
`artifact::begin_package` accepts a ReusePolicy and returns a verified saved outcome
or a PackageRun for new publication. The CLI exposes this as global `--reuse`;
new packages carry v3 provenance and outcome.execution metadata. Existing v2
producer records remain readable and are never reused as execution caches.
