# Event labels: `cytiq.events.labels.v1`

A label table holds what one cytiq stage decided about each event of one
acquisition: a technical QC mask, gate membership, FlowSOM clusters or
classifier predictions. `cytiq export labels` converts the stage's native
output. Nothing is recomputed: the table holds the native decisions, bound
to the raw events, the run that made them and its definition.

Label tables are part of the [dataset contract](dataset-contract.md) and
are implemented in
[`cytiq-population::labels`](../crates/cytiq-population/README.md#label-tables-cytiqeventslabelsv1).
The [analytics product](analytics.md) counts and queries them.

## Rows and keys

One table per acquisition and label source, with **one row per raw event,
in raw order**: `source_event_id` is exactly `0..raw_events`. An event the
producer did not label still has a row, with a state that says why.

| Column | Type | Content |
| --- | --- | --- |
| `acquisition_id` | Utf8, not null | SHA-256 of the raw FCS file |
| `annotation_id` | Utf8, not null | Identity of the labels (below) |
| `source_event_id` | UInt64, not null | Raw row |
| `state` | Utf8, not null | What happened to the event (below) |
| label columns | nullable | The kind's columns, then its labels |

The keys are those of [`cytiq.events.v1`](dataset-contract.md#identity-of-a-cell-event)
tables, so a label table joins an events table of any layer of the same
acquisition on (`acquisition_id`, `source_event_id`). Tables of several
acquisitions and sources stack without losing identity. String columns are
Parquet-dictionary encoded; nothing nested, no Arrow dictionary type.

## Kinds and states

| Kind | Native output | `state` | Columns |
| --- | --- | --- | --- |
| `qc` | `cytiq.mask.v2` of `clean`, `peacoqc`, `compensate`, `autospectral`, `mask combine` | `accepted`, `qc_rejected`, `not_assessed` | one Boolean per filter |
| `gate` | `cytiq gate --membership` | `assessed`, `qc_rejected`, `not_assessed` | one Boolean per gate, in evaluation order |
| `cluster` | `cytiq flowsom` / `flowsom apply` CSV | `classified`, `qc_rejected`, `not_assessed` | `metacluster`, `node` (UInt64, from 1) |
| `classifier` | `predictions.parquet`, schema 2–5 | `classified`, `abstained`, `qc_rejected`, `not_assessed` | `label`, `abstain_reason`, `confidence`, `uncertainty`, `ood_score`, `is_ood`, then `p_<class>` |

Each requested state is recovered without loss from the row `state` and its
cells:

| Semantics | How it reads |
| --- | --- |
| Not assessed | `state = 'not_assessed'`, every label cell null |
| QC rejected | `state = 'qc_rejected'`, every label cell null |
| Assessed and accepted | QC: `state = 'accepted'`; a filter or gate cell `true` |
| Assessed and excluded | a filter or gate cell `false` |
| Classified | `state = 'classified'` with `label`, or `metacluster` and `node` |
| Abstained | `state = 'abstained'`, `label` null, `abstain_reason` set |

- **QC.** `keep` gives `accepted` or `qc_rejected`. Each filter keeps its
  tri-state: `true`, `false`, or null for not assessed. A filter can be null
  while the event is accepted.
- **Gates.** A gate cell is null exactly when the event is not `assessed`.
  Membership is absolute, as `cytiq gate` computes it: a member of a gate is
  a member of its parent. Sibling gates may overlap; nothing is chosen
  between them.
- **Classifier.** A policy (schema 5) abstention keeps its reason
  (`low_confidence`, `ood`, `low_confidence_and_ood`) and `is_ood`. A
  legacy binary `unknown` becomes `abstained` with reason `margin` and a
  null `is_ood`; `unknown` in a multiclass prediction is refused.
  Abstentions keep their probabilities. `confidence`, `uncertainty`,
  `ood_score` and `is_ood` are null before schema 5; nothing is derived.
  `--no-probabilities` leaves out the `p_<class>` columns; the classes stay
  in the descriptor.
- **QC-rejected or not assessed.** An event the producer excluded, or
  omitted (FlowSOM writes no row for masked events, a stage run on a
  filtered file has no row for removed events), is `qc_rejected` when the
  export was given the QC masks the producer read (`--qc-mask`, verified
  below), and `not_assessed` otherwise. An omitted event the masks keep is
  refused.

## Identities

The footer key `cytiq.labels.descriptor` holds the canonical JSON of the
descriptor (`contracts/dataset/v1/labels-descriptor.schema.json`): the
contract, kind, acquisition, raw event count, producer, definition,
eligibility and label columns. It is path-free.

- **`annotation_id`** is the descriptor's canonical digest: *these* labels.
  The producer is its tool, the native contract and digest, and its input,
  recorded by **layer id** when the events table given is the export of
  exactly the file the producer read, and by digest only otherwise.
  Files a cytiq stage writes may record paths, so their digest is no
  path-free identity.
- **`definition_id`** is the digest of the definition and the label columns
  (`cytiq.events.labels.definition.v1`): *how* the labels were decided,
  without the acquisition, input or eligibility. Labels of different
  acquisitions are comparable only when they share it.
  - Gates: the Gating-ML digest.
  - Classifier: the model files and decision policy digests, the classes,
    the vocabulary digest, the schema versions, the normalization and
    `abstain_margin`.
  - FlowSOM `apply`: the model digest, node and metacluster counts and the
    metacluster of each node.
  - QC masks and FlowSOM trained on one acquisition (`cytiq flowsom`) have
    **no** definition: their labels are never pooled across acquisitions.

Neither identity holds the cytiq version. `cytiq.labels.values_sha256` is a
digest of every row, independent of the Parquet encoding (identity vector
`labels_values_sha256`).

## Export

```bash
cytiq export labels --kind gate --input membership.parquet \
  --producer-provenance counts.json.provenance.json \
  --events sample.raw.parquet --qc-mask analysis.parquet \
  --gating-ml gates.xml --output sample.gates.parquet
```

| Option | Kinds | Meaning |
| --- | --- | --- |
| `--input` | all | The native output |
| `--producer-provenance` | all | The run record of the producer |
| `--events` | all | An events table or parts directory of every raw event (`row_set=all`) |
| `--qc-mask` | gate, cluster, classifier | A QC mask the producer read; repeat for each |
| `--gating-ml` | gate | The Gating-ML file `cytiq gate` evaluated |
| `--flowsom-model` | cluster | The model `cytiq flowsom apply` applied |
| `--no-probabilities` | classifier | Leave out `p_<class>` |

Before any row is read, the export refuses:
- a producer record that does not list the native file, or whose tool does
  not write that kind;
- an events table that is not every raw event, does not verify, or is of
  another acquisition than the native file's footer;
- FlowSOM assignments that cannot be bound to the acquisition: the producer
  must have read the events table's file, the raw file, or the QC masks
  given;
- a Gating-ML file or FlowSOM model other than the producer's, and gates
  other than the membership's columns in evaluation order;
- QC masks other than exactly the producer's masks.

While streaming it refuses native rows out of raw order, repeated or beyond
the acquisition, eligibility that disagrees with the QC masks, gates outside
their parent and contradictory classifier rows. The native file is hashed
before and after the export. The table is read back through the bounded
verifier and published only when it verifies, with a provenance record
beside it.

Memory is one batch, the row group and per-event bitmaps of the raw event
count; the frozen heap budget is 24 MiB at 200k events. A table is planned
against the verifier's limits before writing: label tables have no parts, so
an acquisition too large for one table is refused with a message.

## SQL

```sql
SET file_search_path = 'dataset/';
-- Events that passed QC, with their CD3+ gate decision.
SELECT e.*, g."CD3+"
FROM read_parquet('events/M/S12-M.transformed.parquet') e
JOIN read_parquet('labels/M/S12-M.qc.parquet') q USING (acquisition_id, source_event_id)
JOIN read_parquet('labels/M/S12-M.gates.parquet') g USING (acquisition_id, source_event_id)
WHERE q.state = 'accepted' AND g.state = 'assessed';
-- Every gate decision as a row, nulls kept.
SELECT * FROM read_parquet('labels/M/S12-M.gates.parquet')
UNPIVOT INCLUDE NULLS (member FOR gate IN ("cells", "CD45+", "CD34+", "large", "blasts"));
-- The descriptor, without Arrow.
SELECT decode(value)::JSON FROM parquet_kv_metadata('labels/M/S12-M.gates.parquet')
WHERE decode(key) = 'cytiq.labels.descriptor';
```

Join on both keys. Row numbers repeat in every acquisition: two panels of
one specimen both have an event 0, and they are different events.

## In a dataset

A dataset manifest lists a label table as an `annotation` of its
acquisition, contract `cytiq.events.labels.v1`, with its `annotation_id` as
semantic id and its values digest in `content`
(`ArtifactEntry::labels`). `cytiq dataset validate --verify-objects` reads
it through the label verifier and requires its acquisition, raw event count,
semantic id and values digest to be the manifest's.
