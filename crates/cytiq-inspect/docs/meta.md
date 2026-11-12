# Metadata

Exports the metadata of one FCS file as a single JSON document. Backs the
`cytiq meta` command.

Derived FCS files written by other `cytiq` commands carry over the source's
instrument and acquisition keywords, but drop or rename keywords that no longer
describe their values (for example the spillover matrix after `compensate` or
`transform`). Run `cytiq meta` on raw files first to keep the full record.

## What is recorded

- file identity: path, file name, size in bytes, SHA-256 of the raw bytes
- FCS version (`FCS3.0` / `FCS3.1` / `FCS3.2`)
- TEXT / DATA / ANALYSIS segment offsets, as the HEADER stores them, with a
  fallback to `$BEGINDATA`/`$ENDDATA` and `$BEGINANALYSIS`/`$ENDANALYSIS`
  when the HEADER field is `0`. That is the case for segments ending beyond
  byte `99_999_999`, which do not fit the 8-digit HEADER fields. `cytiq-fcs`
  treats the TEXT keywords as authoritative and rejects a file whose non-zero
  HEADER offsets disagree with them.
- event count (decoded rows, falling back to `$TOT`)
- every parameter with its name, label, data type, width and `$Pn*` keywords
- every primary and supplemental TEXT keyword as `[key, value]` pairs, in file
  order and exactly as stored

The document is versioned by `schema_version` (currently `1`):

```json
{
  "schema_version": 1,
  "tool": "cytiq meta",
  "version": "0.1.0",
  "file": { "path": "raw.fcs", "name": "raw.fcs", "bytes": 96813, "sha256": "..." },
  "fcs_version": "FCS3.1",
  "segments": { "text": [256, 812], "data": [813, 96812], "analysis": null },
  "event_count": 4000,
  "parameter_count": 6,
  "parameters": [
    { "index": 1, "name": "FSC-A", "label": null, "data_type": "float",
      "width": { "bits": 32 }, "keywords": { "$P1B": "32", "$P1N": "FSC-A", "...": "..." } }
  ],
  "text": [["$CYT", "..."], ["$DATE", "..."]],
  "supplemental_text": null
}
```

## Usage

```bash
cytiq meta --input raw.fcs | jq '.text | length'
cytiq meta --input raw.fcs --output raw.meta.json --overwrite
```

A file `--output` also gets `raw.meta.json.provenance.json` (the SHA-256 of the
input and of the document), like every other command's output; stdout output
writes no record.

From Rust:

```rust
use cytiq_inspect::meta::{MetaConfig, MetaRunner, read_metadata};

// In-memory only.
let metadata = read_metadata("raw.fcs")?;
println!("{} events", metadata.event_count);

// Full command: writes pretty JSON to a file, or to stdout when `output` is `None`.
let result = MetaRunner::run(&MetaConfig {
    input: "raw.fcs".into(),
    output: Some("raw.meta.json".into()),
    overwrite: false,
})?;
```

## Public API

| Item | Purpose |
| --- | --- |
| `read_metadata` | Read and hash one FCS file into `FcsMetadata` |
| `MetaRunner::run` / `MetaConfig` / `MetaRunResult` | The `cytiq meta` command: read, serialize, write |
| `FcsMetadata`, `FileIdentity`, `SegmentOffsets`, `ParameterMetadata`, `ParameterWidthMetadata` | Serializable document types |
| `MetaError` | Error type for every failure mode |

## Output rules

- `output: None` writes to stdout; logs go to stderr, so piping is safe.
- An existing output file is an error unless `overwrite` is set.
- Files are written through `cytiq-publish` (a synced temporary file beside
  the output, renamed into place), so a failed run never leaves a truncated
  document.

## Dependencies

- [`cytiq-fcs`](../../cytiq-fcs) for parsing
- `serde` / `serde_json` for the document,
  [`cytiq-provenance`](../../cytiq-provenance) for hashing

## Tests

End-to-end behaviour is covered by `crates/cytiq-engine/tests/meta_cli.rs`:

```bash
cargo test -p cytiq-engine --test meta_cli
```
