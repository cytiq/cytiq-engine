# cytiq-flowsom

Deterministic FlowSOM-style clustering of FCS events, including persisted models for cohort inference. Backs the
`cytiq flowsom` command.

- Paper: <https://doi.org/10.1002/cyto.a.22625>
- Upstream R reference: <https://github.com/saeyslab/FlowSOM> (parity target
  FlowSOM 2.18.0, checked against `src/som.c`)

## Pipeline

For persisted models, use:

```bash
cytiq flowsom train --input control1.t.fcs --input control2.t.fcs \
  --channels CD3,CD4,CD8 --output som.json --metaclustering consensus
cytiq flowsom apply --input sample.t.fcs --model som.json --output assignments.csv
```

`FlowSomModel::{train,train_files,train_to_file,assign,populations,save,load,apply_file}`
exposes row-major training, inference and population statistics. Model schema 1
stores channel order and reagent labels, grid, codebook, training mean/SD, node-to-metacluster labels,
seed, training count and preprocessing contract. Assign returns one-based
`(node, metacluster)` pairs and never rescales a new sample independently.
Models are JSON and validated after loading. File workflows check transform
settings and any upstream normalization ID. Training masks currently require a
single input acquisition; CytoNorm manifests support per-file masks for cohorts.
Existing outputs require `--overwrite` in the new model commands.

Model JSON documents are limited to 16 MiB before deserialization. Selected
event values are limited to 8 GiB before row-matrix allocation. Channel values
are read from the original typed FCS columns, avoiding full converted-column
copies; training uses a borrowed full matrix when no training subset is needed.
The selected rows, RNG schedule, numeric casts and scaling are unchanged.

`Metaclustering::Consensus` uses 100 deterministic 90% node subsamples, all
features, Euclidean distance and average linkage inside and after resampling.
Its RNG stream is independent of SOM training. This mode does not reproduce R's
RNG sequence. The legacy command and `FlowSomRunner::run` retain their previous
defaults, assignment schema and behavior.

1. Read one FCS file and select channels (`$PnN`, then `$PnS`, then
   `"$PnN [$PnS]"`). Values are used as stored: no compensation or transform
   is applied here, so run `cytiq transform` first when needed.
2. Scale each channel to mean 0 / SD 1 (`base::scale`, n − 1).
3. Train a 10×10 SOM as `FlowSOM::SOM`: codes initialised from distinct events,
   `10 · n` random draws with replacement, linear learning rate 0.05 → 0.01,
   bubble neighbourhood on the Chebyshev grid distance with radius from the
   0.67 quantile of grid distances (6) down to 0; below 1 only the BMU is
   updated.
4. Assign every event to its nearest node.
5. Merge the 100 nodes into 20 metaclusters by average-linkage hierarchical
   clustering of the codebook.

Defaults come from the `FLOWSOM_*` constants. Deliberate differences from R
defaults: `scale = TRUE`, 20 metaclusters (R: 10), and plain average linkage
instead of ConsensusClusterPlus. After the parity fixes Rust-vs-R ARI is
0.93–0.98, the same as R seed-to-seed; see
[`parity/REPORT.md`](../../parity/REPORT.md) §2.5.

## Usage

```bash
cytiq flowsom --input sample.fcs --output sample.flowsom.csv --channels CD3,CD4,CD8
cytiq flowsom --input sample.fcs --output sample.flowsom.csv \
  --channel CD3 --channel CD4 --debug-dir ./flowsom-debug --threads 4
cytiq flowsom --input big.fcs --output big.flowsom.csv --channels CD3,CD4,CD8 \
  --xdim 12 --ydim 12 --metaclusters 25 --training-events 200000
```

From Rust:

```rust
use cytiq_fcs::parse_channel_list;
use cytiq_flowsom::{FlowSomConfig, FlowSomRunner};

let mut config = FlowSomConfig::new(parse_channel_list("CD3,CD4,CD8")?);
config.debug_dir = Some("./flowsom-debug".into());
let result = FlowSomRunner::run("sample.fcs", "sample.flowsom.csv", &config)?;
println!("{} metaclusters", result.summary.metrics.metaclusters);
```

`--xdim`/`--ydim` set the grid (default 10×10), `--metaclusters` the cluster
count (default 20, at most `xdim·ydim`), `--seed` the seed (default 1) and
`--no-scale` disables z-scoring. `--training-events N` draws a deterministic
sample without replacement from its own RNG stream for training, then assigns
every event; scaling statistics still use every event. Training iterations,
learning rate and radius schedules remain fixed constants.

## Public API

| Item | Purpose |
| --- | --- |
| `UPSTREAM` | Upstream implementations and versions the crate is validated against (`parity/versions.txt`), recorded in provenance `algorithms` |
| `FlowSomConstants` | The fixed training schedule, recorded in provenance `constants` |
| `FlowSomRunner::run(input, output, &FlowSomConfig)` | Run the pipeline and write the assignment CSV |
| `FlowSomConfig` | `channels`, optional `debug_dir`, optional `threads`, `masks`, `xdim`, `ydim`, `metaclusters`, `seed`, `scale`, `training_events` (defaults from `FLOWSOM_*`) |
| `FlowSomResult` | `summary`, per-event `assignments`, `nodes`, `metaclusters` |
| `FlowSomSummary`, `FlowSomParameters`, `FlowSomMetrics`, `FlowSomScaleSummary` | Run summary, written as `summary.json` |
| `FlowSomAssignment`, `FlowSomNode`, `FlowSomMetacluster` | Output rows |
| `FLOWSOM_*` constants | Defaults for grid size, metaclusters, seed (1) and scaling; fixed iterations, learning rate and radius schedule |
| `FlowSomError` | Error type for every failure mode |

## Output rules

- Selected channels that `cytiq transform` did not transform are clustered as
  given, with a logged warning.
- `output` is a CSV with one row per clustered event: `event_index` (0-based
  row in the input file), `source_event_id` (0-based row in the raw file, from
  `Original_ID` when present), `flowsom_node` (1..=100) and
  `flowsom_metacluster` (1..=20). The file is created or replaced.
- With `masks` (CLI `--mask`), only events every mask keeps are scaled,
  trained on, assigned and listed. A mask that keeps nothing is an error.
- With `debug_dir`: `summary.json`, `nodes.csv` (grid position, metacluster,
  event count, codebook), `metaclusters.csv` and a Plotly `report.html`.
- Join results from different stages on `source_event_id`, not on
  `event_index`, which counts rows of this input only.
- Empty inputs, missing channels and `threads = Some(0)` are errors.

## Dependencies

- [`cytiq-fcs`](../cytiq-fcs) for reading and event lineage
- [`cytiq-mask`](../cytiq-mask) for `--mask`
- `rayon`, `csv`, `serde` / `serde_json`, `plotly`

## Tests

```bash
cargo test -p cytiq-flowsom
cargo test -p cytiq-engine --test flowsom_cli
```
