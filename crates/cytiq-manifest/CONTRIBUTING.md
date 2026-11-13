# Contributing to cytiq-manifest

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — `ManifestError`, bounded `load` and parsing; unit tests.
- `src/run.rs` — the document types and the panel, control and
  compensation queries.
- `src/check.rs` — `validate`: the consistency rules of every version.
- `tests/migration.rs` — earlier versions keep loading, and their serialized
  form is pinned in `tests/fixtures/expected/` (`CYTIQ_BLESS=1` rewrites it
  when a change is intended).

## Invariants

- The manifest format is a user-facing contract documented in the root
  `README.md` (`## Preflight`) and this crate's README. Additions must stay
  optional (`#[serde(default)]`) so existing manifests keep loading.
- YAML and JSON describe the same document; tests load both.
- Every struct uses `deny_unknown_fields`, so typos fail loudly.
- No file I/O beyond reading the manifest: checking FCS files belongs in
  `cytiq-qc::preflight`.

## Testing

```bash
cargo test -p cytiq-manifest
```
