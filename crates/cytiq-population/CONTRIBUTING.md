# Contributing to cytiq-population

Workspace guidance lives in [CONTRIBUTING.md](../../CONTRIBUTING.md).

All documents reject unknown versions and invalid references. Keep measured integers exact, preserve missing states, and never infer biological partitions from parent links. Tests are offline and must not depend on sibling repositories. Keep public formats and CLI examples in README.md current.

## Dataset modules

- `identity.rs` keeps three identities apart: the path-free semantic id
  (`semantic_id`), the content digest and size (`ContentRef`) and the
  execution reference to a v3 provenance record (`ExecutionRef`). Objects
  are located by relative `ObjectKey`s only; never record an absolute path,
  URI or user-info.
- `issue.rs` holds the typed validation codes. Codes are a public contract:
  add new ones, never rename or reuse one.
- `privacy.rs` is an allowlist. Adding a keyword to `ALLOWED_KEYWORDS` or
  `ALLOWED_PARAMETER_SUFFIXES` needs a reason it cannot identify a person,
  a patient or a site; anything else goes through a `Disclosure`.
- `bitmap.rs` is the fallible per-event flag set shared by lineage checks.
- `events/` is `cytiq.events.v1`: `descriptor.rs` builds the layer
  descriptor from the `CYTIQ_*` signal keywords, `write.rs` streams an FCS
  file into the table (one batch of at most 16 MiB at a time) and `read.rs`
  verifies it through the bounded reader. A layer's identity is the digest
  of its descriptor; never put a path, timestamp or build in it. A
  compensation matrix or unmixing reference that the file records by path
  enters only as a digest the caller supplies. Changing the columns, the key
  metadata, the descriptor or the values digest is a new contract version.
- `tests/events.rs` builds synthetic FCS files with the cytiq-fcs writer;
  every refusal and every verification code has a test.
- `dataset/` is the dataset manifest v1. `mod.rs` holds its types, identity
  and parsing, `document.rs` the rules that need no I/O (one function per
  rule family), `objects.rs` the rules that read objects, `profile.rs` the
  metadata profiles and `from_run.rs` the entries built from a run manifest
  and FCS files. Every issue code has a test in `tests/dataset.rs`, which
  builds a two-panel dataset (`tests/common/mod.rs`).
- `contracts/dataset/v1/` is a published contract. `tests/dataset_contracts.rs`
  checks it against `contracts/dataset/v1.lock.json`, reproduces
  `identity-vectors.json` and checks that what cytiq writes conforms to the
  schemas. A released version never changes: a change is a new version
  directory, a row in `dataset::migrate` and migration tests.
  `CYTIQ_BLESS=1` rewrites the lock and vectors only for an unreleased
  version.
- `tests/dataset_fixtures.rs` checks the reference dataset of
  `data/fixtures/dataset` offline. It verifies `sha256.json`, validates the
  reference under both profiles with every object verified, checks schema
  conformance and that no identifying text leaks, and replays every
  `malformed/*.json` patch against its expected code. Add a malformed case
  for every new issue code. `cytiq-engine`'s `dataset_fixtures` test rebuilds
  the reference with the commands (see the fixture README).

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
- `dataset_json` (`dataset::parse` and `validate_document`): never panics,
  a rejection names an error, and an accepted manifest round-trips through
  its canonical JSON with the same `dataset_id`.
- `events_parquet` (`events::verify_with_limits`): never panics, a
  rejection names an error, and an accepted table is consistent and
  verifies identically twice.
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
