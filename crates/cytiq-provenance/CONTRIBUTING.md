# Contributing to cytiq-provenance

Workspace-wide rules (commands, style, commits, PRs) live in the root
[CONTRIBUTING.md](../../CONTRIBUTING.md). This file covers what is specific to
this crate.

## Layout

- `src/lib.rs` — `RunRecorder`, `RunRecord`, `FileRecord`, record paths,
  `files_under`, `hash_file`; unit tests.
- `src/outcome.rs` — `OutcomeReason` (the 0–4 exit contract and its
  type-independent rules for I/O and publication errors) and
  `CommandOutcome`. Library-specific classification lives in
  `cytiq-engine/src/error`, never here: this crate must not depend on the
  domain crates.

## Invariants

- The record is a public contract versioned by `SCHEMA_VERSION`. Changing a
  field name, type or meaning bumps it and updates this crate's [README](README.md).
- `arguments` is the command line verbatim. Do not normalise or reorder it.
- Records are written through `cytiq-publish`: a synced temporary file in the
  same directory, renamed into place. `files_under` skips temporary files and
  directories (`*.tmp`, which covers the `*.old.tmp` backups a crash can
  leave) and provenance records.
- Inputs are hashed by `snapshot_inputs` before the command runs, never at
  `stage`.
- The CLI decides which files are inputs and outputs of each command
  (`crates/cytiq-engine/src/commands/*.rs` call `snapshot_inputs`, and `publish` in `src/provenance.rs`).
  When a command gains an input or output file, add it there and to its
  `*_cli.rs` provenance check.

## Testing

```bash
cargo test -p cytiq-provenance
cargo test -p cytiq-engine
```

## Reuse invariants

`fingerprint.rs` owns `FINGERPRINT_VERSION` and `NON_SEMANTIC_OPTIONS`. Bump the
fingerprint version when canonicalization or semantic inclusion changes. All options
are semantic except recursively removed overwrite/debug_dir/dry_run/threads;
classify must call `set_parallelism` with its effective thread count.
`reuse.rs` assesses records read-only; the caller holds the destination's OutputLock
through assessment and publication. Never accept v2, unidentified builds, missing
fingerprints or unchecked outputs. Test both fingerprint component changes and
reused failing outcomes. `stage` and `prepare_staged` require a completion reason.
