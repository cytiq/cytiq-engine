# Classifier decision policy contract v1

`cytiq classify --decision-policy policy.json` applies a separately evaluated
selective classifier to an unchanged bundle v1–v4. Without this argument,
probabilities, labels, summary and Parquet schemas retain their legacy behavior.
The library entry point is `ClassifyRunner::run_with_policy(config, policy_path)`.
Rust result/source structs gain optional decision fields; consumers constructing
`ClassifyRunResult`, `SourceDefinition` or `EventAnnotations` with struct literals
must initialize those fields to `None` for legacy behavior. This source-level
addition exposes abstention/OOD explicitly; legacy serialized bundle/prediction
formats and `ClassifyConfig` are unchanged.

No production policy or new operating threshold is supplied for the shipped
models. Training, calibration, OOD model fitting, cohort adequacy and independent
validation belong to **cytiq-ml**. The engine checks the declared evidence,
identities, disjoint groups and acceptance limits; hashes alone do not establish
the biological adequacy of labels or cohorts.

## Policy artifact

The bounded JSON/YAML mapping rejects unknown fields. Its fields are:

| Field | Contract |
| --- | --- |
| `schema_version`, `artifact_type` | `1`, `classifier_decision_policy` |
| `bundle_manifest_sha256` | Exact manifest file byte hash, including YAML formatting |
| `model_sha256` | Primary ONNX hash from that manifest |
| `profile_sha256` | Canonical digest of the complete feature profile |
| `classes` | Exact ordered bundle class IDs |
| `normalization` | Exact bundle contract: null for v1/v2, explicit none/CytoNorm for v3/v4 |
| `min_confidence` | Nullable finite float32 in [0,1], independently evaluated cutoff |
| `ood` | Null or `{model: {path, sha256}, threshold: <finite float32>}` |
| `validation` | `{path, sha256}` of the validation report |

At least one rejection criterion is required. There are no default thresholds.
The policy conflicts with a nonzero legacy `--abstain-margin`. Relative artifact
paths resolve beside the policy. The policy, validation and OOD model are all
snapshotted in CLI provenance and checked before inference.

Confidence is `max(probabilities)`; uncertainty is the bounded proxy
`1 - confidence`, not an epistemic variance estimate or an OOD probability.
Confidence below the cutoff causes abstention; equality is accepted. Binary
labels still use the saved bundle threshold, and multiclass ties still select
the first vocabulary index. Probabilities are never recalibrated inside the
engine: when calibration is fitted, cytiq-ml must export that probability
transformation in the primary ONNX graph and issue a new model/bundle identity.
For legacy v1, the negative probability is the public float32 complement
`1 - p_positive`; the policy and evaluation use that same vector, avoiding
cutoff inconsistencies with a slightly different native negative probability.

The optional OOD model consumes exactly the same float32 `features` tensor
`[N,F]` as the primary model and returns **one** float32 tensor `ood_score`
`[N,1]`. Higher scores mean more OOD; scores at or above the validated threshold
cause abstention regardless of classifier confidence. Scores need not be
probabilities. Missing/wrong-shaped/non-finite scores fail the run atomically.
The scorer uses the portable CPU provider with the configured thread limit.
No OOD model means OOD is **not assessed**, including for very confident events.

## Validation evidence

`validation` pins the report bytes. The report is a strict mapping containing:

| Field | Contract |
| --- | --- |
| `schema_version`, `artifact_type` | `1`, `classifier_decision_validation` |
| `policy_sha256` | Canonical policy digest **excluding only `validation`**, avoiding a hash cycle |
| `evaluation_sha256`, `evaluator_sha256` | SHA-256 of the evaluated probability/label table and evaluator source |
| `groups` | Lists for `train`, `calibration`, `selection`, `test`, `ood_test` |
| `known_events`, `ood_events` | Independent test event counts |
| `metrics` | `brier`, `ece`, `risk`, `coverage`, nullable `ood_false_accept_rate` |
| `limits` | `max_brier`, `max_ece`, `max_risk`, `min_coverage`, nullable `max_ood_false_accept_rate` |

Group IDs are lowercase SHA-256 identifiers derived from the subject/group
identity, not per-event random splits. They are unique and disjoint across every
role; train/calibration/selection/test must be nonempty. OOD policies additionally
require independent OOD groups/events and a measured false-accept rate with its
acceptance bound. All metrics and bounds must be finite and satisfy their
declared limits. Zero coverage cannot establish a measured selective risk.
Limits, cutoffs and scorer choice are frozen before evaluating the test cohort.
Do not turn a test sweep into threshold selection or reuse a donor across roles.

Canonical digests use the existing v4 JSON/YAML knowledge serialization contract
(`cytiq_population::serialization::digest`, cytiq-ml `canonical_digest`). Thresholds
must be rounded to their runtime float32 values before evaluating/exporting.

## Predictions and downstream release

When a policy is present, Parquet metadata uses
`cytiq.classify.schema_version=5`, separately records the original
`cytiq.classify.bundle_schema_version`, and pins `decision_policy_sha256`.
Existing identity/probability/label columns retain their order and meanings.
Additional columns are:

| Column | Meaning |
| --- | --- |
| `confidence`, `uncertainty` | Nullable float32 maximum probability and its complement |
| `ood_score`, `is_ood` | Nullable scalar score and threshold decision; null when not assessed |
| `decision_status` | Non-null `not_assessed`, `classified` or `abstained` |
| `abstain_reason` | Null, `low_confidence`, `ood` or `low_confidence_and_ood` |

Excluded events keep their original rows with `assessed=false`, null numerical
predictions/labels and `not_assessed`. Abstained events have `assessed=true`,
retain probabilities, and have null labels. Thus inference coverage and accepted
class assignment remain distinct. `summary.json.decision` records policy identity,
bundle version, abstentions, OOD events and whether OOD was assessed.

`unknown` from the legacy binary band is an abstention, not a biological class.
The trained `other` residual remains an ordinary vocabulary class with its
declared scope/exclusions; abstentions are never added to it. Vocabulary contract
v2 and its `unknown_policy: disabled` remain unchanged.

Specimen classifier sources pin an optional `decision_policy: {path, sha256}`.
Policy predictions produce annotation descriptor schema 2 with separate
`abstained_events`/`ood_events`. Bundle v1 remains standalone-only, as before.
Any abstention, including legacy binary `unknown`, makes affected full population
totals unavailable: `not_measured`, null usable values, supporting accepted counts,
and reason `classifier_abstained`. All inferred events stay in the denominator.
Specimen output is incomplete (exit 4); failed technical QC takes its existing
`checks_failed` path (exit 3). Diagnostic artifacts remain published.

## Evaluation harness and cytiq-ml work

`parity/classify_safety.py` consumes an independently prepared Parquet table with
`split`, `group_id`, `probabilities: fixed_size_list<float32>[K]`, nullable
`label_index`, `is_ood`, nullable float32 `ood_score`, and optional boolean `rare`.
OOD rows use `ood_test` and null known-class labels. The table carries native or
engine probabilities for the **pinned** model; labels come from independent
annotations. The harness does not train, calibrate, join labels by row position,
or choose thresholds. It caps events/groups, bounds probability batches and
rejects oversized decoded row groups.

```sh
parity/.env/py/bin/python parity/classify_safety.py \
  --events independent-evaluation.parquet --policy candidate.json \
  --bundle-manifest bundle/manifest.json --limits frozen-limits.json \
  --output target/classifier-evaluation
```

Outputs are `validation.json` and diagnostic `evaluation.json`. Binary Brier is
mean `(p_positive-y)^2`; multiclass Brier is the mean sum over all class residual
squares, without dividing by class count. ECE uses 15 equal-width bins, positive
probability for binary and top-class confidence for multiclass. Selective risk
uses the actual bundle decision rule. Risk–coverage groups all equal scores
together after the fixed OOD gate; its artifact samples at most 200 complete-tie
points. Coverage denominators include all known test events. Group and rare
population metrics are preserved; no accepted events gives null risk, never zero.

Required cytiq-ml changes, **not implemented in this engine checkout**:

1. Extend boosting evaluation with the same Brier/ECE/risk–coverage definitions,
   independent subject/domain splits and rare-population reports. Record hashes
   of the prepared evaluation inputs and evaluator.
2. Fit calibration using calibration groups, and choose cutoffs/OOD models using
   selection groups. Freeze limits and require independent known/OOD test results.
   Evaluate new instruments, acquisition batches and populations outside the task
   scope as explicit OOD cohorts; softmax confidence is not their ground truth.
3. Export optional scalar OOD ONNX and any primary probability calibration, then
   write the policy and evidence references with the identities above. Preserve
   bundle v1–v4; the policy is a separate artifact, not a new vocabulary class.
4. Export native golden probabilities, scalar OOD scores and exact decisions at
   float32 boundaries, across batches and thread counts, for engine parity.

The small ONNX assets under `data/fixtures/classify_safety` and synthetic test
reports exercise software contracts only; they establish no production threshold.
