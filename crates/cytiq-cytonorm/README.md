# cytiq-cytonorm

Panel-specific population-based batch normalization. This is an independent Rust
implementation of [CytoNorm](https://doi.org/10.1002/cyto.a.23904) and the controls
and aggregate workflows described in [CytoNorm 2.0](https://doi.org/10.1002/cyto.a.24910).
Training and inference run without R or Python. The GPL R package is a separate
black-box parity oracle, pinned to 2.0.12 / `278f352d8edffd7a5d3e6b0b76c402d991a1e7a9`.

```bash
cytiq cytonorm train --manifest batches.yaml --output model/ --threads 4
cytiq cytonorm apply --input sample.t.fcs --model model/ --batch day2 \
  --output sample.norm.fcs --debug-dir normalization-plots/
```

## Manifest v1

Paths are relative to the YAML/JSON manifest. Unknown fields are errors.

```yaml
schema_version: 1
panel:
  id: P1
  markers:
    - { marker: CD3, channel: FITC-A, labels: [CD3] }
    - { marker: CD4, channel: PE-A, labels: [CD4] }
mode: controls                    # or aggregate
clustering_markers: [CD3]
normalization_markers: [CD3, CD4]
target: { kind: mean }             # or { kind: batch, batch: day1 }
samples:
  - { id: c1, batch: day1, file: controls/day1.t.fcs, masks: [controls/day1.masks.parquet] }
  - { id: c2, batch: day2, file: controls/day2.t.fcs }
  - { id: v1, batch: day1, file: validation/day1.t.fcs, role: validation }
  - { id: v2, batch: day2, file: validation/day2.t.fcs, role: validation }
som: { xdim: 15, ydim: 15, metaclusters: 10, scale: false, seed: 1 }
training_events: 1000000
min_cluster_events: 100
sparse_policy: error              # or explicitly identity
# probabilities defaults to [0.01, 0.02, ..., 0.99]
```

At least two training batches are required. List all panel markers, not just the
clustering subset. `$PnN` must match `channel` exactly, in any column order;
undeclared measurement channels are rejected, while acquisition channels
(`Time`, FSC/SSC, event/cell length and `Original_ID`) may pass through.
`$PnS` must match one of `labels`, or `marker` when labels are omitted.
Selected channels require verified Cytiq compensation/unmixing and a complete
`CYTIQ_TRANSFORM` record. All files must use identical transform methods and
parameters. Files of one batch must agree on `$CYT`, including its absence.
No channel renaming or instrument/panel harmonization is inferred. Acquisition
and lineage channels cannot be normalized.

`controls` files must represent shared biological references across batches;
their selected events supply batch distributions. `aggregate` files must be
biologically comparable samples: a balanced subset supplies each batch proxy.
SOM input has equal batch/file quotas, selected deterministically without
replacement, capped at `training_events`. Unused quotas are not redistributed.
Small files contribute all available events. Aggregate mode also uses these
subsets for distribution fitting; controls mode uses all masked control events.
Actual selection counts are recorded. Cohort channel matrices are currently held
in memory; the SOM cap does not cap controls-mode quantile memory.

Masks are joined by acquisition/source event ID. Validation files are assessed
after fitting and never enter the SOM, scaling, targets or splines. Duplicate
acquisitions, including train/validation overlap, are rejected. For ML validation,
split subjects before constructing this manifest; do not train a normalizer on
test samples. Validation batch names must already occur in training.

## Algorithm and model

The persisted FlowSOM uses consensus metaclustering (100 node resamples at 90%,
Euclidean distance, average linkage inside and after resampling). This does not
change the defaults of the existing `flowsom` command. Rust uses its own
deterministic RNG, so R node IDs and individual stochastic fits need not match.

Per batch/metacluster/marker, type-7 quantiles map onto the equal-weight batch
mean, or a chosen batch. Fritsch–Carlson monotone Hermite splines interpolate
the mappings. Duplicate source knots average their targets; tails continue
linearly at the endpoint derivative. No fixed raw-intensity anchors are added.
Fewer than `min_cluster_events` or two distinct source knots is an error.
Explicit `sparse_policy: identity` leaves unsupported mappings unchanged and
records why. Unreliable batches do not contribute to a mean target; a missing
reference target makes the corresponding mappings identity under that policy.

Slopes match R `stats::splinefun(method="monoH.FC")` over its full monotone
region, including intervals outside the sufficient radius-three circle.
The final derivative is checked analytically on every segment: R's adjacent
corrections can themselves leave a non-monotone curve (real CD19 regression).
Such fits fail under `sparse_policy: error`; explicit `identity` keeps the
original values and records `unsafe spline` as the reason. No non-monotone
coefficients are published. Old stored monotone coefficients load without
refitting. Non-finite queries return NaN; workflows reject non-finite signals.
An unsafe final spline does not remove its otherwise valid quantiles from
the target calculation; the scientific target remains the same.
Older engines with the radius-three validator may reject some new, valid fits;
apply them with the corrected engine. The stored bundle schema is unchanged.

The bundle contains:

| File | Content |
| --- | --- |
| `model.json` | Schema 1, target, batches/instruments, quantile grid, fitted settings, training input hashes, goals and payload hashes |
| `som.json` | FlowSOM codebook, frozen scaling, labels and transform contract |
| `markers.json` | Full panel mapping and transform settings |
| `splines.parquet` | Batch/metacluster/marker coefficients and explicit identity reasons |
| `training-qc.json` | Separate training/validation diagnostics and sampling counts |
| `provenance.json` | Standard CLI run provenance |

The normalization ID is SHA-256 of the exact `model.json` bytes. Its payload
hashes pin `som.json`, `markers.json` and `splines.parquet`; provenance and QC
do not affect that ID. Load verifies these hashes and complete mapping coverage.
Models and outputs require `--overwrite` for replacement. Keep debug reports in
a separate directory from the model bundle.

Manifest/model JSON or YAML documents are limited to 16 MiB before parsing.
Spline Parquet uses the shared bounded flat-scalar reader from `cytiq-mask`.
Balanced training selects source row IDs before converting panel values, so
unsampled rows do not create an intermediate full row matrix. Event selection,
order, random streams, quantiles and spline arithmetic remain unchanged.

Spline Parquet v1 has non-null `batch: utf8`, `metacluster: u64`, `marker: utf8`,
`knot: u64` (zero-based), `x/y/slope: f64`, `events: u64`, and nullable
`identity_reason: utf8`. One row represents each knot. Identity mappings have
one zero-valued sentinel row and a non-null reason. Cluster IDs are one-based.

## Apply, QC and downstream models

Apply only accepts known batches with compatible panels, instruments and
transform settings. It assigns every event using the original values before
changing selected marker columns. It preserves event count/order, untouched
values, acquisition identity and `Original_ID`; downstream masks still join.
Already normalized input is rejected. Output values stay transformed.

`CYTIQ_NORMALIZATION` schema 1 records `model_sha256`, `panel_sha256` (SHA-256
of `markers.json`), `batch` and physical `channels`. Existing signal metadata
is retained. Normalized channels lose their former acquisition margin bounds.
Acquisition QC and margin assessment must run before normalization. Subsequent
`transform`, including `--allow-retransform`, is rejected.

Apply writes `<output>.cytonorm-qc.json` and the standard
`<output>.provenance.json`. QC schema 1 includes occupancy, marker means,
training/validation CV, before/after quantiles, correction size, tail counts and
identity reasons. `emd_quantile_approx_*` is mean absolute distance between
sample and target quantiles on the saved probability grid: an approximation
to 1-D Wasserstein/EMD, not the histogram metric of upstream `emdEvaluation`.
`--debug-dir` adds Plotly spline and before/after distribution diagnostics.
High CV and biological comparability require scientific review; there is no
universal automatic CV cutoff or guarantee against biological overcorrection.

Classifier bundles v1/v2 reject normalized FCS. Bundle v3 must explicitly pin
this normalization ID; the `cytiq-ml` exporter requires a separate update.
New batches need a newly trained bundle in v1; extending a frozen target is
not implemented.

## Public API and checks

`Manifest::load`, `CytoNormRunner::{input_files,train,apply}`, `TrainConfig`,
`ApplyConfig`, `CytoNormModel::{load,validate,input_files}`, and
`MonotoneSpline::{fit,validate,evaluate}` provide the reusable implementation.
The library writes computational artifacts; the CLI adds standard provenance.

```bash
cargo test -p cytiq-cytonorm
cargo test -p cytiq-engine --test cytonorm_cli
just parity-cytonorm
just parity-cytonorm-real
```

The real-data pilot uses six checksum-pinned CyTOF files shipped with the R
package: three batches, donor 1 for training and donor 2 for validation, with
1,000 real events per file. It checks upstream R splines with fixed cluster
assignments, untouched channels, event order, and identical fitted artifacts
when validation is removed and the thread count changes. Independent R
clustering and batch distances are reported without biological pass thresholds.
See [the real-data instructions](../../parity/real/README.md#cytonorm-pilot)
for preprocessing assumptions and output locations.
