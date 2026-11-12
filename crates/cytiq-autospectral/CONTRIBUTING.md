# Contributing to cytiq-autospectral

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — crate docs and re-exports only.
- `src/manifest.rs` — CSV row types, readers and validation; also
  `is_autofluorescence_dye` and `normalize_identifier` used by the runner.
- `src/runner.rs` — `AutoSpectralRunner`, algorithm constants, detector and
  primary-channel inference, scatter gate, positive selection, condition
  number, QR least-squares unmixing, control cleaning and
  `parallel_try_map`.
- `src/spectrum.rs` — `get.fluorophore.spectra`'s estimator: `MASS::rlm`
  slopes, scatter-matched negatives (`FNN::knnx.index`); unit tests against
  `rlm` fixtures from `parity/make_fixtures.R`.
- `src/export.rs` — `write_outputs`: reference CSV/JSON, unmixed FCS and its
  margin mask, `diagnostics/run.json`, debug summary.
- `src/error.rs` — `AutoSpectralError` (`thiserror`).
- `tests/autospectral.rs`, `tests/support.rs` — runner tests on the fixture set.

## Invariants

- Parity with upstream AutoSpectral OLS is the reference. The constants in
  `runner.rs` mirror `asp$*` parameters (`POSITIVITY_THRESHOLD`,
  `POSITIVE_EVENT_LIMIT`, `NEGATIVE_EVENT_LIMIT`, `MIN_POSITIVE_EVENTS`;
  `spectrum.rs`: `RLM_MAX_ITERATIONS`, `SCATTER_NEIGHBOURS`); changing them,
  the estimator (`rlm` slopes against the peak detector, divided by the
  largest) or the quantile definition (R type 7) changes results. Re-run
  `just parity` and `just parity-real` (the Aurora panel) and update
  `parity/REPORT.md` when you do.
- Spectra are columns of the reference matrix, in manifest order of single
  stains with the autofluorescence component last. Component order defines the
  FCS column order and the JSON/CSV output.
- The condition-number warning uses fluorophores only; the hard limit
  (`CONDITION_NUMBER_LIMIT`) uses the full matrix including AF.
- Parallel stages go through `parallel_try_map`, which keeps results and errors
  in input order. Keep outputs deterministic regardless of the thread count.
- The runner runs on the caller's rayon pool; the CLI sizes the global pool
  with `--threads`.
- Unmixed FCS files carry the sample's acquisition keywords, but never its
  spillover matrix or earlier `CYTIQ_*` state, which describe the replaced
  detectors. They must record `CYTIQ_UNMIXING`, so later stages know the
  components have no detector margins and must not be compensated.
- The least-squares solve is a Householder QR (`unmixing_matrix`). nalgebra's
  SVD and the normal equations were measured less accurate for similar
  spectra (`qr_solver_keeps_precision_for_similar_spectra`).
- `clean_controls` is the default (`--quantile-gate` turns it off): the
  quantile box keeps debris and saturated events of real cell controls, which
  bias the spectra. It changes the retained control events and so the
  spectra.
- Diagnostics use `log`; never print to stdout.

## Where changes belong

- FCS reading/writing and keyword handling → `cytiq-fcs`.
- Flags and `--help` text → `crates/cytiq-engine/src/commands/autospectral.rs`.
- A shared KDE scatter gate (the parity report suggests reusing the
  `cytiq-autospill` gate) → `cytiq-density`, not a copy in this crate.
- Fixture changes → `data/fixtures/autospectral/` and its README.

## Testing

```bash
cargo test -p cytiq-autospectral
cargo test -p cytiq-engine --test autospectral_cli
```

Unit tests live in `src/manifest.rs` (manifest validation, column aliases) and
`src/runner.rs` (reference matrix layout, OLS recovery, detector inference,
`parallel_try_map` ordering). `tests/support.rs` is also compiled into
`crates/cytiq-engine/tests/autospectral_cli.rs` via `#[path]`; keep its helpers
generic. Parity scripts live in `parity/` (`run_all.sh`, `compare.py`).
