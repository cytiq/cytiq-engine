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
  `GatingError`. A zero thread count is rejected by `ThreadPoolBuilder`.

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
