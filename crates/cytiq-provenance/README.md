# cytiq-provenance

Run records for cytiq commands: which command, with which arguments, read
which files and wrote which files.

Every command that writes files writes a `provenance.json` next to its output.
`meta` to stdout carries the input hash in its document and writes no record:

- `<output>.provenance.json` when the output is a file (`sample.comp.fcs` →
  `sample.comp.fcs.provenance.json`);
- `<dir>/provenance.json` when the output is a directory (`clean`,
  `autospectral`, `cytoscan`, `transform` with a directory output).

```json
{
  "schema_version": 3,
  "tool": "cytiq compensate",
  "version": "0.1.0",
  "build": { "git_commit": "…", "rustc": "rustc 1.99.1 …", "target": "aarch64-apple-darwin", "profile": "release" },
  "platform": { "os": "macos", "arch": "aarch64", "cpus": 12 },
  "arguments": ["compensate", "--input", "sample.fcs", "--output", "sample.comp.fcs"],
  "config": { "input": "sample.fcs", "output": "sample.comp.fcs", "matrix": null, "matrix_kind": null, "overwrite": false },
  "constants": null,
  "algorithms": { "FlowKit": "1.3.0", "flowCore": "2.22.1" },
  "started_at_unix_ms": 1792790000000,
  "finished_at_unix_ms": 1792790000420,
  "inputs":  [{ "path": "sample.fcs", "role": "input", "bytes": 320905, "sha256": "…" }],
  "outputs": [{ "path": "sample.comp.fcs", "bytes": 321580, "sha256": "…" },
              { "path": "sample.comp.masks.parquet", "bytes": 21544, "sha256": "…" }],
  "execution_fingerprint": "…",
  "reason": "success"
}
```

`arguments` is the exact command line, so a run can be repeated from its
record; `config` is every option with its default applied, so the record
stays meaningful when defaults change. `constants` and `algorithms` capture
what is fixed in the code: algorithm constants and the upstream versions the
port is validated against. Inputs are hashed before the command runs, so a
command that replaces its own input still records what it read; each input
carries its `role`. Debug directories are not listed.

`build.git_commit` is `CYTIQ_BUILD_COMMIT` when set at build time (as in
Docker), else the commit cargo recorded when packaging the crate (a crates.io
install; `-dirty` marks uncommitted changes), else `git rev-parse HEAD` with `-dirty.<digest>` for modified source. An unknown
commit or a packaged bare `-dirty` build cannot be reused. Following each record's inputs back to earlier outputs
reconstructs the chain from the raw FCS files.

Schema version 2 added `build`, `platform`, `config`, `constants`,
`algorithms` and input roles, and moved input hashing before the run.

## Public API

`RunRecorder::prepare_staged` builds a v3 record from `(final_path, staged_path)`
pairs, verifying input byte identities before publication. It writes nothing;
the caller publishes it with data and an `outcome::CommandOutcome` v1 in one
directory transaction. `finish` and `prepare_staged` require an explicit `OutcomeReason`. Readers still accept
v2 records; reuse requires v3.

| Item | Purpose |
| --- | --- |
| `RunRecorder::start` / `snapshot_inputs` / `snapshot_roles` / `finish` | Start a record and hash the inputs, with their role, before a command runs; hash its outputs and write the record afterwards |
| `RunRecorder::with_build`, `set_config`, `set_constants`, `set_algorithms` | Build info, effective configuration, fixed algorithm constants, upstream versions |
| `RunRecord`, `FileRecord`, `BuildInfo`, `Platform`, `SCHEMA_VERSION` (3) | The document |
| `record_path_for_file`, `record_path_for_dir` | Where a command's record goes |
| `files_under` | Every file of an output directory, sorted, without records and `.tmp` files or directories (staged outputs and `.old.tmp` backups of `cytiq-publish`) |
| `DirSnapshot` | `take` before a run, `published` after it: the files of a directory output the run wrote or replaced, so leftovers of earlier runs are never recorded |
| `hash_file` | Streamed SHA-256 (also used by `cytiq-inspect`) |
| `outcome::OutcomeReason`, `exit_code` | The 0–4 exit contract of every `cytiq` command: `success` 0, `runtime_error` 1, `invalid_configuration` 2, `checks_failed` 3, `output_conflict`/`incomplete` 4 |
| `OutcomeReason::of_io_kind` / `of_io` / `of_publish` | Type-independent classification: a missing path is `incomplete`, an existing one `output_conflict`, malformed or truncated data `invalid_configuration`, other I/O and staging failures `runtime_error` |
| `outcome::CommandOutcome` (v1), `new`, `write` | The JSON completion record (`--outcome`, `outcome.json` of packages); `write` replaces the file atomically |
| `ProvenanceError`, `outcome_reason` | Error type and its place in the exit contract |

## Rules

- Records are written through `cytiq-publish` (a synced temporary file,
  renamed into place), so a failed run never leaves half a record.
- A run's outputs are complete when its record exists and every output it
  lists has the recorded size and SHA-256. A failed run publishes none of its
  outputs, but a crash in the middle of publishing can leave some outputs new
  and others previous ones next to the previous run's record; only the hashes
  tell.
- Paths that are not files (optional inputs and outputs that were not given)
  are skipped.
- An input that changed during the run and is not also an output fails
  `finish` (`InputChanged`) rather than write a record that does not describe
  what was read. An input that is also an output (`transform --overwrite` in
  place) is recorded with both hashes.
- `--dry-run` runs write no record.

## Dependencies

`serde` / `serde_json`, `sha2`, `log`, `thiserror`.

## Tests

```bash
cargo test -p cytiq-provenance
cargo test -p cytiq-engine   # every *_cli.rs checks its command's record via tests/common
```

## Execution fingerprints and reuse

Version 3 adds optional `execution_fingerprint`, `reason` and `parallelism`.
`RunRecorder::execution_fingerprint()` and `RunRecord::compute_fingerprint()` use
one versioned SHA-256 of sorted JSON keys: tool/version, build, effective semantic
configuration, constants, algorithm versions, ordered input paths/roles/sizes/hashes,
and recorded parallelism. Timestamps, platform, arguments and outputs are excluded.
`overwrite`, `debug_dir`, `dry_run` and `threads` are removed recursively from config;
every other option is semantic. Classify records effective parallelism separately,
because ONNX results can depend on it.

`reuse::assess` verifies the fingerprint and every output size/hash under the caller's
output lock. `reuse::decide` implements recompute/skip/reject; v2, stale, damaged and
unrecorded results are never reused. `CommandOutcome.execution` reports the
fingerprint and whether the result was reused. Input paths remain significant,
and assessment re-hashes all outputs. An unlocked dependency update at `cargo install`
time can change a build without changing its source commit; use `--locked`.
