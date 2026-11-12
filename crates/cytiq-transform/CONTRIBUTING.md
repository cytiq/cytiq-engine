# Contributing to cytiq-transform

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — public config/result types, `TransformRunner`, input
  resolution (files, directories, globs), output planning, channel selection,
  the per-value transform operator (public as `ValueTransform`, rebuilt from a
  `CYTIQ_TRANSFORM` contract), metrics and FCS/JSON output.
- `src/scales.rs` — reference scales `Logicle`, `Hyperlog`, `FlowJoBiexp` with
  `scale`/`inverse` and parameter validation; public as `cytiq_transform::scales`.
- `src/export.rs` — `--debug-dir` artifacts: `summary.json`, `metrics.csv`,
  Plotly `report.html`.
- `src/error.rs` — `TransformError` (`thiserror`).

## Invariants

- `scales.rs` is a port of reference implementations (Parks logicle,
  Gating-ML hyperlog, cytolib `biexpTrans::computCalTbl` + R natural spline,
  including its `float` divisions and integer truncations). Keep the unit tests
  at their tolerances (`1e-9` against flowCore / flowWorkspace); do not replace
  these with approximate formulas. `parity/REPORT.md` 2.1 records why: earlier
  versions used different functions.
- `Logicle::scale` returns the Gating-ML `[0, 1]` scale; the `transform`
  command multiplies by `M` to match `flowCore::logicleTransform`. `Hyperlog`
  stays on `[0, 1]`. `cytiq-gating` and `cytiq-autospill` depend on these
  exact semantics.
- Defaults (`DEFAULT_LOGICLE_*`, `DEFAULT_BIEXPONENTIAL_*`, cofactor 150 in the
  CLI) are part of the public contract and match the AutoSpill / flowCore
  defaults. Changing one changes every output.
- The `CYTIQ_TRANSFORM` keyword and the `metadata_json` sidecar share the
  `TransformMetadataDocument` shape (`tool`, `version`, `method`, `parameters`,
  `files`). Treat field changes as a contract change and update this crate's
  [README](README.md#inputs-channels-and-outputs).
- Validate everything before writing: config, inputs, output plans
  (no duplicate outputs, no non-`.fcs` outputs, `overwrite` checks) and the
  metadata path are all checked before the first file is written. A
  `dry_run` must never touch the filesystem.
- Output is deterministic for a given input and config regardless of the
  thread count (the caller's rayon pool): parallelism is per file and per
  column, results are collected in input order.

## Where changes belong

- FCS parsing/writing details belong in `cytiq-fcs`.
- CLI flags belong in `crates/cytiq-engine/src/commands/transform.rs`; their
  defaults are the `DEFAULT_*` constants of this crate.
- New scales that other crates need go in `scales.rs` with reference tests.

## Testing

```bash
cargo test -p cytiq-transform
cargo test -p cytiq-engine --test transform_cli
```

Unit tests are in `src/lib.rs` (arcsinh formula, finite outputs of the
log-like methods, glob matching, bit-identical output against a frozen copy of
the pre-optimisation column transform) and `src/scales.rs` (flowCore /
flowWorkspace parity). `tests/publication.rs` checks that a non-finite input
publishes neither the FCS nor the metadata file. `tests/value_transform.rs` checks that
`ValueTransform::from_contract` reproduces the stored values of every method,
so `cytiq-plot` axes stay in step with the command. R reference values are included from
`data/fixtures/transform/flowcore_reference.rs`. End-to-end CLI tests in
`crates/cytiq-engine/tests/transform_cli.rs` cover metadata, multiple inputs,
globs, channel selection, all methods, `--dry-run`, `--overwrite` and
unsupported formats. For scale changes also re-run `parity/run_all.sh`.
