# cytiq-gating

Evaluates Gating-ML 2.0 gating strategies on FCS events. Backs the
`cytiq gate` command and is the gate engine behind `cytiq-clean`.

The crate applies gate boundaries that are already defined; it does not
estimate gates from the data. Results are checked against
[FlowKit](https://github.com/whitews/FlowKit) (`GatingStrategy.gate_sample`).
Specification: <http://flowcyt.sourceforge.net/gating/latest.pdf>.

## What is supported

- gates: `RectangleGate` (range, rectangle, hyper-rectangle), `PolygonGate`,
  `EllipsoidGate`, `BooleanGate` (`and`, `or`, `not`, `use-as-complement`),
  nested through `parent_id`
- transformations: `flin`, `flog`, `fasinh`, `logicle`, `hyperlog`
  (`logicle`/`hyperlog` come from `cytiq-transform::scales`)
- compensation: `uncompensated`, `FCS` (`$SPILLOVER`/`$SPILL`), or a square
  `transforms:spectrumMatrix`
- rejected with an error: quadrant gates, `fratio`, `new-dimension`

Membership semantics follow FlowKit/flowutils exactly: rectangles use
`min <= x < max`, polygons use the winding number with odd-winding interiors,
ellipsoids include their boundary, and every gate is intersected with its
parent. Events are pre-processed as FlowKit "raw" events (`$PnE`, `$PnG`,
`$TIMESTEP`) before compensation.

Dimension names match `$PnN`, then `$PnS`, then the FlowJo spelling of `$PnN`
(`/` replaced with `_`), then a fluorochrome of the referenced spectrum matrix.
An `FCS` compensation reference is treated as uncompensated when the file has
no spillover keyword. Gate ids must be valid XML `ID`s: use `CD3pos`, not `CD3+`.

## Usage

```bash
cytiq gate --input sample.fcs --gating-ml gates.xml --output counts.csv --format csv
cytiq gate --input sample.fcs --gating-ml gates.xml --output counts.csv \
  --mask sample.analysis.parquet --membership membership.parquet
```

From Rust:

```rust
use cytiq_gating::{parse_gatingml, to_gatingml};

let strategy = parse_gatingml(&std::fs::read_to_string("gates.xml")?)?;
let dataset = cytiq_fcs::FcsReader::open("sample.fcs")?.read()?;
let results = strategy.gate_sample(&dataset)?;
let mask: &[bool] = results.membership("CD3pos").unwrap();
let report = results.report(); // Vec<GateCount>, in evaluation order

// Strategies built in code can be written back out.
let xml = to_gatingml(&strategy)?;
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `parse_gatingml` | Parse a Gating-ML 2.0 document into a `GatingStrategy` |
| `to_gatingml` | Serialize a strategy back to Gating-ML (deterministic, round-trips) |
| `GatingStrategy` | Gates in evaluation order; `new`, `gate_sample`, `gate_sample_masked`, `dimension_values`, `gate_path` |
| `GatingResults` / `GateCount` | Per-gate membership masks, counts and percentages; `event_mask` |
| `Gate`, `GateKind`, `GateRef`, `BooleanOp`, `Dimension`, `CompensationRef`, `SpectrumMatrix` | Strategy model |
| `Transform` | Gating-ML parametrized transformations |
| `GatingRunner::run` / `GatingConfig` / `GatingRunResult` / `OutputFormat` | The `cytiq gate` command |
| `GatingError` | Error type for every failure mode |

## Output rules

- `--format csv` (default) or `json`; one row per gate in evaluation order with
  `gate_id`, `gate_path`, `parent_id`, `gate_type`, `count`, `parent_count`,
  `percent_of_parent`, `percent_of_total`.
- The output's parent directory is created if needed. An existing output or
  membership file is refused before any input is read unless `overwrite` is
  set (`--overwrite`); then both are replaced together.
- `masks` (CLI `--mask`) restricts gating to events every mask keeps; events
  outside belong to no gate and `percent_of_total` uses the masked count.
- `membership` writes a Parquet file in the [`cytiq-mask`](../cytiq-mask)
  layout: one nullable Boolean column per gate id (null outside the event
  mask) and `keep` marking events inside it. Gate ids `event_id`,
  `source_event_id` and `keep` are rejected there.
- Files from `cytiq transform`, `cytiq compensate` or `cytiq autospectral`
  are refused when a dimension would transform or compensate an already
  processed channel.
- Gates are evaluated on the caller's rayon pool; `cytiq gate --threads` sizes it.
- `to_gatingml` cannot write `logicle` or `hyperlog` transformations, because
  their original parameters are not kept in memory.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for events, `$SPILLOVER`, linearisation and signal state
- [`cytiq-mask`](../cytiq-mask) for `--mask` and `--membership`
- [`cytiq-transform`](../cytiq-transform) for `Logicle` / `Hyperlog`
- `roxmltree` for XML, `rayon` for column-parallel transforms, `csv` / `serde_json` for reports

## Tests

```bash
cargo test -p cytiq-gating
cargo test -p cytiq-engine --test gate_cli
```

`tests/flowkit_parity.rs` compares per-event membership for every gate against
FlowKit results in `data/fixtures/gating/` (see its README to regenerate). Parity
numbers are in [`parity/REPORT.md`](../../parity/REPORT.md) (section `gate`).
