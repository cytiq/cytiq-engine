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

## MIFlowCyt metadata (`schema_version: 3`)

Version 3 adds optional descriptive metadata after MIFlowCyt. Four
identities stay separate:
- a **channel** is an FCS parameter (`$PnN`);
- a **detector** is the instrument's detector (`$PnDET`);
- a **marker** is the analyte (`$PnS`, `$PnANALYTE`);
- a **reporter** is the fluorochrome or metal tag that carries it (`$PnTAG`).

```yaml
schema_version: 3
cytometry: conventional
experiment: { title: T and B phenotyping, purpose: Method comparison, keywords: [lymphocytes] }
instruments:
  - id: canto-1
    manufacturer: BD Biosciences
    model: FACSCanto II
    detectors:
      - { id: B530, kind: pmt, excitation_nm: 488, emission_filter: 530/30 }
specimens:
  - { id: SP01, organism: Homo sapiens, material: peripheral blood,
      ontology: [{ id: "UBERON:0000178", label: blood }] }
panels:
  - name: T
    instrument: canto-1
    markers:
      - { marker: CD3, channel: FITC-A, reporter: FITC, detector: B530,
          reagent: { clone: SK7, vendor: BD Biosciences, catalog: "345763" } }
samples:
  - { id: SP01-T, file: sp01_t.fcs, panel: T, specimen: SP01 }
controls:
  - { file: unstained.fcs, dye: Autofluorescence, role: unstained }
  - { file: cd3_fitc.fcs, dye: FITC, panel: T, role: single_stain }
```

Specimen fields describe biology only (organism, material, preparation,
ontology terms). Specimen and sample ids must be study pseudonyms, never
donor or patient identifiers. Which fields a dataset must fill in is decided
by the dataset validation profile (`cytiq dataset validate --profile`), not
by loading. `RunManifest::migrate` raises an earlier document to
`SCHEMA_VERSION` and validates it under the current rules.

## Public API

| Item | Purpose |
| --- | --- |
| `RunManifest::load` | Read (at most `MANIFEST_BYTES_MAX`, 16 MiB, as UTF-8), parse (by extension) and validate; resolves relative paths (FCS files and matrices) against the manifest's directory |
| `RunManifest`, `Cytometry`, `Panel`, `Marker`, `SampleEntry`, `ControlEntry`, `CompensationSource`, `MatrixKind`, `SignalState` | The document; a single `panel` is read as one entry of `panels` |
| `Experiment`, `Instrument`, `Detector`, `DetectorKind`, `Specimen`, `OntologyTerm`, `Reagent`, `ControlRole` | `schema_version: 3` metadata |
| `SCHEMA_VERSION`, `SUPPORTED_SCHEMA_VERSIONS`, `RunManifest::migrate` | Versions read (1–3) and migration to the current one |
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
- `cytometry: cytof` requires v2 or later, raw ion counts and no compensation source.
- v3 fields in an earlier version are errors. In v3, instrument, detector
  and specimen ids are unique; `panel.instrument` and `marker.detector`
  resolve; reporters and channels are unique within a panel (ignoring case);
  declared specimens cover every sample's `specimen`; ontology ids are
  `PREFIX:LOCAL` CURIEs; `role: unstained` matches an unstained `dye`, and
  a single-stain control's `dye` is a reporter of its panel when the panel
  declares reporters.
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
