# cytiq-qc

Checks one sample's final event mask against the QC policy of its panel in
the run manifest. Backs `cytiq qc`.

The optional [`preflight` module](docs/preflight.md) also backs `cytiq preflight`
and checks a run manifest and its FCS files before computation. Enable the
`preflight` feature for that Rust API; the CLI enables it automatically.

```bash
cytiq qc --manifest run.yaml --sample S01 --mask sample.analysis.parquet --output qc.json
```

Add `--require-policy` where the report decides whether a sample is released:
a panel without an effective policy then fails instead of warning. `qc: {}`
and policies containing only empty filter entries are not effective policies.
An optional check that did not assess events warns rather than passing.

Population publication reevaluates the pinned panel policy on the actual mask.
It requires an effective policy and every `required` assessment, even if that
filter's exploratory severity is `warn`. Missing requirements produce rejected
observations and a retained `release-qc.json` assessment. Specimen reconciliation
uses that hash-verified snapshot without reopening event files. No extra QC mode
or configuration layer is needed.

The policy is the panel's `qc` section (`cytiq_manifest::QcPolicy`):

```yaml
qc:
  min_events: 10000
  min_kept_events: 3000
  max_removed_fraction: 0.6
  filters:
    peacoqc_pass: { required: true, min_assessed_fraction: 0.99, assessed_of: eligible, max_removed_fraction: 0.3 }
    live: { min_assessed_fraction: 0.95, max_removed_fraction: 0.5, severity: warn }
```

## Checks

| Check | Scope | Fails when | Warns when |
| --- | --- | --- | --- |
| `mask_acquisition` | sample | the mask belongs to another raw acquisition | |
| `mask_coverage` | sample | the mask has no row for a raw event, or rows for events the raw file does not have | |
| `policy` | sample | the panel has no `qc` and `--require-policy` is set | the panel has no `qc` |
| `min_events` | sample | the raw file has fewer events | |
| `min_kept_events` | sample | the mask keeps fewer events | |
| `max_removed_fraction` | sample | `1 - kept / raw events` is larger | |
| `filter_assessed` | `filter <name>` | a `required` filter is missing or assessed no event (`severity: fail`) | the same with `severity: warn` |
| `filter_assessed_fraction` | `filter <name>` in the mask | the filter assessed a smaller share of the events `assessed_of` names than `min_assessed_fraction` (`severity: fail`); passes when no event was eligible | the same with `severity: warn` |
| `filter_removed_fraction` | `filter <name>` | the filter removed more of the events it assessed (`severity: fail`) | the same with `severity: warn` |

Columns `cytiq mask combine` prefixed after a repeated name (`2.live`) count
as the same filter: an event is assessed when any column is non-null and
removed when any is `false`.
Recombining an already combined mask can add further numeric prefixes
(`2.2.live`); all such combine prefixes resolve to the original filter.

`min_assessed_fraction` is a share of the events `assessed_of` names:
- `raw` (default): every raw event. A filter that only saw the events an
  earlier stage kept (`peacoqc_pass` after the margin mask) assesses fewer,
  so its bound has to allow for them.
- `eligible`: the events no other filter removed. The mask has no stage
  order, but an event that never reached a later filter was removed by an
  earlier one, so this is what a filter saw when it ran after the others,
  and all it had to assess when it ran beside them. An event it skipped that
  another filter removed is not counted against it: that event is dropped
  anyway.

## Public API

| Item | Purpose |
| --- | --- |
| `QcRunner::run` / `QcConfig` | Run the checks and write `qc.json` |
| `QcRunner::input_files` | The manifest, the sample FCS and the mask, for provenance |
| `QcReport` (`ensure_passed`) | The report: status, counts, metrics, checks |
| `QcMetrics`, `FilterMetrics` | Raw and kept events; per filter: columns, assessed (count and share of raw events), eligible events and the share of them assessed, removed |
| `evaluate` | The policy rules over metrics, without files |
| `evaluate_release` | Effective panel policy and mandatory assessments required for release |
| `QcCheck`, `CheckStatus` | One finding: `pass`, `warn`, `fail`, with `value` and `limit` |
| `QcError` | Error type |

## Dependencies

[`cytiq-fcs`](../cytiq-fcs) (acquisition identity),
[`cytiq-manifest`](../cytiq-manifest) (`QcPolicy`),
[`cytiq-mask`](../cytiq-mask), `serde` / `serde_json`.

Default features are empty. Feature `preflight` adds `cytiq-compensate` for
matrix-file checks. Standalone QC and specimen consumers do not enable it.
`preflight::CheckStatus` re-exports the root `CheckStatus`.

## Rust API migration

Replace a `cytiq-preflight` dependency with `cytiq-qc` at the matching workspace
release version and enable its `preflight` feature. Imports move from
`cytiq_preflight::{…}` to `cytiq_qc::preflight::{…}`; type, function, constant and
error-variant names stay the same. There is no compatibility package.

The existing root QC API, including `cytiq_qc::CheckStatus`, is preserved.
`cytiq preflight` retains its flags, findings, report format and exit codes.

## Tests

`retention::population_retention` compares ordered filtering masks with labels
indexed by the original acquisition's source event IDs. It reports conditional
and cumulative retention, population frequencies and signed absolute/relative
frequency errors after every stage. Optional technical-good truth separately
measures retention of good events and removal of known artifacts. Labels alone
never establish technical removal accuracy. Duplicate/out-of-range IDs and
mixed acquisitions are errors. A small overall removed fraction cannot establish
safety for a rare or weakly positive population.

```bash
cargo test -p cytiq-qc
cargo test -p cytiq-qc --features preflight
cargo test -p cytiq-engine --test qc_cli
cargo test -p cytiq-engine --test pipeline_cli   # the golden chain runs qc
```
