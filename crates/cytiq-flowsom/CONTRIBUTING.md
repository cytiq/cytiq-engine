# Contributing to cytiq-flowsom

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/model.rs` (a child of `runner`) — versioned portable SOM, frozen-scale
  inference, population statistics and independently implemented consensus.
  The matrix API is shared by CytoNorm; do not re-estimate scale during apply.

- `src/lib.rs` — crate docs (paper and upstream links) and re-exports only.
- `src/runner.rs` — constants, config and result types, FCS loading, channel
  resolution, scaling, the deterministic LCG, SOM training, event assignment,
  average-linkage metaclustering, summary; unit tests.
- `src/export.rs` — assignment CSV, debug `summary.json` / `nodes.csv` /
  `metaclusters.csv`, Plotly `report.html`.
- `src/error.rs` — `FlowSomError` (`thiserror`).

## Invariants

- Same input, same output. Training uses the in-crate `DeterministicRng`
  (64-bit LCG, top 53 bits for indices) seeded with `FLOWSOM_RANDOM_SEED`.
  Do not swap it for `rand` or add thread-dependent randomness; the CLI test
  `cli_runs_flowsom_with_compact_channels_deterministically` relies on it.
- SOM training is sequential by design. A per-event `rayon` dispatch inside the
  training loop once made it ~60× slower (`parity/REPORT.md` §2.5). Use
  `rayon` only for embarrassingly parallel steps (matrix assembly, assignment,
  output rows).
- The training schedule mirrors `FlowSOM::SOM` / `som.c`: bubble neighbourhood,
  Chebyshev grid distance, per-step linear radius and learning-rate decay,
  BMU-only updates below radius 1, sampling with replacement, distinct-event
  initialisation. Changes need a parity run (`parity/run_all.sh`) and an
  updated report.
- The `scale = TRUE`, 20-metacluster and average-linkage choices are documented
  MVP decisions. Changing them changes every user's clusters: update this
  crate's [README](README.md) and the report.
- IDs: `event_index` and `source_event_id` are 0-based; node, metacluster and
  grid coordinates are 1-based. Downstream joins on `source_event_id` depend
  on this.
- Grid size, metacluster count, seed, scaling and the training subsample are
  `FlowSomConfig` fields whose defaults are the `FLOWSOM_*` constants; with
  defaults the output must stay bit-identical to earlier versions. The
  training schedule (iterations, learning rate, radius) stays fixed constants.
- The training subsample draws from its own RNG stream
  (`seed ^ TRAINING_SAMPLE_STREAM`) and is sorted, so subsampling never changes
  the training draws of a full run with the same seed.

## Where changes belong

- FCS parsing belongs in `cytiq-fcs`, including channel lookup
  (`cytiq_fcs::find_parameter_index`) and `--channels` parsing
  (`cytiq_fcs::parse_channel_list`).
- Transformations belong in `cytiq-transform`; this crate clusters values as
  given.
- CLI flags belong in `crates/cytiq-engine/src/commands/flowsom.rs`.

## Testing

```bash
cargo test -p cytiq-flowsom
cargo test -p cytiq-engine --test flowsom_cli
```

- `src/runner.rs` `#[cfg(test)]` — deterministic training, radius quantile, Chebyshev distance, distinct-event
  initialisation, metacluster count.
- `tests/populations.rs` — `FlowSomModel::populations` signal means, empty
  metaclusters and input validation.
- `crates/cytiq-engine/tests/flowsom_cli.rs` — end-to-end runs with debug
  artifacts, determinism, event masks, missing input / channel / output, zero
  threads (rejected by the CLI parser), and the pinned pooled cohort models.
