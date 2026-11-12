# cytiq-transform

Inputs carrying `CYTIQ_NORMALIZATION` are rejected, including when
`--allow-retransform` is set: retransforming them would invalidate the pinned
normalization contract. Normalize only after the final signal transform.

Applies analysis-ready signal transformations (arcsinh, logicle, hyperlog,
FlowJo biexponential) to the channels of one or more FCS files and writes new
FCS 3.1 files. Backs the `cytiq transform` command.

The `scales` module also exports the reference display scales used by
[`cytiq-gating`](../cytiq-gating) (`Logicle`, `Hyperlog`) and
[`cytiq-autospill`](../cytiq-autospill) (`FlowJoBiexp`).

## Methods

| `TransformMethod` | Definition | Output scale |
| --- | --- | --- |
| `Arcsinh` (default) | `asinh(value / cofactor)`, default cofactor 150 (flowCore `arcsinhTransform`) | unbounded |
| `Logicle` | Moore & Parks (2012) logicle, as `flowCore::logicleTransform` | `[0, M]` |
| `Hyperlog` | Gating-ML 2.0 hyperlog, as `flowCore::hyperlogtGml2` / FlowKit | `[0, 1]` |
| `Biexponential` | `flowWorkspace::flowjo_biexp(256, 262144, 4.418539922, 0, -100)`: cytolib's calibration table with R's natural cubic spline | `[0, 256]` |
| `None` | identity (channels are copied) | — |

Logicle and hyperlog share `LogicleParameters` (`t`, `m`, `w`, `a`, defaults
`262144`, `4.5`, `0.5`, `0`) and reject values outside `2W <= M` and
`-W <= A <= M - 2W`.

`cytiq transform --help` lists every flag and its default.
Parity against flowCore / flowWorkspace / FlowKit:
[`parity/REPORT.md`](../../parity/REPORT.md) section 2.1.

## Usage

```bash
cytiq transform --input sample.fcs --output sample.transformed.fcs
cytiq transform --input "samples/*.fcs" --method logicle --output transformed/
```

From Rust:

```rust
use cytiq_transform::{TransformConfig, TransformMethod, TransformRunner, parse_channel_list};

let mut config = TransformConfig::new(vec!["sample.fcs".into()], "out.fcs".into());
config.method = TransformMethod::Logicle;
config.channels = parse_channel_list("CD3,CD4,CD8")?;
let result = TransformRunner::run(&config)?;

// Scales on their own:
use cytiq_transform::scales::Logicle;
let logicle = Logicle::new(262_144.0, 0.5, 4.5, 0.0)?; // t, w, m, a
let y = logicle.scale(1_000.0); // [0, 1] Gating-ML scale
```

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `TransformRunner::run` / `TransformConfig` | The `cytiq transform` command: resolve inputs, transform, write FCS + sidecars |
| `TransformMethod`, `LogicleParameters`, `BiexponentialParameters`, `TransformParameters`, `TransformFormat` | Method selection and parameters |
| `TransformRunResult`, `TransformFileResult`, `TransformChannelMetrics`, `TransformValueMetrics` | Per-file plan and per-channel raw/transformed statistics |
| `parse_channel_list` | Parse a comma-separated channel list |
| `scales::{Logicle, Hyperlog, FlowJoBiexp, ScaleParameterError}` | Reference scales with `scale` / `inverse` |
| `ValueTransform::from_contract` / `apply` | The forward per-value transform of a written `CYTIQ_TRANSFORM` keyword (`cytiq_fcs::TransformContract`), with the command's exact semantics; `cytiq plot` uses it to place raw decades on transformed axes |
| `scales::estimate_logicle_width` | flowCore `estimateLogicle`'s `w` for one channel (`T = maxRange`, `M`, `q`); used by `cytiq spillqc` |
| `TransformError` | Error type for every failure mode |

## Inputs, channels and outputs

- Inputs: `.fcs` files, directories (immediate `.fcs` children) and simple
  file-name globs such as `samples/*.fcs`.
- One input writes to `output`; several inputs, or an existing directory as
  `output`, write `<stem><suffix>.fcs` (default suffix `.transformed`) into it.
- Without `channels`, every channel except obvious acquisition channels
  (`Time`, event/cell length and light scatter: `FSC…`/`SSC…`, a Navios's
  `FS INT`/`SS PEAK`/`SS INT`/`SS TOF`, see `cytiq_fcs::is_scatter_channel`)
  and `Original_ID` is transformed. Naming `Original_ID` in `channels` is an error
  (`ReservedChannel`): it is event lineage, not a measurement.
  Channels match `$PnN`, then `$PnS`, then `$PnN [$PnS]`.
- Only `TransformFormat::Fcs` is implemented; `Csv` and `Parquet` are rejected.
- Transformed values are rounded to `f32`; untransformed columns keep their
  exact values, so the FCS 3.1 output uses doubles when one of them (such as
  `Original_ID` above 2^24) does not fit `f32`. The output carries the source
  keywords, `CYTIQ_TRANSFORM` (the transform metadata JSON) and
  `CYTIQ_ACQUISITION`.
- The output keeps acquisition keywords and the original `$PnR` of unchanged
  channels; transformed channels get a `$PnR` from their new values. The source
  spillover matrix moves to `CYTIQ_SOURCE_SPILL`. Detector margin bounds are
  mapped through the transform into `CYTIQ_MARGINS`, so `peacoqc --remove-margins`
  can remove the same events as on the raw file.
- `TransformRunner::input_files` resolves the inputs a run would read.
- Compensation comes first: a selected channel in the file's pending
  `$SPILLOVER`/`$SPILL` matrix is `UncompensatedInput` unless
  `allow_uncompensated` (`--allow-uncompensated`) is set. An input with
  `CYTIQ_TRANSFORM` is `AlreadyTransformed` unless `allow_retransform`.
- Existing outputs and the `metadata_json` sidecar are errors unless
  `overwrite` is set. Every output, including one that replaces its own input,
  is staged as a synced temporary file beside it and renamed into place
  (`cytiq-publish`).
- Non-finite input values in a selected channel, or non-finite outputs, fail
  the run.
- `dry_run` resolves inputs, channels and outputs without writing anything.
- `debug_dir` gets `summary.json`, `metrics.csv` and a Plotly `report.html`.
- Files and columns are processed in parallel on the caller's rayon pool;
  `cytiq transform --threads` sizes it.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for reading and writing FCS
- `rayon` for parallelism, `plotly` for the debug report, `serde_json` / `csv`
  for sidecars

## Tests

```bash
cargo test -p cytiq-transform
cargo test -p cytiq-engine --test transform_cli
```
