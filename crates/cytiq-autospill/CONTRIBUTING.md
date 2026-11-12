# Contributing to cytiq-autospill

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — crate docs and re-exports only.
- `src/runner.rs` — `AutospillRunner` / `AutospreadRunner`, config and result
  types, control loading and primary-channel inference, autofluorescence
  proxy, initial spillover, refinement state machine, spread estimation.
- `src/regression.rs` — `MASS::rlm` port, OLS with/without intercept, joint
  trimming, skewness, percentiles and robust SD.
- `src/transform.rs` — per-channel FlowJo biexponential built on
  `cytiq_transform::scales::FlowJoBiexp`, and the transformed-to-linear slope
  conversion.
- `src/manifest.rs` — controls, detectors and transform CSV manifests with
  validation.
- `src/export.rs` — matrix export to CSV / TSV / Parquet.
- `src/plotting.rs` — `--debug-dir` `report.html` and `summary.json`.
- `src/error.rs` — `AutospillError` (`thiserror`).

## Invariants

- This crate follows upstream R `autospill` step by step; the goal is parity,
  not Rust-specific tuning. The constants at the top of `src/runner.rs`
  (`RLM_*`, `RS_*`), the rlm details in `src/regression.rs` (Huber `k = 1.345`,
  scale `median(|r|) / 0.6745`, `irls.delta <= 1e-4`, OLS fallback, `trim_n`
  rounding half to even) and the biexponential defaults in `src/transform.rs`
  are algorithm-compatibility values. Changing one requires re-running the
  parity suite and updating `parity/REPORT.md`.
- The refinement state machine must keep R's behaviour on the linear → biexp
  switch (λ `1 → 0.1`, `delta_change` reset to `-1`); `parity/REPORT.md` 2.2
  lists the bugs that came from diverging here.
- Controls are read as `flowCore::read.FCS` reads them in upstream
  `read.flow.control`: every value above its channel's `$PnR` is capped at
  `$PnR`. The synthetic parity data never exceeds `$PnR`; the real-data suite
  (`parity/real/`, `autospill` chain) is what covers this.
- Gating lives in `cytiq-density` and is shared with `cytiq-clean`; change it
  there, with the same parity checks.
- Primary channels are always reported by their canonical `$PnN` short name.
  Dye names must be unique, inferred primary channels must be unique, and an
  ambiguous or missing match is an error, never a guess.
- Square output with a `channel` header is the backward-compatible layout;
  keep it when detectors equal the primary channels.
- Results do not depend on the thread count: parallel stages collect in input
  order. The runners use the caller's rayon pool.
- AutoSpread has no upstream reference; its golden file
  `data/fixtures/autospread/expected_spread.csv` is regenerated only on an
  intentional algorithm change, and the PR must say so.

## Where changes belong

- FSC/SSC gate, KDE and generic statistics shared with other crates:
  `cytiq-density`.
- Display scales: `cytiq-transform::scales`.
- CLI flags and output path handling: `crates/cytiq-engine/src/commands/autospill.rs` and `autospread.rs`.

## Testing

```bash
cargo test -p cytiq-autospill
cargo test -p cytiq-engine --test autospill_cli
parity/run_all.sh   # after parity/setup_env.sh; needs R + Python
parity/real/run_real.sh   # real controls; CYTIQ_REAL_SLOW=1 adds the 24-colour set
```

Unit tests sit next to the code in `export.rs`, `manifest.rs`,
`regression.rs` and `transform.rs`. Integration tests are in
`tests/autospill.rs`, with fixture builders in `tests/support.rs`; they use the
checked-in fixtures `data/fixtures/autospill/` (expected spillover and
compensation matrices) and `data/fixtures/autospread/` (expected spread
matrix), plus synthetic controls for rectangular detector sets,
autofluorescence, singular matrices, channel-inference errors and values
above `$PnR`.
