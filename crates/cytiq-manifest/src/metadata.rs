//! Descriptive metadata after `MIFlowCyt`: experiment, instruments and their
//! detectors, specimens, reagents and control roles (`schema_version: 3`).
//!
//! Identities are kept apart: a *channel* is an FCS parameter (`$PnN`), a
//! *detector* is the instrument's optical or mass detector (`$PnDET`), a
//! *marker* is the analyte the reagent binds (`$PnS`/`$PnANALYTE`) and a
//! *reporter* is the fluorochrome or metal tag that carries it (`$PnTAG`).
//! None of these fields may hold personal data: specimens are described by
//! their biology, never by donor or patient attributes.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{ControlEntry, RunManifest};

/// The study a run belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the experiment is meant to show (`MIFlowCyt` "purpose").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Experimental variables, for example `treatment` or `time point`, and
    /// what each one describes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, String>,
}

/// A cytometer and its detectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instrument {
    /// Identifier panels refer to; not the instrument serial number.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optical configuration name, for example `3L-13C`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detectors: Vec<Detector>,
}

/// One detector of an instrument (`$PnDET`), not the FCS channel it feeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detector {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<DetectorKind>,
    /// Excitation laser wavelength in nanometres (`$PnL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excitation_nm: Option<u32>,
    /// Emission filter, for example `530/30` (`$PnF`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_filter: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorKind {
    Pmt,
    Apd,
    SpectralArray,
    Mass,
    Other,
}

/// The biological material a run's samples come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specimen {
    /// Study pseudonym the samples' `specimen` refers to.
    pub id: String,
    /// Organism, for example `Homo sapiens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organism: Option<String>,
    /// Source material, for example `peripheral blood`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Ontology terms (`UBERON:0000178`, `NCBITaxon:9606`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ontology: Vec<OntologyTerm>,
    /// How the specimen was prepared, for example `RBC lysis`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OntologyTerm {
    /// CURIE `PREFIX:LOCAL`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The antibody or probe that carries a marker's reporter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reagent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot: Option<String>,
}

/// What a control file is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRole {
    Unstained,
    SingleStain,
    Fmo,
    Isotype,
    Beads,
    Reference,
}

/// Longest excitation wavelength accepted, in nanometres.
const EXCITATION_NM_MAX: u32 = 2_000;

impl RunManifest {
    /// Whether the document uses a field introduced by `schema_version: 3`.
    pub(crate) fn uses_v3_metadata(&self) -> Option<&'static str> {
        if self.experiment.is_some() {
            return Some("experiment");
        }
        if !self.instruments.is_empty() {
            return Some("instruments");
        }
        if !self.specimens.is_empty() {
            return Some("specimens");
        }
        if self.panels.iter().any(|panel| panel.instrument.is_some()) {
            return Some("panels[].instrument");
        }
        let markers = self.panels.iter().flat_map(|panel| &panel.markers);
        for marker in markers {
            if marker.reporter.is_some() {
                return Some("markers[].reporter");
            }
            if marker.detector.is_some() {
                return Some("markers[].detector");
            }
            if marker.reagent.is_some() {
                return Some("markers[].reagent");
            }
        }
        if self.controls.iter().any(|control| control.role.is_some()) {
            return Some("controls[].role");
        }
        None
    }

    /// Rules of the `schema_version: 3` metadata. Completeness is not
    /// required here: the dataset validation profile decides which fields a
    /// dataset needs.
    pub(crate) fn check_metadata(&self) -> Result<(), String> {
        if self.schema_version < 3 {
            return match self.uses_v3_metadata() {
                Some(field) => Err(format!("`{field}` requires manifest schema_version 3")),
                None => Ok(()),
            };
        }
        if let Some(experiment) = &self.experiment {
            check_experiment(experiment)?;
        }
        self.check_instruments()?;
        self.check_specimens()?;
        self.check_markers()?;
        self.check_control_roles()
    }

    fn check_instruments(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        for instrument in &self.instruments {
            let id = non_empty("instrument id", &instrument.id)?;
            if !ids.insert(id) {
                return Err(format!("instrument `{id}` appears more than once"));
            }
            for (field, value) in [
                ("manufacturer", &instrument.manufacturer),
                ("model", &instrument.model),
                ("configuration", &instrument.configuration),
            ] {
                optional(&format!("instrument `{id}`: `{field}`"), value.as_deref())?;
            }
            let mut detectors = HashSet::new();
            for detector in &instrument.detectors {
                let name = non_empty(&format!("instrument `{id}`: detector id"), &detector.id)?;
                if !detectors.insert(name.to_ascii_lowercase()) {
                    return Err(format!(
                        "instrument `{id}`: detector `{name}` appears more than once"
                    ));
                }
                if detector
                    .excitation_nm
                    .is_some_and(|nm| nm == 0 || nm > EXCITATION_NM_MAX)
                {
                    return Err(format!(
                        "instrument `{id}`: detector `{name}`: `excitation_nm` must be between 1 and {EXCITATION_NM_MAX}"
                    ));
                }
                optional(
                    &format!("instrument `{id}`: detector `{name}`: `emission_filter`"),
                    detector.emission_filter.as_deref(),
                )?;
            }
        }
        for panel in &self.panels {
            if let Some(instrument) = &panel.instrument
                && !ids.contains(instrument.trim())
            {
                return Err(format!(
                    "panel `{}`: instrument `{instrument}` is not defined",
                    panel.label()
                ));
            }
        }
        Ok(())
    }

    fn check_specimens(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        for specimen in &self.specimens {
            let id = non_empty("specimen id", &specimen.id)?;
            if !ids.insert(id) {
                return Err(format!("specimen `{id}` appears more than once"));
            }
            for (field, value) in [
                ("organism", &specimen.organism),
                ("material", &specimen.material),
                ("preparation", &specimen.preparation),
            ] {
                optional(&format!("specimen `{id}`: `{field}`"), value.as_deref())?;
            }
            for term in &specimen.ontology {
                if !is_curie(&term.id) {
                    return Err(format!(
                        "specimen `{id}`: ontology id `{}` is not a `PREFIX:LOCAL` CURIE",
                        term.id
                    ));
                }
                optional(
                    &format!("specimen `{id}`: ontology label"),
                    term.label.as_deref(),
                )?;
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        for sample in &self.samples {
            match &sample.specimen {
                Some(specimen) if !ids.contains(specimen.trim()) => {
                    return Err(format!(
                        "sample `{}`: specimen `{specimen}` is not defined",
                        sample.id
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn check_markers(&self) -> Result<(), String> {
        for panel in &self.panels {
            let label = panel.label();
            let instrument = panel.instrument.as_deref().and_then(|id| {
                self.instruments
                    .iter()
                    .find(|instrument| instrument.id.trim() == id.trim())
            });
            let mut reporters = HashSet::new();
            let mut channels = HashSet::new();
            for marker in &panel.markers {
                let name = &marker.marker;
                if let Some(channel) = &marker.channel {
                    let channel = non_empty(
                        &format!("panel `{label}`: marker `{name}`: `channel`"),
                        channel,
                    )?;
                    if !channels.insert(channel.to_ascii_lowercase()) {
                        return Err(format!(
                            "panel `{label}`: channel `{channel}` carries more than one marker"
                        ));
                    }
                }
                if let Some(reporter) = &marker.reporter {
                    let reporter = non_empty(
                        &format!("panel `{label}`: marker `{name}`: `reporter`"),
                        reporter,
                    )?;
                    if !reporters.insert(reporter.to_ascii_lowercase()) {
                        return Err(format!(
                            "panel `{label}`: reporter `{reporter}` labels more than one marker"
                        ));
                    }
                }
                if let Some(detector) = &marker.detector {
                    let Some(instrument) = instrument else {
                        return Err(format!(
                            "panel `{label}`: marker `{name}` names detector `{detector}` but the panel names no `instrument`"
                        ));
                    };
                    if !instrument
                        .detectors
                        .iter()
                        .any(|known| known.id.trim().eq_ignore_ascii_case(detector.trim()))
                    {
                        return Err(format!(
                            "panel `{label}`: detector `{detector}` is not defined on instrument `{}`",
                            instrument.id
                        ));
                    }
                }
                if let Some(reagent) = &marker.reagent {
                    for (field, value) in [
                        ("clone", &reagent.clone),
                        ("vendor", &reagent.vendor),
                        ("catalog", &reagent.catalog),
                        ("lot", &reagent.lot),
                    ] {
                        optional(
                            &format!("panel `{label}`: marker `{name}`: reagent `{field}`"),
                            value.as_deref(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_control_roles(&self) -> Result<(), String> {
        for control in &self.controls {
            match (control.role, control.is_unstained()) {
                (Some(ControlRole::Unstained), false) => {
                    return Err(format!(
                        "control `{}`: `role: unstained` needs `dye: Autofluorescence`",
                        control.dye
                    ));
                }
                (Some(role), true) if role != ControlRole::Unstained => {
                    return Err(format!(
                        "control `{}` is unstained; its `role` must be `unstained`",
                        control.dye
                    ));
                }
                (Some(ControlRole::SingleStain), _) => self.check_single_stain(control)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// A single-stain control's dye is the reporter of a marker in each of
    /// its panels that declares reporters.
    fn check_single_stain(&self, control: &ControlEntry) -> Result<(), String> {
        for panel in self.panels.iter().filter(|panel| {
            control.panel.is_none() || control.panel.as_deref() == panel.name.as_deref()
        }) {
            let reporters: Vec<&str> = panel
                .markers
                .iter()
                .filter_map(|marker| marker.reporter.as_deref())
                .collect();
            if !reporters.is_empty()
                && !reporters
                    .iter()
                    .any(|reporter| reporter.trim().eq_ignore_ascii_case(control.dye.trim()))
            {
                return Err(format!(
                    "control `{}`: no marker of panel `{}` has reporter `{}`",
                    control.dye,
                    panel.label(),
                    control.dye
                ));
            }
        }
        Ok(())
    }
}

fn check_experiment(experiment: &Experiment) -> Result<(), String> {
    optional("experiment `title`", experiment.title.as_deref())?;
    optional("experiment `purpose`", experiment.purpose.as_deref())?;
    for keyword in &experiment.keywords {
        non_empty("experiment keyword", keyword)?;
    }
    for (name, description) in &experiment.variables {
        non_empty("experiment variable name", name)?;
        non_empty(&format!("experiment variable `{name}`"), description)?;
    }
    Ok(())
}

fn non_empty<'a>(what: &str, value: &'a str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{what} must not be empty"))
    } else {
        Ok(value)
    }
}

fn optional(what: &str, value: Option<&str>) -> Result<(), String> {
    value.map_or(Ok(()), |value| non_empty(what, value).map(|_| ()))
}

/// `PREFIX:LOCAL`, where the prefix starts with a letter and holds letters,
/// digits or `_`, and the local part is not empty and holds no whitespace.
fn is_curie(id: &str) -> bool {
    let Some((prefix, local)) = id.split_once(':') else {
        return false;
    };
    prefix
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !local.is_empty()
        && !local.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::is_curie;

    #[test]
    fn curies_need_a_prefix_and_a_local_part() {
        assert!(is_curie("UBERON:0000178"));
        assert!(is_curie("NCBITaxon:9606"));
        assert!(!is_curie("UBERON"));
        assert!(!is_curie(":0000178"));
        assert!(!is_curie("UBERON:"));
        assert!(!is_curie("1X:2"));
        assert!(!is_curie("CL:00 01"));
    }
}
