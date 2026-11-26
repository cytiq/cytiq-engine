# Preflight checks

Checks a run manifest and its FCS files before any computation. Backs
`cytiq preflight`.

```bash
cytiq preflight --manifest run.yaml --output preflight.json
```

The report is always written. The command exits with `3` when any check
fails; failures and warnings are also logged.

Manifest v2 also accepts `cytometry: cytof`. These panels require raw ion counts
and no compensation. Preflight checks FCS, markers, linearity and acquisition
metadata without assigning fluorescence spillover or spectral control coverage.

## Checks

| Check | Scope | Fails when | Warns when |
| --- | --- | --- | --- |
| `fcs_readable` | every file | the FCS cannot be parsed | |
| `events` | every file | the file has no events | |
| `raw_signal` | every file, by its panel's `signal_state`; controls, always as `raw` | the processing cytiq recorded (`CYTIQ_*` keywords) does not fit the declared state: any for `raw`; anything but `cytiq compensate` for `compensated`, anything but `cytiq autospectral` for `unmixed`; `cytiq transform` always; or a `CYTIQ_*` keyword cannot be read | |
| `measurement_types` | every file, by its panel's `signal_state`; controls, always as `raw` | the file's `$PnTYPE` (FCS 3.2) declares both `Raw Fluorescence` and `Unmixed Fluorescence` channels, or `Unmixed Fluorescence` channels in a `raw` panel that `cytiq autospectral` did not unmix: `cytiq export events` refuses them as a raw layer | a `$PnTYPE` value FCS 3.2 does not define, which declares nothing |
| `acquisition_metadata` | every file | | `$CYT` or `$DATE` is missing |
| `repeated_keywords` | every file | | TEXT repeats a keyword without `$` that cytiq does not read (case ignored): cytiq reads none of its values, and no file it writes keeps it |
| `signal_state` | samples, by their panel's `signal_state`; controls, always as `raw` | `raw`: `Comp-<detector>` channels without the detector; `compensated`: the file carries `$SPILLOVER`/`$SPILL`, which stages would apply again | `raw`: `Comp-<detector>` channels next to their detectors; `unmixed`: the file carries a spillover keyword |
| `linear_scale` | every file | a control, or a sample `cytiq compensate` does not process (spectral, or declared compensated), has a log-amplified parameter (`$PnE` other than `0,0`): the stages that read it refuse it | a raw sample of a conventional panel has one: `cytiq compensate` writes it linear, the other stages refuse it raw |
| `panel_marker` | samples | a required marker matches no channel, or several | an optional marker is missing; `channel`'s `$PnS` names another marker |
| `unique_labels` | samples | | two channels share a `$PnS` |
| `spillover` | raw samples of a conventional panel with `source: fcs` (the default) | no, invalid, mismatched or singular spillover matrix | its 1-norm condition number exceeds `SPILLOVER_CONDITION_WARN` (100) |
| `compensation_matrix` | conventional panel with `source: matrix` | the file cannot be read, is not square or is singular | its 1-norm condition number exceeds `SPILLOVER_CONDITION_WARN` |
| `compensation_channels` | samples of that panel | a matrix channel is not in the file | |
| `control_coverage` | conventional panel with `source: controls`; spectral panels of raw samples | `source: controls` and a marker channel has no single-stain control with that `primary_channel` | a spectral marker channel has no control, or a control without `primary_channel` makes coverage unknown |
| `control_dyes` | panels with controls | two controls of the panel share a dye | |
| `primary_channel` | controls with one | the channel is not in the file | |
| `detector_configuration` | every file of a panel but the first (samples only when they are compensated or unmixed) | the `$PnN` set differs from the panel's first sample | a `$PnV` voltage differs |
| `unique_files` | manifest | | a file is listed twice |

A marker without `channel` matches a parameter whose `$PnS` or `$PnN` equals
it, or whose `$PnS` starts with it as a word (`CD3 BUV395`), ignoring case;
`CD3` does not match `CD38`.

Samples are checked against their own panel. A control without `panel` is
part of every panel, so it is compared with each panel's detector set.
`control_coverage` resolves markers without `channel` on the panel's first
readable sample. The report lists each panel with its samples, controls,
`signal_state` and compensation source under `panels`.

`signal_state` can only check the traces other software leaves: FlowJo's
`Comp-<detector>` channel names and a leftover spillover keyword. Values
compensated in place under the detector names, with the spillover keyword
still present, look exactly like raw values; such files must be declared
`compensated` in the manifest.

## Public API

Enable the `preflight` feature and import these items from
`cytiq_qc::preflight`. `CheckStatus` is shared with the root QC API.

| Item | Purpose |
| --- | --- |
| `PreflightRunner::run` / `PreflightConfig` | Run the checks and write the report |
| `PreflightRunner::input_files` | The manifest, the FCS files it lists and the panels' matrix files, for provenance |
| `PreflightReport` (`ensure_passed`), `PanelSummary` | The report: status, counts, files, panels, checks |
| `Check`, `CheckStatus` | One finding: `pass`, `warn`, `fail` |
| `SPILLOVER_CONDITION_WARN` | Condition-number warning threshold |
| `PreflightError` | Error type |

## Dependencies

[`cytiq-fcs`](../../cytiq-fcs), [`cytiq-manifest`](../../cytiq-manifest),
[`cytiq-compensate`](../../cytiq-compensate) (matrix files, read as `cytiq
compensate --matrix` reads them), `serde` / `serde_json`.

## Tests

```bash
cargo test -p cytiq-qc --features preflight
cargo test -p cytiq-engine --test preflight_cli
```
