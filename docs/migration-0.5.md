# Migrating to 0.5

0.5 simplifies the library crates and tightens the command-line output
contract. Scientific results are unchanged unless a section says otherwise.
Each change below names what to do.

## Thread pools belong to the caller (Rust API)

Library runners no longer build their own rayon pools. They run on the pool
of the caller, as rayon recommends for libraries. The `cytiq` binary builds
the process-wide pool once from `--threads`; the flag and its meaning are
unchanged, and results never depend on it (`tests/threads_cli.rs`).

Removed:

- The `threads` field of `AutoSpectralConfig`, `AutospillConfig`,
  `AutospreadConfig`, `CleanConfig`, `CytoScanConfig`, `FlowSomConfig`,
  `GatingConfig`, `PeacoQcConfig`, `SpillQcConfig`, `TransformConfig` and
  CytoNorm's `TrainConfig` and `ApplyConfig`.
- The `threads` parameter of `FlowSomModel::apply_file` and
  `FlowSomModel::apply_model_file`.
- The `ThreadPoolBuild` error variants of those crates, and the
  `InvalidThreads` variants of `CleanError`, `CytoScanError` and
  `GatingError`. The CLI rejects `--threads 0`; library callers should use
  a positive count for a fixed limit (`ThreadPoolBuilder` treats zero as
  automatic sizing).

`cytiq-classify` keeps `ClassifyConfig::threads`: it sets ONNX Runtime's
intra-op threads, not a rayon pool.

To keep a thread limit, run the library call inside a pool:

```rust,ignore
let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
let result = pool.install(|| cytiq_gating::GatingRunner::run(&config))?;
```

`cytoscan_config.json` no longer records `threads`, which described how fast
the run went, not what it computed.

## One channel-list parser (Rust API)

`cytiq_flowsom::parse_channel_list`, `cytiq_peacoqc::parse_channel_list` and
`cytiq_cytoscan::parse_channel_list` were re-exports of
`cytiq_fcs::parse_channel_list`. Import it from `cytiq_fcs`.
`cytiq_transform::parse_channel_list` remains: it returns `TransformError`.

## Existing outputs are refused (command line)

Every command that writes results now refuses an existing output with exit
4 (`output_conflict`) before doing any work, and leaves it untouched. Before
0.5, `autospill`, `autospread`, `autospectral`, `peacoqc`, `cytoscan`, `gate`,
`clean` and `flowsom` (without a subcommand) silently replaced their outputs.
They gain `--overwrite`, as the other commands already had.

- A rerun into the same paths needs `--overwrite`, or `--reuse skip` to reuse
  a verified matching output without recomputing.
- A directory output (`autospectral`, `cytoscan`, `clean`) must be missing or
  empty without `--overwrite`. With it, the run's files replace their earlier
  versions; other files in the directory are kept and are no longer listed
  as outputs in `provenance.json`.
- `peacoqc --debug-dir` artifacts follow the same rule: a run either
  publishes the cleaned FCS, mask, summary and debug artifacts, or none.
- `--debug-dir` forces a recomputation, so with `--reuse skip` it also needs
  `--overwrite` when the output exists.
- `flowsom` (without a subcommand) now publishes its assignment CSV
  atomically: a failed run no longer leaves a truncated file.
- `flowsom train`, `flowsom apply`, `cytonorm train`, `cytonorm apply` and
  `classify` reported an existing output as invalid input (exit 2). They now
  report it as an output conflict (exit 4), the documented code, as every
  other command does.

Rust API: `GatingConfig`, `PeacoQcConfig`, `CleanConfig`, `CytoScanConfig` and
`AutoSpectralConfig` gain `overwrite`; `FlowSomRunner::run` gains an
`overwrite` argument; `cytiq_autospill::export_matrix`,
`export_spread_matrix` and `cytiq_autospectral::write_outputs` take an
`Existing`. Pass `false` / `Existing::Refuse` for the new default, `true` /
`Existing::Replace` for the old behaviour.

## FlowSOM cohorts (additive)

`cytiq flowsom train` with several `--input` files now accepts one or more
`--mask` per input: each mask applies to the input of its acquisition. Every
input then needs a mask, every mask must match an input, and an acquisition
may not be listed twice. `--cohort-sampling per-file --training-events N`
draws `ceil(N / inputs)` events from each input instead of sampling the pooled
events, so large acquisitions no longer dominate the scale and the training
set. Without these options training is byte-identical to 0.4.1, and
`cohort_sampling` is recorded in provenance only when it is `per-file`.

A per-file model carries an optional `cohort` object. 0.5 reads 0.4 models
unchanged; 0.4 refuses a per-file model (unknown field `cohort`) rather than
misreading it.

Rust API: `FlowSomConfig` gains `cohort_sampling` (default `Pooled`) and
`FlowSomModel` gains `cohort` (`None` for pooled models; struct literals need
`cohort: None`). `cytiq_flowsom::{subsample_rows, sample_seed}` is the shared
sampler; CytoNorm's private copy is gone and its bundles are unchanged.

## Gating-ML (additive, one Rust API break)

- `QuadrantGate` is supported, with FlowKit's semantics (verified on
  `data/fixtures/gating/quadrant/`). Each quadrant is a gate; the
  QuadrantGate id is not, and referencing it is an error. Strategies without
  QuadrantGates evaluate exactly as before.
- `to_gatingml` writes `logicle` and `hyperlog` transformations instead of
  failing, and QuadrantGates; every parsed document round-trips.
- A `ratio-dimension` is reported as `UnsupportedElement` instead of
  `InvalidGatingMl` (still exit 2).

Rust API: `GateKind` gains `Quadrant { definition, dimensions }`; exhaustive
matches outside `cytiq-gating` need an arm (treating it like `Rectangle`
gives the right geometry). `Logicle::parameters` and `Hyperlog::parameters`
return `[T, W, M, A]`.

## Biexponential parameters (additive)

`cytiq transform --method biexponential` accepts `--biexponential-length`,
`--biexponential-max-range`, `--biexponential-pos`, `--biexponential-neg` and
`--biexponential-width` (FlowJo `biex` attributes). Their defaults are the
values used before, so default output is unchanged. They appear in the
provenance `config` only when one differs from its default; `CYTIQ_TRANSFORM`
always records the effective values.
`cytiq_transform::DEFAULT_BIEXPONENTIAL_*` are now public.
