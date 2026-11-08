# Contributing to cytiq-clean

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — crate docs and re-exports only.
- `src/runner.rs` — `CleanRunner`, `CleanConfig` validation, fitted vs supplied
  (`--gates`) plans, mask collection and `metrics.json` counts.
- `src/channels.rs` — `ChannelRoles` resolution and viability-dye detection.
- `src/debris.rs` — debris filter (adaptive valley, population, fixed).
- `src/singlets.rs` — singlet filter (FSC-A/FSC-H wedge or FSC-W bound).
- `src/viability.rs` — viability filter (`fasinh` axis, valley / control / threshold).
- `src/filter.rs` — shared types: gate ids, `FilterStatus`, `FilterReport`,
  `FilterFit`. Fitting subsets come from `cytiq_density::sample_indices`.
- `src/export.rs` — writes `masks.parquet` (through `cytiq-mask`),
  `metrics.json`, `gates.gatingml.xml`.
- `src/report.rs` — `report.html` (Plotly, at most 10,000 plotted events).
- `src/error.rs` — `CleanError` (`thiserror`).

## Invariants

- Never pretend a filter passed. A filter that cannot be fitted reliably returns
  `not_assessed` with a `reason`, its mask column is null and it is excluded
  from `keep`. This is the core contract of the crate.
- Masks are always computed by evaluating a `GatingStrategy` through
  `cytiq-gating`, and the same strategy is written with `to_gatingml`. The
  exported file must reproduce the masks exactly (`exported_gates_reproduce_masks`).
  Consequently fitted gates may only use what `to_gatingml` can write: no
  `logicle` or `hyperlog` transforms.
- Gate ids `non_debris`, `singlets`, `live`, `clean` are a contract with
  `--gates`; renaming them breaks re-application of edited files.
- `clean` is a Boolean `and`; with a single applied filter the gate is ANDed
  with itself, because Gating-ML `and` needs two operands.
- Fitting is deterministic: subsets come from `cytiq_density::sample_indices` (evenly spaced),
  never from a random sample. Fit order is debris → singlets (on non-debris) →
  viability (on non-debris singlets).
- `event_id` is the 0-based input row; events are never reordered or copied.
- Config validation lives in `validate` in `runner.rs`; keep error messages in
  terms of CLI flags, since they surface to users unchanged.
- An unmet `--require` is an assessment failure: publish diagnostics before
  returning `RequiredFilterNotApplied`; the CLI records `checks_failed`/exit 3.
  Input, computation and publication errors retain the ordinary rollback contract.
- `metrics.json` and the `masks.parquet` schema are output contracts; update
  this crate's [README](README.md#output-rules) when they change.

## Where changes belong

- KDE, valley detection, median/MAD and the FSC/SSC population gate live in
  `cytiq-density`; share improvements there rather than forking them here.
- Gate semantics and Gating-ML I/O belong in `cytiq-gating`.
- CLI flags belong in `crates/cytiq-engine/src/commands/clean.rs` (`CleanArgs`). Keep this
  crate free of `clap`.

## Testing

```bash
cargo test -p cytiq-clean
cargo test -p cytiq-engine --test clean_cli
```

Unit tests sit next to the code in `channels.rs` and `viability.rs`. `clean_cli.rs` checks recovery of known debris, doublets and
dead cells (>98% accuracy per filter) against `data/fixtures/clean/truth.json`,
`not_assessed`/`disabled` handling, `--gates` re-application and the control
threshold. If you change the synthetic data, regenerate the fixtures with
`parity/.env/py/bin/python parity/gen/clean.py data/fixtures/clean`.
