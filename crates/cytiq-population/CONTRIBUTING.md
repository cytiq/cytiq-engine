# Contributing to cytiq-population

Workspace guidance lives in [CONTRIBUTING.md](../../CONTRIBUTING.md).

All documents reject unknown versions and invalid references. Keep measured integers exact, preserve missing states, and never infer biological partitions from parent links. Tests are offline and must not depend on sibling repositories. Keep public formats and CLI examples in README.md current.

## Fuzzing

`fuzz/` holds cargo-fuzz targets outside the workspace; what they check lives
in `fuzz/checks.rs`. Run one with `just fuzz cytiq-population <target> [seconds]` (the
nightly that `mise.toml` pins). `tests/fuzz_regressions.rs` includes
`fuzz/checks.rs` and replays every file in `tests/fuzz_regressions/<target>/`,
so `cargo test` covers what the fuzzer found. Hand-written seeds pin a verdict
by name: `valid*` must be accepted, other names rejected. When the fuzzer finds
a crash, copy the input to `tests/fuzz_regressions/<target>/crash-<sha1>`, fix
the bug, and add a named test for it next to the code it exercises. Never name
those directories `corpus` or `artifacts`: `fuzz/.gitignore` ignores both.

Targets:

- `document_json` and `document_yaml` (`serialization::parse`): never panics;
  accepted documents are mappings with a stable digest.
- `observations_parquet` (`table::read_from`): accepted rows validate.
- `projected_parquet` (`cytiq_mask::parquet::ScalarParquetReader` with a
  projection, as `annotations.rs` reads QC masks and classify or membership
  tables): accepted batches carry exactly the projected columns and the
  footer's row count.

Every Parquet input goes through the bounded scalar reader, not Arrow's
packed decoder, which panics on corrupt definition levels in parquet 60.1.0.
The saved `crash-*` inputs replay against both Parquet targets; all four
targets run in the CI fuzz matrix (`.github/workflows/fuzz.yml`).

## Contracts

`contracts/cytiq-tree.lock.json` pins the vendored cytiq-tree schemas by
revision and raw-byte sha256; `tests/contracts.rs` checks every vendored file
against it. To bump the pin, copy the schemas from the new revision, update
the lock, and run `just contracts-upstream`.
