//! The run manifest document: cytometry, panels, samples and controls.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cytometry {
    /// Compensation with a spillover matrix.
    Conventional,
    /// Spectral unmixing with reference controls.
    Spectral,
    /// Mass cytometry with an explicitly selected ion-count recipe.
    Cytof,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ManifestDocument")]
pub struct RunManifest {
    pub schema_version: u32,
    pub cytometry: Cytometry,
    /// One or more panels; a manifest with a single `panel` has one entry.
    pub panels: Vec<Panel>,
    pub samples: Vec<SampleEntry>,
    #[serde(default)]
    pub controls: Vec<ControlEntry>,
}

/// The manifest as written: `panel` (one) or `panels` (several).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDocument {
    #[serde(default = "manifest_v1")]
    schema_version: u32,
    cytometry: Cytometry,
    #[serde(default)]
    panel: Option<Panel>,
    #[serde(default)]
    panels: Option<Vec<Panel>>,
    samples: Vec<SampleEntry>,
    #[serde(default)]
    controls: Vec<ControlEntry>,
}

const fn manifest_v1() -> u32 {
    1
}

impl TryFrom<ManifestDocument> for RunManifest {
    type Error = String;

    fn try_from(document: ManifestDocument) -> Result<Self, Self::Error> {
        let panels = match (document.panel, document.panels) {
            (Some(panel), None) => vec![panel],
            (None, Some(panels)) => panels,
            (Some(_), Some(_)) => {
                return Err("give either `panel` or `panels`, not both".to_owned());
            }
            (None, None) => return Err("missing field `panels` (or `panel`)".to_owned()),
        };
        Ok(Self {
            schema_version: document.schema_version,
            cytometry: document.cytometry,
            panels,
            samples: document.samples,
            controls: document.controls,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    /// Required, and unique, when the run has several panels.
    #[serde(default)]
    pub name: Option<String>,
    pub markers: Vec<Marker>,
    /// What the stored values of the panel's sample files are. Controls are
    /// always raw.
    #[serde(default)]
    pub signal_state: SignalState,
    /// Where the compensation matrix of a conventional panel comes from;
    /// `None` means the samples' own `$SPILLOVER` (`source: fcs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensation: Option<CompensationSource>,
    /// Acceptance rules `cytiq qc` applies to the panel's samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qc: Option<QcPolicy>,
}

/// Panel-level QC policy, checked by `cytiq qc` on a sample's final event
/// mask.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QcPolicy {
    /// Minimum events in the raw sample file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_events: Option<u64>,
    /// Minimum events the final mask keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_kept_events: Option<u64>,
    /// Maximum fraction of raw events the final mask removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_removed_fraction: Option<f64>,
    /// Rules per mask filter column (`margin_pass`, `peacoqc_pass`,
    /// `non_debris`, `singlets`, `live`, gate ids, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: BTreeMap<String, FilterPolicy>,
}

impl QcPolicy {
    /// Whether this policy requests an actual acceptance check.
    #[must_use]
    pub fn has_rules(&self) -> bool {
        self.min_events.is_some()
            || self.min_kept_events.is_some()
            || self.max_removed_fraction.is_some()
            || self.filters.values().any(|rule| {
                rule.required
                    || rule.min_assessed_fraction.is_some()
                    || rule.max_removed_fraction.is_some()
            })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterPolicy {
    /// The filter must have assessed at least one event.
    #[serde(default)]
    pub required: bool,
    /// Minimum fraction of the events `assessed_of` names that the filter
    /// assessed (non-null).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_assessed_fraction: Option<f64>,
    /// The events `min_assessed_fraction` is a share of.
    #[serde(default)]
    pub assessed_of: AssessedOf,
    /// Maximum fraction of the events it assessed that the filter removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_removed_fraction: Option<f64>,
    /// What a violated rule of this filter reports.
    #[serde(default)]
    pub severity: Severity,
}

/// The events a filter's `min_assessed_fraction` counts against.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessedOf {
    /// Every raw event.
    #[default]
    Raw,
    /// The events no other filter removed: what the filter saw when it ran
    /// after the others, or all it had to assess when it ran beside them.
    Eligible,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Fail,
    Warn,
}

/// The signal a panel's sample files hold. FCS files have no standard
/// keyword for it, and cytiq stages only recognise their own processing
/// (`CYTIQ_*` keywords), so files exported from other software must say so
/// here.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    /// Detector values as acquired; cytiq compensates or unmixes them.
    #[default]
    Raw,
    /// Compensated by other software (a conventional panel).
    Compensated,
    /// Unmixed by other software (a spectral panel).
    Unmixed,
}

impl Panel {
    /// `name`, or `panel` for an unnamed single panel.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("panel")
    }
}

/// Source of a conventional panel's compensation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompensationSource {
    /// Each sample's own `$SPILLOVER`/`$SPILL` keyword.
    Fcs,
    /// A matrix file, for example from `cytiq autospill`, applied with
    /// `cytiq compensate --matrix`.
    Matrix { matrix: PathBuf, kind: MatrixKind },
    /// A matrix `cytiq autospill` will compute from the panel's single-stain
    /// controls.
    Controls,
}

/// What a matrix file holds, as `cytiq compensate --matrix-kind` takes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixKind {
    Spillover,
    Compensation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    /// Marker name, matched against `$PnS` when `channel` is not given.
    pub marker: String,
    /// Detector channel (`$PnN`) carrying the marker.
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default = "default_required")]
    pub required: bool,
}

const fn default_required() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleEntry {
    pub id: String,
    pub file: PathBuf,
    /// Panel name; required when the run has several panels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// The biological specimen the file was acquired from, shared by its
    /// files in different panels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlEntry {
    pub file: PathBuf,
    pub dye: String,
    #[serde(default)]
    pub primary_channel: Option<String>,
    /// Panel name; a control without one belongs to every panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
}

impl ControlEntry {
    /// Unstained / autofluorescence control (`dye: Autofluorescence`,
    /// `AF`, or `unstained`).
    #[must_use]
    pub fn is_unstained(&self) -> bool {
        ["autofluorescence", "af", "unstained"]
            .iter()
            .any(|name| self.dye.trim().eq_ignore_ascii_case(name))
    }
}

impl RunManifest {
    /// The panel `sample` was acquired with: the one it names, or the only
    /// panel. `None` only for a manifest that fails [`Self::validate`].
    #[must_use]
    pub fn panel_of(&self, sample: &SampleEntry) -> Option<&Panel> {
        match &sample.panel {
            Some(name) => self
                .panels
                .iter()
                .find(|panel| panel.name.as_deref() == Some(name.as_str())),
            None => match self.panels.as_slice() {
                [panel] => Some(panel),
                _ => None,
            },
        }
    }

    /// Controls of `panel`: those naming it, and those naming no panel.
    pub fn controls_of<'a>(&'a self, panel: &'a Panel) -> impl Iterator<Item = &'a ControlEntry> {
        self.controls.iter().filter(move |control| {
            control.panel.is_none() || control.panel.as_deref() == panel.name.as_deref()
        })
    }

    /// The compensation source of a conventional panel of raw files (`fcs`
    /// by default); `None` for spectral panels and for samples declared
    /// already compensated.
    #[must_use]
    pub fn compensation_of<'a>(&self, panel: &'a Panel) -> Option<&'a CompensationSource> {
        const FCS: &CompensationSource = &CompensationSource::Fcs;
        match (self.cytometry, panel.signal_state) {
            (Cytometry::Conventional, SignalState::Raw) => {
                Some(panel.compensation.as_ref().unwrap_or(FCS))
            }
            _ => None,
        }
    }
}
