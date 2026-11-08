# cytiq-clean

Removes debris, doublets and dead cells from one FCS file as per-event masks.
Backs the `cytiq clean` command.

Three independent filters each fit a gate from the data, or report why they
could not:

| Filter | Gate id | Method | Needs |
| --- | --- | --- | --- |
| debris | `non_debris` | FSC-A threshold at the density valley above the low-FSC debris peak; or the AutoSpill FSC/SSC population gate; or fixed thresholds; or off | `FSC-A` |
| singlets | `singlets` | `FSC-A / FSC-H` within `median ± k·MAD` (a wedge polygon), or `FSC-W <= median + k·MAD` | `FSC-H` or `FSC-W` |
| viability | `live` | threshold on the compensated, `fasinh`-scaled dye: density valley, a control-file quantile, or an explicit value | a viability channel |

Events are never copied. Gates are written as Gating-ML 2.0 and evaluated with
[`cytiq-gating`](../cytiq-gating), so they can be edited by hand and re-applied
with `--gates`. The population debris gate is the AutoSpill FSC/SSC auto-gate
(<https://www.nature.com/articles/s41467-021-23126-8>).

## Usage

```bash
cytiq clean --input sample.fcs --output clean/
cytiq clean --input sample.fcs --gates clean/gates.gatingml.xml --output clean-v2/
```

From Rust:

```rust
use cytiq_clean::{CleanConfig, CleanRunner};

let mut config = CleanConfig::new("sample.fcs".into(), "clean".into());
config.channels.no_viability = true;
let result = CleanRunner::run(&config)?;
println!("kept {} of {}", result.metrics.kept, result.metrics.event_count);
```

## Filter configuration

- Viability is auto-detected from `$PnN`/`$PnS` (Live/Dead, L/D, Zombie,
  Viability, Fixable, DAPI, 7-AAD, PI, propidium, SYTOX). Several candidates
  are an error; select `--viability-channel` or pass `--no-viability`.
- The viability axis compensates the dye and applies `fasinh` with
  `T = cofactor · sinh(4.5 ln 10)`, a rescaled `asinh(x / cofactor)`.
  `--viability-cofactor` defaults to 150.
- By default the viability threshold is the valley after the dimmest
  population; no clear valley means `not_assessed`. Use
  `--viability-control unstained.fcs` with `--viability-control-quantile`
  (default 0.995), or `--viability-threshold` in compensated intensity units.
- Adaptive debris detection needs a separate low-FSC peak at most 0.25 times
  the next population's FSC position; otherwise it is `not_assessed`.
  `--debris-mode fixed --debris-fsc-min/--debris-ssc-min` supplies thresholds.
- `--debris-mode population` suits beads and cell lines; its gate around the
  dominant population drops minor populations in mixed samples.
- `--debris-mode off` makes the debris filter `disabled`, so it removes
  nothing. Use it on files another tool already gated: with no debris left, the
  lowest-FSC population is cells (lymphocytes in bone marrow), which the
  adaptive valley can take for debris. It conflicts with `--require debris` and
  with `--gates` (remove `non_debris` from the file instead).
- `--gates` re-applies the ids `non_debris`, `singlets`, `live` and `clean`
  without refitting. A missing gate makes its filter `not_assessed`.

## Public API

| Item | Purpose |
| --- | --- |
| `CleanRunner::run` / `CleanConfig` / `CleanResult` | The `cytiq clean` command |
| `FilterKind` | Filters named by `required` (`--require`) |
| `CleanMetrics`, `FilterReports`, `FilterReport`, `FilterStatus` | Contents of `metrics.json` |
| `CleanMasks` | Per-event filter results (`None` = not assessed) |
| `ChannelOverrides`, `ChannelRoles`, `is_viability_parameter` | Channel selection and viability auto-detection |
| `DebrisConfig`, `DebrisMode` | `adaptive` (default), `population`, `fixed` |
| `ViabilityConfig` | Cofactor, control file, control quantile, explicit threshold |
| `NON_DEBRIS_GATE`, `SINGLETS_GATE`, `LIVE_GATE`, `CLEAN_GATE` | Gate ids looked up by `--gates` |
| `MASKS_FILE`, `METRICS_FILE`, `GATINGML_FILE`, `REPORT_FILE` | Output file names |
| `DEFAULT_*`, `MIN_FIT_EVENTS`, `DEBRIS_MAX_PEAK_POSITION_RATIO`, `VIABILITY_TRANSFORM` | Defaults and limits |
| `CleanError` | Error type for every failure mode |

## Output rules

`output` is a directory (created if needed; existing files are replaced):

```
masks.parquet       event_id, source_event_id, non_debris, singlets, live, keep
metrics.json        per-filter status, method, fitted parameters, counts, overlaps
gates.gatingml.xml  the fitted gates plus `clean` (Boolean AND of the applied ones)
report.html         each gate drawn on its own dimensions (Plotly)
```

- `event_id` is the 0-based row index of the input FCS; `source_event_id` is
  the raw-file row (from `Original_ID`, otherwise equal to `event_id`).
- With `masks` (CLI `--mask`), filters are fitted only on events every mask
  keeps and `keep` is false for the others; filter columns are still evaluated
  on every event. `metrics.json` then has an `input_mask` section.
- A filter is `applied`, `not_assessed` or `disabled`, with a `reason`. The mask
  column of a filter that is not `applied` is null, not all-true. `keep` is the
  AND of the applied filters only.
- `required` (CLI `--require`) blocks acceptance when a listed filter is not
  `applied`. Masks, metrics, gates, HTML and provenance are retained atomically;
  the CLI records a completed `checks_failed` assessment and exits with 3.
  Read/compute/write errors still publish no new result.
- Singlet bands use `cytiq_density::robust_spread`, so a zero MAD falls back
  to `IQR / 1.349` or a relative spread (`spread_source` in `metrics.json`).
- A viability or scatter channel transformed by `cytiq transform` is rejected
  up front (`TransformedViability`, `TransformedScatter`).
- Without a viability dye, viability is `not_assessed`; it never marks every
  event as live.
- Fitting uses at most `max_fit_events` (default 50,000) evenly spaced events,
  so runs are deterministic. Singlets are fitted on non-debris events, viability
  on non-debris singlets; every gate is then evaluated on all events.
- Masks come from evaluating the exported Gating-ML, so `cytiq gate` on
  `gates.gatingml.xml` reproduces the counts exactly.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for events, [`cytiq-gating`](../cytiq-gating)
  for gate evaluation and Gating-ML I/O
- [`cytiq-density`](../cytiq-density) for KDE, valleys, median/MAD and the
  FSC/SSC population gate
- [`cytiq-mask`](../cytiq-mask) for reading and writing masks, `plotly` for
  the report, `rayon`

## Tests

```bash
cargo test -p cytiq-clean
cargo test -p cytiq-engine --test clean_cli
```

`clean_cli.rs` runs against `data/fixtures/clean/` (generated by
`parity/gen/clean.py`, with per-event ground truth in `truth.json`).
