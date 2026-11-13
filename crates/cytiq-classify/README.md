# cytiq-classify

Panel-specific supervised classification using a Cytiq model bundle v1–v4 and CPU
ONNX Runtime. `ClassifyRunner::run(&ClassifyConfig)` reads an FCS, checks marker
labels and instrument, applies the declared preprocessing, and writes
`predictions.parquet` plus `summary.json`. The CLI adds `provenance.json`.

The first profile is FACSCanto II P1, reproducing expert Blast annotations in
BLAST110. The Blast phenotype does not imply a leukemic/LAIP-positive cell.

```sh
cytiq classify --input sample.fcs --model bundle/ --output results/
```

An explicit `--decision-policy policy.json` enables a separately validated
selective classifier. It never supplies default rejection thresholds or changes
bundle v1–v4. `ClassifyRunner::run_with_policy` is the corresponding Rust API;
`run` retains legacy behavior. See the [decision policy contract](../../docs/classifier-decision-policy.md)
for evidence, OOD scorer, output schema 5 and the required cytiq-ml changes.

The bundle contains `model.onnx` and a manifest declaring instrument,
channels/marker aliases, preprocessing, class order, checksum and threshold.
The runner validates the panel and model hash, linearises raw FCS signals,
compensates from the spillover matrix and applies fluorescence arcsinh(x/150).
Compatible Cytiq inputs skip steps already recorded; incompatible transforms
are errors.

The optional `--mask` flags use cytiq-mask acquisition identity. Every input
event remains in the output; excluded events have null probabilities and labels.
`--abstain-margin 0.05` labels events within 0.05 of the saved threshold as
`unknown`. It is disabled by default and is not an out-of-distribution detector.

Raw conventional FCS and compatible Cytiq compensated/arcsinh FCS are supported.
Third-party processed FCS without verifiable Cytiq signal state are outside the
contract. Files without Cytiq state are interpreted as raw, as declared by the
dataset profile. A model is only supported for its declared instrument and panel.

`predictions.parquet` holds 0-based `event_id` and `source_event_id`, `assessed`,
nullable `p_non_blast`, `p_blast` and `label`. Its metadata records acquisition
identity and schema version. Labels default to blast/non_blast; the abstain
band can add unknown. `summary.json` reports counts and Blast fraction among
assessed events; `provenance.json` records model, input and mask hashes.
Existing outputs require `--overwrite`. `--batch-size` defaults to 65536;
`--threads` caps ONNX CPU threads.

The input is read `--batch-size` events at a time (`cytiq_fcs::FcsReader::stream`),
so memory holds one batch of events and features plus the masks, not the file.
The panel, instrument, signal state and normalization are checked from TEXT
before the model loads; each batch's lineage, mask decisions, features and
predictions are computed on that batch alone, with the same results as for the
whole file, so batch boundaries and outputs do not depend on how the file is
read. Masked events are still featurized: a nonfinite feature anywhere fails the
run. `predictions.parquet` is written in place of a staged file and published
with `summary.json` only when every batch succeeds; a failure publishes nothing
and leaves no new output directory. When an input has several defects, the
first one met while reading is reported.

ONNX Runtime 1.31.0 is linked statically by `ort` 2.0.0-rc.15 using its
`download-binaries` feature. The crate handles downloading, SHA-256 verification
and caching the prebuilt library during Cargo builds; no project download script
is needed. The first build needs HTTPS access to the upstream binary CDN.

The built executable runs without Python, runtime environment variables or a
separate ONNX Runtime installation. Moving `cytiq` alone is sufficient on the
supported macOS Apple Silicon and Linux x86_64/aarch64 targets; standard OS
libraries remain required. Test using `cargo test -p cytiq-classify` and
`cargo test -p cytiq-engine --test classify_cli`.

## Vocabulary-aware bundle v2

The same `cytiq classify` command also reads v2 bundles from `cytiq-ml`. Each bundle
adds `classes.json`, `panel.json` and `label-mapping.json`, pinned by SHA-256. The
runner checks vocabulary indices, task and decision rule, hierarchy references,
residual definitions, panel agreement and dense float32 `[N,K]` probabilities.
The CLI records all knowledge files in provenance. No Python runtime is needed.

Multiclass uses vocabulary order and argmax; ties choose the lowest index. Its
unknown policy is `disabled`. The learned `other` residual class has a declared
population scope and exclusions. `--abstain-margin` is only supported for binary
tasks. A binary phenotype v2 task retains the saved threshold and abstain band.
Existing v1 bundle outputs retain their original columns and summary.

V2 Parquet has `event_id`, `source_event_id`, `assessed`, one nullable `p_<class_id>`
column per ordered class, then `label`. Metadata pins class order and vocabulary
hash. Summary reports `class_counts`, `class_fractions`, task and vocabulary hash.
Excluded events retain their rows with null predictions. Multiclass summaries
omit the binary-specific Blast fields.

The `cytiq-flowcyt-stored-v1` profile reads the stored measurements in original
FlowCyt author exports. It applies no compensation, linearisation or arcsinh and
rejects inputs already carrying Cytiq processing state. The instrument, detector
names and reagent labels must match the pinned profile. This profile is scoped to
the published nonmalignant bone marrow cohort; it does not assert that the same
lineage definitions hold for AML/P1 or unrelated processed FCS files.

Conventional flow profiles need `$CYT` to equal the profile's instrument, with
one exception: Kaluza re-exports write `$CYT=Kaluza` and keep the acquiring
cytometer in `$OriginalInstrument`, which must then equal the instrument up to
case (the FlowCyt exports write `Navios` for the profile's `NAVIOS`). Files
without instrument keywords, such as the archive's flowCore re-exports, are
rejected. `tests/flowcyt.rs` runs this on real author files from
`data/fixtures/flowcyt`.

The `cytiq-cytof-arcsinh-v1` profile reads mass cytometry ion counts as stored.
CyTOF software writes vendor and version into `$CYT`, so this profile names an
instrument family (such as `CyTOF`) that `$CYT` must contain, case-insensitively.
Every profile channel must be linear (`$PnE` 0) with unit gain. It applies no
compensation, rejects a declared `$SPILL`/`$SPILLOVER` and Cytiq processing state,
and applies `asinh(x / cofactor)` (cofactor 5 for CyTOF) to the channels the profile
marks `fluorescence: true`. Other channels, such as `Event_length`, pass through
unchanged. `cytiq-table-stored-v1` marks models trained on published event tables
without acquisition FCS files. Such models are training-only, and `cytiq classify`
rejects them. Fixtures under `data/fixtures/classify/cytof` come from
`cytiq-ml/scripts/generate_classify_fixture.py --cytof`.

`ClassifyRunner` retains one ONNX environment for the process, allowing repeated
or concurrent library calls. Test both v1 and v2 with `cargo test -p cytiq-classify`
and `cargo test -p cytiq-engine --test classify_cli`.

## Normalization-aware bundle v3

V3 inherits the vocabulary, panel, label-mapping and prediction columns of v2.
The manifest must additionally declare one of:

```json
{"normalization": {"kind": "none"}}
```

```json
{"normalization": {"kind": "cytonorm", "model_sha256": "<64 lowercase hex characters>"}}
```

The hash is SHA-256 of the exact CytoNorm `model.json`, whose payload hashes
pin SOM, marker/transform contract and splines. It must equal the FCS
`CYTIQ_NORMALIZATION.model_sha256`. Missing or mismatched normalization is
rejected before inference or output creation. Existing panel, instrument,
compensation and arcsinh recipe checks still apply; normalization does not
enable cross-instrument classifier inference automatically. V3 output metadata
and summary record this contract. Public `feature_columns` retains the legacy
unnormalized contract; `feature_columns_with_normalization` accepts an explicit
`NormalizationContract`.

V1/v2 bundles and the stored FlowCyt recipe reject normalized FCS. They retain
their prior behavior on compatible unnormalized files. A v3 model must have been
trained with its declared normalization; changing a manifest alone cannot make
an old classifier scientifically compatible. The exporter in `cytiq-ml` must be
updated separately, with subject-level splitting before normalizer training.

## JSON/YAML bundle v4

V4 accepts exactly one `.json`, `.yaml` or `.yml` file for each of `manifest`,
`classes`, `panel` and `label-mapping`; mixed extensions are supported. The
knowledge artifacts themselves remain schema v2. V1/v2/v3 retain their fixed
JSON paths and byte-based integrity checks.

V4 knowledge checksums use the exact canonical artifact JSON from `cytiq-tree`:
Python `json.dumps(payload, indent=2, sort_keys=True, ensure_ascii=True,
allow_nan=False) + '\n'`, encoded as UTF-8. Presentation, YAML aliases and field
order do not affect identity. Model/normalizer hashes and provenance checksums
continue to describe exact bytes. V4 requires explicit `normalization` under
the same rules as v3; `kind: none` retains the unnormalized preprocessing path.

YAML `yes/no/on/off` are strings, booleans use `true/false`, and numeric label
keys require quotes. Duplicate/non-string keys, merge keys, unsupported tags,
recursive aliases and non-finite values are rejected. Missing or ambiguous
bundle files and mismatched checksums fail before inference/output creation.
A `model.onnx` or OOD model over `MODEL_BYTES_MAX` (2 GiB − 1, the protobuf
limit without external data) is refused before it is hashed or loaded.
`input_files()` returns the actual selected paths for provenance.

Golden serialization vectors and Python-exported v4 fixtures verify canonical
SHA-256 and identical binary/multiclass predictions for YAML and JSON. The v4
fixtures reuse the v2 ONNX weights during tests rather than duplicating models.
