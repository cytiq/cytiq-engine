# cytiq-spillqc

Checks the spillover matrix applied to fully stained samples with
[CompensAID](https://github.com/Olsman/CompensAID)'s Secondary Stain Index
(SSI), and summarizes the flags across files compensated with the same matrix.
Backs the `cytiq spillqc` command.

`cytiq preflight` only checks that a `$SPILL` matrix is well formed and well
conditioned. A matrix can pass those checks and still hold a wrong coefficient,
for example 12% where the true spillover is 16%. That error skews every
compensated value downstream. This crate looks for such errors in the data.

> **Flags need review.** CompensAID reports 96% sensitivity on conventional
> data but only 0.16–0.27 precision against expert consensus (Olsman et al.,
> 2026). A flag is a pair to inspect on a dot plot, not a correction. Never
> change a spillover value because of a flag alone.

## Usage

```bash
# One raw sample: margins, compensation with its $SPILL, logicle, PeacoQC, SSI
cytiq spillqc --input sample.fcs --output spillqc/

# A batch: directories and globs, one SSI table set per file plus batch.csv
cytiq spillqc --input tubes/ --input 'plate2/*.fcs' --output spillqc/

# Re-test a candidate matrix (for example from `cytiq autospill`) on the same files
cytiq spillqc --input tubes/ --output spillqc-candidate/ \
  --matrix autospill/spillover.csv --matrix-kind spillover

# Input that is already compensated and transformed (CompensAID's own contract)
cytiq spillqc --input sample.t.fcs --output spillqc/ --preprocessed
```

From Rust:

```rust
use cytiq_spillqc::{SpillQcConfig, SpillQcRunner};

let config = SpillQcConfig::new(vec!["tubes/".into()], "spillqc".into());
let result = SpillQcRunner::run(&config)?;
for file in &result.summary.files {
    for flag in &file.flags {
        println!("{}: {} -> {} SSI {:?}", file.name, flag.primary_marker, flag.secondary_marker, flag.ssi);
    }
}
```

## How a file is assessed

Raw preprocessing refuses files declaring unmixed fluorescence in `$PnTYPE`.

1. **Preprocessing** follows the CompensAID vignette. Skip it with
   `--preprocessed`.
   1. `PeacoQC::RemoveMargins` runs on the raw values of the matrix channels,
      using flowCore's ranges (including the `flowCore_$PnRmin`/`Rmax` keywords).
   2. The matrix is applied to the linearised values: the file's
      `$SPILLOVER`/`$SPILL`, or `--matrix`.
   3. flowCore's `estimateLogicle` (`T = maxRange`, `M = 4.5`, `q = 0.05`)
      and the logicle transform run on every matrix channel.
   4. PeacoQC runs over the matrix channels. Skip it with `--no-peacoqc`, or
      skip the margin step with `--no-margins`.
2. **Marker channels** are those flowCore's `markernames` returns: a `$PnS`
   label, and a name without `time`, `fsc` or `ssc`. When no matrix channel is
   labelled, every matrix channel is used. `--channels`/`--channel` override
   the selection.
3. **CompensAID** runs on every ordered pair (primary P, secondary S):
   1. Each channel gets a density cut-off from `flowDensity::deGate`.
   2. Events negative in S are split into P-negative and P-positive, with a
      separation margin around the P cut-off.
   3. The P-positive events are split into 4 segments of increasing P
      intensity. Segments with fewer than 50 events are merged from the top
      down.
   4. Each segment gets `SSI = (median S of the segment − median S of the
      negatives) / (2 · SD of the negatives)`, rounded to 2 decimals.
4. **The SSI matrix cell** is the SSI of the highest segment.

| Status | Meaning |
| --- | --- |
| `over` | `SSI ≤ −1`: the P-positive events sit below the negatives in S, as when the spillover of P into S is overestimated |
| `under` | `SSI ≥ 1`: residual S signal grows with P. This is under-compensation, or real co-expression below the S cut-off |
| `pass` | `−1 < SSI < 1` |
| `not_assessed` | Fewer than 50 P-negative or P-positive events among the S-negative events |

The thresholds are the ones `CompensAID::PlotMatrix` colours. The vignette
lists `< −1` only.

### Batch view

A cytiq extension: CompensAID assesses one file at a time.

1. Files are grouped by `matrix_id`, a SHA-256 of the matrix channels and the
   compensation values actually applied. In `--preprocessed` mode the id comes
   from the matrix the file still names.
2. A pair is `recurrent_over` or `recurrent_under` when at least
   `--batch-min-fraction` (default 0.5) of at least `--batch-min-files`
   (default 3) assessed files flag it in that direction.

A wrong coefficient affects every file compensated with it, while skew caused
by biology is specific to one sample. A recurrent flag is therefore stronger
evidence than one in a single file. Other statuses are `sporadic`, `pass` and
`insufficient`.

### Known limits

- **Strong under-compensation is invisible.** When a large coefficient is far
  too small, for example halved from 0.22, the P-positive events rise above
  S's first density valley. They leave the S-negative population, and the pair
  is reported `not_assessed`, or is flagged indirectly through other pairs.
- **Errors propagate through the inverse.** One wrong coefficient also skews
  pairs that share a channel with it. Flags cluster around the true error rather
  than pinpointing it.
- **Co-expression looks like under-compensation.** Dim co-expression below the
  S cut-off reads as positive SSI.
- **Small errors go unnoticed on real data.** On a real 11-colour FACSCelesta
  sample, single-coefficient errors of ±5–20% were flagged at most 11% of the
  time, and ±50% about a quarter of the time. Precision was 0.07–0.29, and the
  unchanged sample already drew flags on most pairs with both the lab and the
  AutoSpill matrix. See [`parity/benchmark/`](../../parity/benchmark/README.md).

## Outputs

`--output` is a directory:

| Path | Content |
| --- | --- |
| `summary.json` | Parameters, counts, per-file preprocessing (events, margins, PeacoQC, logicle `T`/`W`, matrix id), cut-offs, flags, batch rows |
| `<name>/ssi_matrix.csv` | CompensAID's matrix: rows are secondary channels, columns primary channels, in cytiq's matrix CSV layout. Empty means not assessed |
| `<name>/ssi_segments.csv` | CompensAID's `matrixInfo` in snake_case: cut-offs, segment edges, counts, MFIs, SD, SSI, message, plus `status` |
| `<name>/cutoffs.csv` | Every `deGate` candidate per channel and the one chosen (closest to the centre) |
| `batch.csv` | One row per matrix group and pair: files, files assessed, over, under, median SSI, status |
| `report.html` | SSI heatmaps, batch flag-share heatmaps, and dot plots of the strongest flags (runs of up to 25 files) |

A file without a usable matrix, without two marker channels, or that fails
logicle or PeacoQC is listed as `skipped` with a reason, and the run goes on.
Unreadable files, input already compensated or transformed by cytiq (without
`--preprocessed`) and invalid arguments end the run. An existing non-empty
output directory needs `--overwrite`, which replaces only what the run writes.
Every output is staged and published together once the run succeeds,
`summary.json` last; a failed run leaves the previous outputs untouched.
`cytiq spillqc --fail-on-flag` exits with 3 (`checks_failed`) after writing
every output when a pair is flagged.

## Public API

| Item | Purpose |
| --- | --- |
| `SpillQcRunner::run` / `input_files`, `SpillQcConfig`, `SpillQcResult`, `SpillQcSummary`, `FileSummary`, `FlagSummary` | The `cytiq spillqc` command and `summary.json` |
| `compensaid`, `CompensaidParameters`, `CompensaidResult`, `PairAssessment`, `SsiSegment`, `SsiStatus`, `MarkerChannel` | CompensAID on in-memory marker columns |
| `summarize`, `BatchSettings`, `BatchRow`, `BatchStatus` | The batch view |
| `PreprocessOptions`, `MatrixRecord`, `LogicleRecord` | Preprocessing switches and records |
| `DEFAULT_SEGMENTS`, `DEFAULT_MIN_EVENTS`, `FLAG_THRESHOLD`, `DEFAULT_BATCH_*` | CompensAID and batch defaults |
| `*_FILE` | Output file names |
| `UPSTREAM` | CompensAID, flowDensity, flowCore and PeacoQC versions the crate is validated against, recorded in provenance |
| `SpillQcCounts::ensure_unflagged` | `--fail-on-flag`: `SpillQcError::Flagged` when a pair was flagged |
| `SpillQcError` | Error type |

## Parity

On CompensAID's own example (`68983.fcs` and the raw `raw.68983.fcs`) and on
synthetic tubes with two injected errors, cytiq reproduces CompensAID's tables:

- every cut-off, segment edge, count, merge message and SSI, with values
  within 1e-14;
- every flag.

On the full vignette chain it keeps the same events as R. See
[`parity/REPORT.md`](../../parity/REPORT.md) §2.11.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for FCS I/O, spillover, linearisation and
  margins
- [`cytiq-compensate`](../cytiq-compensate) for matrix files
- [`cytiq-density`](../cytiq-density) for `density`, `smooth.spline` and
  `deGate`
- [`cytiq-transform`](../cytiq-transform) for logicle and `estimateLogicle`
- [`cytiq-peacoqc`](../cytiq-peacoqc) for the in-memory PeacoQC mask
- `plotly` for the report

## Tests

```bash
cargo test -p cytiq-spillqc
cargo test -p cytiq-engine --test spillqc_cli
cargo test -p cytiq-engine --test parity_smoke spillqc
```
