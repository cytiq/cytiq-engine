# Analytics product: `cytiq dataset export`

`cytiq dataset export` turns a [dataset](dataset-contract.md) into a
directory a SQL engine or R can query: path-free Parquet tables, DuckDB
views and a `product.json` that lists them. It summarizes what the dataset
already holds — [label tables](event-labels.md) and population
observations — and computes nothing new about cells. Inferential
statistics are left to diffcyt and CATALYST, which the views feed.

```bash
cytiq dataset export --manifest dataset.json --output product
```

`--root` names the directory object keys resolve against (the manifest's
by default). An existing product is refused without `--overwrite`. The
product is published atomically, with `provenance.json` in it.

## What is verified

- The manifest must validate (document rules, structural profile).
- The objects the product reads are verified as `cytiq dataset validate
  --verify-objects` does: label tables through the label verifier,
  observation tables through the bounded reader, annotation descriptors by
  digest. Events tables are not read: views refer to them. Validate the
  dataset with `--verify-objects` to check them too.
- Observations of an acquisition without a dataset sample, or of a
  specimen the dataset does not list, are refused.
- **Parity.** When an observation's native output (its
  `lineage.annotations_sha256`) has a label table, its assessed event count
  must equal the table's. When the dataset also lists the observation's
  `event-annotations.json`, every count, denominator and fraction must equal
  the label counts through the descriptor's bindings, the fraction bit for
  bit. Any disagreement publishes nothing (exit 2). `product.json` and the
  `parity` table record what was checked (`assessed_events` or `counts`).

Annotation descriptors record absolute paths, so a dataset lists them only
under a disclosure with `provenance_records`. The product never copies a
path, nor the plan's own acquisition labels: an observation's sample is
the dataset sample of its acquisition.

## Tables

All in `tables/`, flat (Utf8, UInt64, Float64, Boolean), read back through
the bounded reader before publication.

| Table | One row per |
| --- | --- |
| `subjects` | design subject |
| `specimens` | specimen, with its subject |
| `samples` | sample: panel, acquisition, specimen, subject |
| `factors` | design factor value: level, entity, factor, value |
| `acquisitions`, `panels`, `channels` | manifest entry (channels with detector, marker, reporter) |
| `layers` | signal layer: acquisition, panel, state, recipe, object key |
| `labels` | label table: annotation and definition ids, kind, acquisition, object key, producer, eligibility, metaclusters |
| `label_columns`, `label_classes` | label column (gates with their parent), classifier class |
| `label_states` | (label table, state) with its exact count |
| `label_counts` | (label table, `label_column`, `label_value`, state) with its exact count |
| `observations` | observation, flattened: target, measure, unit, status, reason, `value_u64`, `value_f64`, `n_events`, denominator, method, lineage digests, technical QC status |
| `sources`, `bindings` | listed annotation descriptor; label → population binding |
| `parity` | label table checked against observations |

Counts are `UBIGINT`, taken from the verified tables, never through floating
point.

## Views

`views.sql` (`cytiq.analytics.views.v1`) uses relative paths: product tables
relative to the product directory, events and label tables to the dataset
root.

```sql
SET file_search_path = 'product,dataset';
.read product/views.sql
```

| View | What it gives |
| --- | --- |
| one per table | `SELECT * FROM <table>` |
| `sample_design`, `specimen_design` | each sample (specimen) with every declared factor as a column |
| `events_<panel>_<state>` | the signal layers of a panel and state, stacked |
| `labels_<kind>` | the label tables of a kind, stacked by column name |
| `gate_membership` | every gate decision as a row, nulls kept |
| `sample_label_states`, `sample_label_counts` | exact counts per sample |
| `sample_label_fractions` | per sample, zero-filled: gate members of assessed events, classes of classified and abstained events (abstentions stay in the denominator), metaclusters of classified events; definitions shared across acquisitions only |
| `population_measures` | observations pivoted: observed count, estimated count and fraction with their statuses |
| `cohort_population`, `cohort_labels` | per-subject statistics by subject and specimen factors and panel |
| `diffcyt_experiment_info`, `diffcyt_counts`, `diffcyt_marker_info`, `diffcyt_medians_<panel>` | inputs of diffcyt and CATALYST |

### Biological replicates

The unit of a cohort statistic is the **subject**, from the dataset's
`design`. Millions of cells of one subject are one observation, and so are
several samples of one subject in one cell.

- A cell is a combination of the subject- and specimen-level factors (group,
  timepoint), the panel, and the source and target or label.
- Each subject contributes one value: its single usable observation
  (measured or estimated; copies of one observation in a package and in a
  specimen result count once) or sample fraction.
- `n_subjects` counts subjects with exactly one value. A subject with more
  than one (a retake, technical replicates) is counted in
  `n_subjects_ambiguous` and left out, never averaged; select one with
  `cytiq specimen plan` and use the specimen result. Subjects without a
  usable value are counted as `n_subjects_qc_rejected` or
  `n_subjects_not_measured`.
- `mean`, `sd`, `min`, `q25`, `median`, `q75` and `max` are over those
  subjects. `n_events` is never an n.

Cohort views are written only when every sample's specimen has a subject;
otherwise `product.json` says which sample has none.

### diffcyt and CATALYST

Clusters are comparable across samples only under one FlowSOM model applied
to all of them (`cytiq flowsom train`, then `apply`): the diffcyt views use
label tables with a `definition_id`, and leave out FlowSOM trained per
acquisition.

```sql
COPY (SELECT * FROM diffcyt_counts) TO 'counts.parquet';
COPY (SELECT * FROM diffcyt_experiment_info) TO 'experiment_info.parquet';
COPY (SELECT * FROM "diffcyt_medians_P1") TO 'medians.parquet';
```

```r
library(arrow); library(SummarizedExperiment); library(diffcyt)
counts <- read_parquet("counts.parquet")
info <- as.data.frame(read_parquet("experiment_info.parquet"))
wide <- tidyr::pivot_wider(counts, id_cols = cluster_id,
                           names_from = sample_id, values_from = n_cells)
d_counts <- SummarizedExperiment(
  assays = list(counts = as.matrix(wide[, info$sample_id])),
  rowData = data.frame(cluster_id = factor(wide$cluster_id)),
  colData = info)
design <- createDesignMatrix(info, cols_design = "group")
contrast <- createContrast(c(0, 1))
res <- testDA_edgeR(d_counts, design, contrast)
```

`diffcyt_marker_info` lists the marked channels with a null `marker_class`:
set it to `type` or `state` for `testDS_*`. Medians are taken in `DOUBLE`
over the panel's transformed (or normalized) layer.

## `product.json`

`cytiq.analytics.product.v1`, canonical JSON: the dataset id, each table's
object key, row count, size and SHA-256, the views' contract, digest and
names, the parity entries and whether the cohort views are present. The same
dataset gives the same product, byte for byte.

## Checks

- `crates/cytiq-population/tests/analytics.rs`: parity, refusals, cohort
  availability, determinism, no path or plan label in the tables.
- `crates/cytiq-engine/tests/dataset_export_cli.rs`: the reference dataset's
  product, and `population observe` counts matching the label tables of the
  same membership exactly.
- `parity/lakehouse/labels.py` (`just lakehouse-interop`): every view in
  DuckDB against PyArrow and CSV oracles; see
  [`parity/lakehouse`](../parity/lakehouse/README.md).
