# Reference dataset

The reference dataset of the [dataset contract](../../../docs/dataset-contract.md):
`cytiq.events.v1` tables and a dataset manifest v1, built from existing FCS
fixtures with the real commands. No FCS file is added here.

| Path | What it is |
| --- | --- |
| `run.yaml` | Run manifest (schema 3) with MIFlowCyt metadata: two instruments, two specimens, three panels |
| `reference/manifest.json` | The dataset manifest, canonical JSON, `deidentified` privacy profile |
| `reference/events/M/` | `../flowcyt/Case12_P.fcs` (Kaluza, Navios) as raw, compensated (`cytiq compensate`, the file's own `$SPILLOVER`) and transformed (`cytiq transform`, arcsinh 150) layers |
| `reference/masks/M/` | The raw detector-margin mask `cytiq compensate` wrote (`cytiq.mask.v2`) |
| `reference/events/B/` | `../flowcyt/Case30_P.fcs` (flowCore re-export of compensated values) as an externally compensated layer |
| `reference/events/X/` | `../fcs/fcs3_2_mixed.fcs` (synthetic): integer, float and double channels, FCS 3.2 `$PnDET`, `$PnTYPE`, `$PnANALYTE`, `$PnFEATURE` |
| `reference/definitions/gates.xml` | `../specimen/gates.xml`, a Gating-ML artifact |
| `case12-gates.xml`, `reference/definitions/case12-gates.xml` | Gates on panel M's transformed layer: nested (`blasts` in `CD45+`), overlapping (`CD45+` and `CD34+` share event 2) and empty (`large`) |
| `reference/labels/M/` | `cytiq.events.labels.v1` tables written by `cytiq export labels`: the margin mask as `qc` labels, and `cytiq gate --membership` of `case12-gates.xml` within that mask as `gate` labels |
| `malformed/*.json` | One or more cases per validation code: a JSON patch of `reference/manifest.json`, the expected code, the tier (`document` or `objects`) and an optional profile |
| `sha256.json` | SHA-256 of every file here except this README and the lock |

The manifest also references `../specimen/tree.json` and
`../specimen/flowcyt-bm.projection.json` by their canonical cytiq-tree digests.

The FlowCyt files are CC-BY-4.0 (see [`../flowcyt/README.md`](../flowcyt/README.md)).

The manifest's `design` puts specimen SP01 (panels M and B) under subject
P01 in group `case` and the synthetic SP02 under P02 in group `control`,
both at timepoint `D0`, with batches per sample.

Panels M and B both belong to specimen SP01 and both hold `source_event_id`
0, 1, 2…. That pairing is **synthetic**: Case12 and Case30 are different
FlowCyt cases. It makes the reference both multi-panel and a
cross-acquisition test, in which equal row numbers of different acquisitions
must never be the same event. The reporter and detector of panel X are
declared, since the synthetic file has none.

Checks:
- `crates/cytiq-population/tests/dataset_fixtures.rs` verifies the lock,
  validates the reference under both profiles with every object verified,
  checks schema conformance and the absence of identifying text, and replays
  every malformed case.
- `crates/cytiq-engine/tests/dataset_fixtures.rs` rebuilds the reference with
  the commands and requires the same semantic ids, recipes, values digests
  and dataset id. `cytiq transform` writes FCS bytes that differ between
  builds, so a rebuild changes the transformed table's
  `cytiq.events.source_sha256` (and its bytes) but no identity; the gate
  labels record that input by its layer id, which is path-free.

To regenerate after an intended change:

```sh
CYTIQ_BLESS=1 cargo test -p cytiq-engine --test dataset_fixtures
CYTIQ_BLESS=1 cargo test -p cytiq-population --test dataset_fixtures
```

Explain every changed identity in the pull request. Never regenerate to make
a change pass.
