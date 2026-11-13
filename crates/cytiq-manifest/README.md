# cytiq-manifest

The run manifest: one YAML (`.yaml`/`.yml`) or JSON (`.json`) file that
describes an acquisition run. Read by `cytiq preflight` and `cytiq qc`.

```yaml
cytometry: conventional        # or spectral
panel:
  name: T-cell-8c
  markers:
    - { marker: CD3, channel: FL1-A }        # channel optional: matched by $PnS
    - { marker: Live/Dead, required: false }  # required defaults to true
samples:
  - { id: S01, file: fcs/s01.fcs }
controls:                                     # optional
  - { file: fcs/unstained.fcs, dye: Autofluorescence }
  - { file: fcs/cd3_fitc.fcs, dye: FITC, primary_channel: FL1-A }
```

Several panels go under `panels` (each named); samples name their `panel`,
controls may (a control without `panel` belongs to every panel), and
`specimen` groups one biological sample's files across panels. A
conventional panel says where its compensation comes from:

```yaml
panels:
  - name: T
    markers: [{ marker: CD3 }]
    compensation: { source: matrix, matrix: autospill/t.csv, kind: compensation }
  - name: B
    markers: [{ marker: CD19 }]
    compensation: { source: controls }      # default: { source: fcs }
samples:
  - { id: S01-T, file: s01_t.fcs, panel: T, specimen: S01 }
  - { id: S01-B, file: s01_b.fcs, panel: B, specimen: S01 }
```

A panel whose sample files other software already processed declares it with
`signal_state`: `raw` (default), `compensated` (conventional) or `unmixed`
(spectral). cytiq stages only recognise their own `CYTIQ_*` keywords, so this
is the only way they learn about processing done elsewhere.

## Public API

| Item | Purpose |
| --- | --- |
| `RunManifest::load` | Read (at most `MANIFEST_BYTES_MAX`, 16 MiB, as UTF-8), parse (by extension) and validate; resolves relative paths (FCS files and matrices) against the manifest's directory |
| `RunManifest`, `Cytometry`, `Panel`, `Marker`, `SampleEntry`, `ControlEntry`, `CompensationSource`, `MatrixKind`, `SignalState` | The document; a single `panel` is read as one entry of `panels` |
| `RunManifest::panel_of`, `controls_of`, `compensation_of` | A sample's panel, a panel's controls, the compensation source of a conventional panel of raw samples (`fcs` by default) |
| `Panel::label` | `name`, or `panel` when unnamed |
| `QcPolicy`, `FilterPolicy`, `AssessedOf`, `Severity` | A panel's `qc` section, checked by [`cytiq qc`](../cytiq-qc) |
| `ControlEntry::is_unstained` | `dye` is `Autofluorescence`, `AF` or `unstained` |
| `ManifestError` | Error type |

## Rules

- Unknown fields are errors, with the line and column for YAML.
- Exactly one of `panel` and `panels`.
- At least one sample, one panel and one marker per panel; sample ids, panel
  names and a panel's marker names are unique (marker names ignoring case);
  file paths are not empty.
- With several panels, every panel has a name and every sample names an
  existing panel; a control's `panel`, when given, exists.
- Omitted `schema_version` means v1, which permits at most one file per specimen/panel.
  Version 2 permits repeats with unique acquisition/sample IDs; specimen planning
  requires explicit selection when there is more than one candidate.
- `cytometry: cytof` requires v2, raw ion counts and no compensation source.
- `compensation` is for conventional runs only; `source: controls` needs
  single-stain controls in the panel.
- Every spectral panel of raw samples needs an unstained control.
- `signal_state: compensated` is for conventional panels and excludes
  `compensation`; `unmixed` is for spectral panels.
- QC fractions (`max_removed_fraction`, `min_assessed_fraction`) lie
  between 0 and 1; filter names are not empty; `assessed_of` other than
  `raw` comes with `min_assessed_fraction`.
- Files themselves are not opened here; `cytiq preflight` checks them.

## Dependencies

`serde`, `serde-saphyr` (YAML), `serde_json`, `thiserror`.

## Tests

```bash
cargo test -p cytiq-manifest
cargo test -p cytiq-engine --test preflight_cli
```
