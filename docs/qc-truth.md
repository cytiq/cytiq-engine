# QC artifact truth: `cytiq.qc.truth.v1`

An artifact-truth table says, for each raw event of one acquisition, which
technical artifacts it is: debris, a doublet, a dead cell, an event in a
time anomaly, or a saturated event. It is **independent evidence about the
acquisition**, produced outside cytiq, and `cytiq qc --artifact-truth` uses it
to measure how well the QC masks removed artifacts and how many clean events
they lost (`artifact_removal` in `qc.json`, see
[`cytiq-qc`](../crates/cytiq-qc/README.md#artifact-removal)).

Population labels cannot do this. A gate or a published cell type says what
a cell is, not whether an event that a filter removed was debris. Removal
accuracy is therefore measured only against this contract, and never
inferred from population retention.

The table is implemented by `cytiq_qc::truth::ArtifactTruth`. It is not a
[label table](event-labels.md): label tables hold what a cytiq run decided,
and truth must not come from cytiq. **Do not supply labels derived from any
cytiq output**, such as masks, gates, classifier predictions or clusters. A
filter evaluated against its own decisions always looks perfect.

## Rows and columns

One Parquet file per raw acquisition, with **one row per raw event, in raw
order**: `source_event_id` is exactly `0..rows`. An event the annotators did
not label still has a row.

| Column | Type | Content |
| --- | --- | --- |
| `source_event_id` | UInt64, not null | Raw row (0-based), as in [masks](../crates/cytiq-mask/README.md) |
| `debris` | Boolean, nullable | Debris: not a cell |
| `doublet` | Boolean, nullable | Two or more cells in one event |
| `dead` | Boolean, nullable | A non-viable cell |
| `time_anomaly` | Boolean, nullable | Acquired during a flow or signal disturbance |
| `saturated` | Boolean, nullable | A value at the detector's limit |

At least one class column is required. Each class column holds one of three values per event:
- `true`: the event is an artifact of that class.
- `false`: the event was verified **not** to be one.
- null: the class did not label the event, or the annotators found it ambiguous.

Any other column is rejected, including `keep`, `event_id` and pandas'
`__index_level_0__`, so a mask or a membership file cannot pass as truth.
Classes may overlap: an event can be both a doublet and dead.

Every event gets one state:
- **artifact** if any class marks it;
- **clean** if every class in the table verified it free;
- **undetermined** otherwise.

"Clean" means clean **of the labelled classes only**, which the report lists as
`clean_of`. A table that labels only debris says nothing about doublets.

## Footer

| Key | Value |
| --- | --- |
| `cytiq.qc_truth.schema_version` | `1` |
| `cytiq.acquisition_sha256` | SHA-256 of the raw acquisition (`CYTIQ_ACQUISITION`), as in masks |
| `cytiq.qc_truth.descriptor` | The descriptor, JSON, at most 64 KiB |

Every `cytiq.*` key must be one of these three, appear once and have a value.
Other keys (`ARROW:schema`, `pandas`) are ignored. Write tables **uncompressed**:
a standalone `cytiq-qc` build decodes no compression codec.

## Descriptor

The descriptor records who labelled the events, how, and with what
evidence. Unknown fields are rejected, and so is a class key given twice.

```json
{
  "annotation": {
    "producer": "Flow core, two operators",
    "protocol": "SOP-17 rev 3: FSC-A/SSC-A debris gate, adjudicated",
    "reference": "doi:…"
  },
  "classes": {
    "debris": {
      "evidence": "expert_consensus",
      "selection": "random_subset",
      "method": "2,000 random events gated independently by two operators; disagreements adjudicated",
      "channels": ["FSC-A", "SSC-A"],
      "annotators": 2,
      "agreement": { "statistic": "cohen_kappa", "value": 0.91, "events": 2000 }
    }
  }
}
```

`classes` must describe exactly the class columns of the table.

| Field | Values | Rule |
| --- | --- | --- |
| `evidence` | `generative`, `definitional`, `orthogonal_assay`, `expert_consensus`, `single_expert` | |
| `selection` | `all_events`, `random_subset`, `targeted` | which events were labelled |
| `method` | text | not empty; enough to repeat the labelling |
| `channels` | up to 64 names | the channels the labels were drawn on; empty for generated truth |
| `annotators` | count | 1 for `single_expert`, at least 2 for `expert_consensus`, absent otherwise |
| `agreement` | `cohen_kappa`, `fleiss_kappa`, `krippendorff_alpha` in [−1, 1]; `percent_agreement` in [0, 1]; `events` > 0 | expert evidence only |

**Uncertainty.** Each class records its evidence, its selection and the
annotators' agreement. Unlabelled and ambiguous events are null, and every
metric gives bounds over them (below). Version 1 has no per-event confidence
column.

## What a report may claim

Each class gets a **truth class** derived from its declared evidence. The
evidence is declared, not verified, so the report echoes `channels`. Truth
drawn on the channels a filter uses is not independent of that filter, even
from experts.

| Evidence | Truth class | Supports |
| --- | --- | --- |
| `generative` | `synthetic` | algorithm behaviour on simulated data; not biology |
| `definitional` | `definitional` | that a filter implements a definition (such as `$PnR − 1`); not that the definition is right |
| `orthogonal_assay` | `independent` | removal accuracy on real data |
| `expert_consensus` with `agreement` | `expert_reference` | removal accuracy against a measured human reference |
| `expert_consensus` without `agreement`, `single_expert` | `exploratory` | hypotheses only |

With `selection: targeted`, the labelled events do not represent the
acquisition, so point estimates are biased. Only the bounds hold.

## Metrics

`artifact_removal` reports two things:
- **every mask column on its own**, as `filters`;
- **the final `keep`**, as `combined`.

Columns are never merged by filter name. Compensate's physical `margin_pass`
and PeacoQC's legacy-extrema `2.margin_pass` mean different things.

A column's outcome per event is one of three: removed (`false`), passed (`true`) or not assessed (null).
A null keeps the event, so it counts as not removed. The fixed map from column to target class is:

| Column | Target class |
| --- | --- |
| `non_debris` | `debris` |
| `singlets` | `doublet` |
| `live` | `dead` |
| `peacoqc_pass` | `time_anomaly` |
| `margin_pass`, `margin_upper_pass` | `saturated` |
| `margin_lower_pass`, other columns | none |

Combine prefixes such as `2.live` keep their target.

| Metric | Value | Bounds over unknown truth |
| --- | --- | --- |
| `sensitivity` | positives removed / positives | `[r⁺/(n⁺+k?), (r⁺+r?)/(n⁺+r?)]` |
| `specificity` | negatives kept / negatives | `[k⁻/(n⁻+r?), (k⁻+k?)/(n⁻+k?)]` |
| `precision` | positives removed / labelled events removed | `[r⁺/(r⁺+r⁻+r?), (r⁺+r?)/(r⁺+r⁻+r?)]` |
| `clean_loss` | clean removed / clean | as sensitivity, over clean events |
| `removed_artifact_share` | artifacts removed / determined events removed | as precision |
| `artifact_recall` (combined) | artifacts removed / artifacts | as sensitivity |
| `clean_retention` (combined) | clean kept / clean | as specificity |

Notation used in the table:
- `r` counts removed events and `k` kept ones.
- `⁺` marks positives (the class labels `true`), `⁻` negatives (`false`), and `?` unlabelled events, or undetermined ones for event-level metrics.
- `n` is the size of a group.

`assessed_sensitivity`, the positives removed over the positives the column
assessed, is a diagnostic only.

Each bound is sharp for its own metric. The bounds are not joint: one
assignment of the unknown events does not reach every bound at once. A ratio
with a zero denominator is null, never 0 or 1.

Where a target is reported `not_assessed`, its reason is given and the target
carries no number:
- the table does not label the target class;
- no event is labelled for the target class;
- the column assessed no event.

The whole section is `not_assessed`, with its reason, in four cases:
- no truth was supplied;
- the truth belongs to another acquisition;
- the truth has a different number of events from the raw file;
- the mask does not cover every raw event exactly once.

Two further things describe each population with `--populations`:
- Population retention gains `clean_frequency`, the population's frequency
  among clean events.
- Each population also gains the retained frequency's error against
  `clean_frequency`.

These are **descriptive**. `artifact_removal` never adds a check, and it
never changes `status`, the exit code or a release decision. Acceptance
bounds need independent, validated real data and a reviewed `QcPolicy`
change ([scientific validation](../parity/scientific/README.md#qc-artifact-truth-benchmark)).

## Writing a table

`ArtifactTruth::new(acquisition, descriptor, classes)` validates a table and
`write_parquet` writes it. With pyarrow:

```python
table = pa.table({"source_event_id": pa.array(range(n), pa.uint64()),
                  "debris": pa.array(debris, pa.bool_())})
table = table.replace_schema_metadata({
    "cytiq.qc_truth.schema_version": "1",
    "cytiq.acquisition_sha256": acquisition,
    "cytiq.qc_truth.descriptor": json.dumps(descriptor)})
pq.write_table(table, path, compression="none")
```

The acquisition is the raw file's SHA-256, or its `CYTIQ_ACQUISITION`
keyword when it has one. Truth for a subset file (PeacoQC output, an
excerpt) must still have one row per event of the raw acquisition.
