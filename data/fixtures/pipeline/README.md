# Pipeline Fixture

Golden data for the end-to-end test of the whole cytiq chain
(`crates/cytiq-engine/tests/pipeline_cli.rs`). Every event's truth is known.

- `sample.fcs`: 15,000 raw events over `FSC-A/H/W`, `SSC-A`, `FL1-A`…`FL6-A`
  (CD3, CD4, CD8, CD19, CD56, Live/Dead) and `Time`, with `$SPILLOVER` (6×6),
  `$CYT` and `$DATE`. Live singlets are CD4 T, CD8 T, double-negative T, B, NK,
  a rare CD19⁺CD56⁺ population (0.3%) and monocytes. Artefacts: debris (8%),
  doublets (6%), dead cells (10%), an unstable acquisition interval (rows
  6000–6899: a clog then a gain drift) and detector saturation (0.8%, one
  fluorescence channel at `$PnR - 1`). Fluorescence is generated compensated,
  mixed with the spillover matrix and stored raw.
- `truth/events.csv`: `raw_row, population, debris, doublet, dead, unstable,
  margin` per event; `good` events have no flag. `truth/summary.json` has the
  counts; `truth/spillover.csv` the matrix.
- `gates.xml`: population gates for `cytiq transform` output (asinh, cofactor
  150) with `uncompensated` dimensions. `gates_raw.xml`: the same gates on the
  raw file, compensated through `FCS` with the equivalent `fasinh`, as FlowKit
  applies them.
- `run.yaml`: the run manifest for `cytiq preflight`, with the `qc` policy
  `cytiq qc` checks the final mask against.
- `thresholds.json`: acceptance thresholds of the end-to-end test.

## Measured on this fixture

Chain: `compensate → transform → peacoqc (FL1–FL5) → clean (raw) → mask
combine → gate --mask → flowsom --mask`.

| Stage | Result |
| --- | --- |
| margins (`compensate` mask) | all 117 saturated events found and nothing else removed: no good events lost. flowCore's −111 floor, which the mask applied before, removed 0.7% of good events here |
| PeacoQC | every unstable event removed; 9.6% of good events removed around the artefact (rows 5250–7499). Upstream R PeacoQC 1.20.0 removes exactly the same 2,250 events: at 15,000 events the bins are coarse |
| clean | debris recall 0.998, doublet 1.0, dead 0.996; 0.15% of good events lost |
| final mask | 4 of 4,181 bad events kept; F1 against `good` 0.949 |
| original-population retention | good-event retention 0.892857–0.907254 across populations; rare events 27/30 = 0.9. The dataset-specific floor is frozen at 0.88, below the measured minimum. Gates evaluated only on survivors would miss this loss. |
| gates | precision 1.0, recall ≥ 0.998 for every population; Rare 27 of 27 |
| FlowSOM (`--metaclusters 7`) | ARI 0.996 against the true populations; the rare population is a pure SOM node and, with the default 20 metaclusters, a metacluster of purity 0.9 |

Regenerate (deterministic, byte-identical) with:

```bash
parity/.env/py/bin/python parity/gen/pipeline.py data/fixtures/pipeline ci
```
