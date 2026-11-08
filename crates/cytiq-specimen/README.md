# cytiq-specimen

Specimen planning, summary-only reconciliation, consistency checks and publication.
This is a research/shadow-run interface; `reference_ratio` remains experimental.

## Inputs and ownership

The run manifest owns acquisition paths, IDs and panel assignments. Omitted
`schema_version` means v1; v2 permits repeat acquisitions and `cytometry: cytof`.
The specimen protocol owns source bindings, panel requirements and reconciliation.
`cytiq-tree` owns ontology, panel projections and classifier vocabularies.

`SpecimenLayout` v1 accepts JSON/YAML and rejects unknown fields. All file paths
resolve against the layout. Panels reference projections by **canonical** SHA-256;
source `definition` references identify exact Gating-ML or model.onnx **bytes**.
Every panel must use the same tree/context. Population counts are not inferred
from hierarchy links.

```yaml
schema_version: 1
protocol_id: immune-research-v1
panels:
  P1:
    required: true
    projection: {path: P1.projection.json, sha256: "<canonical projection digest>"}
    sources:
      gates:
        format: gate_membership
        definition: {path: gates.xml, sha256: "<file digest>"}
        bindings:
          - label: All
            target: {kind: population, id: leukocytes, scope: leukocytes}
          - label: T
            target: {kind: population, id: t_cells, scope: leukocytes}
            denominator: All
reconciliation: []
consistency: []
```

Classifier sources use `format: classify_predictions`, a model definition,
`bundle_manifest: {path: manifest.yaml, sha256: "<file digest>"}`, an explicit
`abstain_margin` (defaults to 0), and
`vocabulary: {path: classes.yaml, sha256: "<canonical vocabulary digest>"}`.
Bindings come from that vocabulary. The supplied projection must match all of its
knowledge identities; placeholder tree/projection digests are rejected.

Classifier sources may additionally pin `decision_policy: {path: policy.json,
sha256: "<file digest>"}`; it conflicts with a nonzero legacy `abstain_margin`.
See the [decision policy contract](../../docs/classifier-decision-policy.md).
Abstention blocks full population totals and yields an incomplete specimen,
while probabilities and supporting counts remain diagnostic artifacts.

Release requires each population package's hash-verified `release-qc.json`
snapshot. Older packages without it remain inspectable but cannot authorize
release; regenerate them through `population observe`. Specimen reconciliation
continues to operate on summaries without reading native event Parquet or FCS.

`SpecimenPlan` v1 snapshots candidates and the effective layout. With a single
acquisition a panel selects it automatically; repeats require `--select`.
Non-selected acquisitions retain `not_selected` reasons. No `qc_best` heuristic
or replicate averaging runs implicitly.

## Running a specimen

First freeze definitions and selection; `--output` always names a package directory:

```sh
cytiq specimen validate --manifest run.yaml --layout layout.yaml --specimen S01 --select P1=A02
cytiq specimen plan --manifest run.yaml --layout layout.yaml --specimen S01 --select P1=A02 --output plan/
```

Use existing per-acquisition commands with the same final combined mask for
technical QC and annotations. Conventional flow supports gate/classify; spectral
flow supports gate; CyTOF supports the existing classify recipe. New debarcoding,
EQ-bead normalization and spectral ML recipes are outside this version.

```sh
cytiq qc --manifest run.yaml --sample A02 --mask analysis.parquet --output qc.json
cytiq gate --input a02.fcs --gating-ml gates.xml --mask analysis.parquet \
  --membership membership.parquet --output counts.json --format json
cytiq population observe --plan plan/plan.json --acquisition A02 --source gates \
  --annotations membership.parquet --producer-provenance counts.json.provenance.json \
  --mask analysis.parquet --qc-report qc.json --qc-provenance qc.json.provenance.json \
  --output observations/A02-gates/
cytiq specimen run --plan plan/plan.json --observations observations/A02-gates/ \
  --output results/S01/
```

For ML use `predictions.parquet` and the classifier's `provenance.json` as the
annotation inputs. Repeat `--observations` for every selected panel/source.
`population validate` takes the observe input flags, omits `--output`, and runs
the same validation without publishing a package.

## Reconciliation and QC

Without rules all observations remain independent. Rules are explicit per target:

```yaml
reconciliation:
  - target: {kind: population, id: t_cells, scope: leukocytes}
    strategy:
      kind: reference_ratio
      source: {panel: P2, source: gates}
      reference: {panel: P1, source: gates}
      anchor: {kind: population, id: leukocytes, scope: leukocytes}
      anchor_limits: {min_events: 400, max_scale: 4, max_frequency_ratio: 1.5}
consistency:
  - target: {kind: population, id: t_cells, scope: leukocytes}
    max_abs_fraction_difference: 0.05
    severity: warn
```

`independent` retains all source estimates. `prefer_reference` names a
`reference: {panel, source}` and selects its count/fraction without fallback.
`reference_ratio` computes source count × reference anchor / source anchor.
Both anchors must be positive and measured. Compatibility is enforced when the
layout and plan are validated: different panels of one specimen plan, the same
tree and context, an anchor scope equal to the target's, and the anchor bound
in both sources. It emits a separate experimental `estimated_count` (method
version 2); all originals remain in `observations`. Its method references the
three contributing observation identities.

`anchor_limits` is required and has no defaults: derive each limit from the
assay's validation data, not from these examples.

| Limit | Check | Effect |
| --- | --- | --- |
| `min_events` (≥ 1) | `reference_ratio_anchor_events`: each anchor's count, with its Poisson CV `1/√n` | Fail; the estimate becomes `qc_rejected` with reason `anchor_below_min_events` |
| `max_scale` (optional, ≥ 1) | `reference_ratio_scale`: `s = reference anchor / source anchor` must satisfy `max(s, 1/s) ≤ max_scale` | Fail; reason `anchor_scale_out_of_bounds` |
| `max_frequency_ratio` (optional, ≥ 1) | `reference_ratio_anchor_frequency`: the anchor's frequency (count / assessed events) in the two panels, larger over smaller | Warn only |

A count equal to `min_events` and a value equal to either bound pass. The
scale and frequency checks are reported only when both anchors are measured
and positive. The `reference_ratio_anchor` check also states the estimate's
Poisson CV, `√(1/population + 1/source anchor + 1/reference anchor)`, which
ignores population-recognition error. A rejection affects only the derived
estimate: measured counts and fractions are never changed.

Technical failures make observations `qc_rejected`, with supporting counts.
Consistency policies compare fractions only when target/denominator scopes agree;
they never delete samples or rewrite biological measurements. Missing QC policies
remain warnings from the upstream technical report. Missing sources and panels
produce explicit null observations; absent optional panels do not make a specimen
incomplete. Clinical criteria, absolute concentration and LoD/LoQ are not evaluated.

## Packages and orchestration contract

Plans contain `plan.json`; observations contain `event-annotations.json` and
`observations.parquet`; specimen results contain `result.json` and
`results.parquet`. Every package also contains RunRecord v3 `provenance.json` (v2 remains readable) and
completion record v1 `outcome.json`. Readers verify every outcome digest before
using data; the presence of a result file alone does not prove completion.
`SpecimenResult` v1 includes source observations, selected/derived results and
separate technical/completeness/consistency checks.
`SpecimenResult::load` verifies package digests, row identities, reconciliation
references, outcome/QC agreement and logical equality of JSON and Parquet results.

Publication stages and syncs a complete directory before replacement.
`--overwrite` replaces the entire prior package; the default refuses existing
outputs. `--reuse skip` reuses a matching verified package; `--reuse reject` refuses
stale or damaged packages with exit 4. Interrupted directory publication is recovered
under an output lock before assessment. Implicit retries are not performed.
Only `specimen plan` reads raw FCS metadata; `specimen run` reads verified plans
and population packages, so raw files and native annotations can be offline.

After argument parsing the new command families emit one JSON outcome on stdout:

| Code | Meaning |
| --- | --- |
| 0 | Completed successfully; warnings remain visible |
| 1 | Runtime/internal error |
| 2 | Invalid configuration or incompatible/corrupt data |
| 3 | Completed and published, checks failed |
| 4 | Existing output conflict, missing required input, or published incomplete specimen |

`completed` distinguishes published partial/check-failed results from execution
errors. Argument syntax errors use clap's usual code 2 and need not have JSON.
Every other `cytiq` command uses the same codes and writes the same JSON with
`--outcome <PATH>` (see [cytiq-engine](../cytiq-engine/README.md#exit-codes)).
This contract is usable by the API/compute facade and Prefect without either
becoming a runtime dependency.

## Comparing native results

```sh
cytiq specimen compare --baseline previous/S01/ --candidate current/S01/ --output comparison/S01/
cytiq specimen compare --baseline previous/S01/ --candidate current/S01/ \
  --policy comparison-policy.yaml --output comparison/S01/ --overwrite
```

Both inputs are verified `specimen run` packages for the same specimen. The
command reads summaries only, compares `observations` and `results` independently,
and publishes `comparison.json`, provenance and outcome. Embedded snapshots make
the comparison readable after the original packages are moved or removed.
`SpecimenComparison::load` verifies the package and recomputes its calculations.

Rows match by collection, panel, source, full target and measure, never content
identity or acquisition ID. Duplicate semantic keys are invalid. A changed tree,
unit or denominator target/scope is incompatible; changed status, added/removed
rows, QC status multisets and specimen completeness/QC are checks. Model, mask,
acquisition, method, protocol and projection changes remain visible as context.
Denominator counts may change without changing its definition. Measured zero
stays numeric; equal unavailable states have no numeric delta.

Default limits are exactly zero. Policy v1 accepts JSON/YAML with absolute limits:

```yaml
schema_version: 1
defaults:
  observed_count: 0       # exact UInt64 event difference
  estimated_count: 0.0
  fraction: 0.001         # units fraction, not percent
  concentration: 0.0
overrides:
  - key:
      collection: results
      panel_id: P1
      source_id: gates
      target: {kind: population, id: t_cells, scope: cells}
      measure: observed_count
    limit: {kind: count, value: 2}
```

Other measures use `limit: {kind: float, value: 0.001}`. Limits are inclusive,
finite and nonnegative; duplicate overrides and keys absent from both inputs are
errors. Counts and their deltas never pass through floating point. Checks compare
status multisets, preserving repetitions; message changes are descriptive.

Exit 0 means all comparisons passed; 3 publishes a comparison with failed checks;
4 publishes an unavailable comparison when there are no numeric pairs and no
failures. Invalid inputs/policy use 2, missing inputs/output conflicts use 4 and
runtime errors use 1. An unchanged failing source QC can compare equal; its source
status remains present and a successful comparison is not a source QC approval.

## Reports and exports

```sh
cytiq specimen report --input results/S01/ --output reports/S01/
cytiq specimen report --input comparison/S01/ --output reports/S01-comparison/
cytiq specimen report --input results/S01/ --format csv --output exports/S01/
```

`--format all` (default) publishes offline `report.html`, versioned `report.json`
and CSV tables. Select `html`, `json` or `csv` to publish only that representation.
Every representation is a package with provenance and outcome; `--output` is a
directory, and existing directories require `--overwrite`.

For a specimen, CSV consists of `summary.csv`, `observations.csv`, `results.csv`
and `checks.csv`. A comparison exports `summary.csv`, `rows.csv`, `checks.csv` and
`context.csv`. Observation columns retain separate `value_u64`/`value_f64`, status,
unit, complete target, denominator, method and lineage. Structured columns contain
compact JSON. Nulls are empty cells, counts are exact decimal integers, fractions
remain 0–1, and floating values are not rounded before export. Comparison rows
include both sides, the applied tolerance, verdict and typed exact/float deltas.

The HTML is self-contained and script-free, with summary tables and expandable
metadata. Text is escaped and integers never pass through JavaScript Number.
Only the supplied package is read: source data, models and plans can be offline;
reports of comparisons also work after the baseline/candidate packages are gone.
Information absent from a result, such as excluded retake reasons, is not inferred.

Report generation returns exit 0 on a successful export, including exports of
incomplete specimens or failed comparisons. Source QC/completeness and comparison
verdicts remain prominent in the report and JSON; they are not export errors.
Corrupt/unsupported input uses exit 2, missing input or output conflicts use 4,
and runtime failures use 1. Publication failure preserves the prior report.

Rust entry points are `compare`, `ComparisonPolicy`, `SpecimenComparison::load`,
`SpecimenReport::load`, `runner::write_comparison` and `report::write_report` with
`ReportConfig`/`ReportFormat`. Existing specimen/result schemas are unchanged.

## Validation and shadow runs

```sh
cargo test -p cytiq-population -p cytiq-specimen
cargo test -p cytiq-engine --test specimen_cli
just verify
just docker-build
```

Fixtures establish software behavior, not clinical validity. For historical shadow
runs, freeze the layout, models, input hashes and legacy version; compare measured
counts separately from derived estimates, missing/rejected states, eligibility,
fractions and QC. Record differences and acceptance bounds per panel/instrument
before promoting experimental reconciliation. The known 3000/10000 versus
6400/20000 case must retain both measured counts and emit 3200 only as an estimate.
