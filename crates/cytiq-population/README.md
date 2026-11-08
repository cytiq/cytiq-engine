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
