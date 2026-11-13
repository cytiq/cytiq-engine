//! Consistency rules of a run manifest; files are checked by `cytiq preflight`.

use std::collections::HashSet;
use std::path::Path;

use crate::{
    AssessedOf, CompensationSource, ControlEntry, Cytometry, ManifestError, Panel, QcPolicy,
    RunManifest, SUPPORTED_SCHEMA_VERSIONS, SignalState,
};

impl RunManifest {
    /// Check the manifest's own consistency (files are checked by
    /// `cytiq preflight`): samples, panels and markers present; unique
    /// non-empty sample ids, panel names and marker names; every sample and
    /// control linked to an existing panel; non-empty paths; compensation
    /// only for conventional panels, with controls when it comes from them;
    /// an unstained control for every spectral panel of raw files; a
    /// `signal_state` that fits the cytometry, and no `compensation` for
    /// compensated samples.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Invalid`] naming the first problem.
    pub fn validate(&self, path: &Path) -> Result<(), ManifestError> {
        self.check().map_err(|reason| ManifestError::Invalid {
            path: path.to_path_buf(),
            reason,
        })
    }

    fn check(&self) -> Result<(), String> {
        if !SUPPORTED_SCHEMA_VERSIONS.contains(&self.schema_version) {
            return Err("unsupported manifest schema_version".to_owned());
        }
        if self.cytometry == Cytometry::Cytof && self.schema_version < 2 {
            return Err("cytof requires manifest schema_version 2 or later".to_owned());
        }
        if self.samples.is_empty() {
            return Err("`samples` must list at least one sample".to_owned());
        }
        self.check_panels()?;
        let mut ids = HashSet::new();
        let mut specimens = HashSet::new();
        for sample in &self.samples {
            if sample.id.trim().is_empty() {
                return Err("sample ids must not be empty".to_owned());
            }
            if !ids.insert(sample.id.trim()) {
                return Err(format!("sample id `{}` appears more than once", sample.id));
            }
            let Some(panel) = self.panel_of(sample) else {
                return Err(match &sample.panel {
                    Some(name) => format!("sample `{}`: panel `{name}` is not defined", sample.id),
                    None => format!(
                        "sample `{}` must name its `panel`: the run has several",
                        sample.id
                    ),
                });
            };
            if let Some(specimen) = &sample.specimen
                && self.schema_version == 1
                && !specimens.insert((specimen.trim(), panel.label()))
            {
                return Err(format!(
                    "specimen `{specimen}` has more than one file in panel `{}`",
                    panel.label()
                ));
            }
            if self.schema_version >= 2
                && sample
                    .specimen
                    .as_ref()
                    .is_some_and(|id| id.trim().is_empty())
            {
                return Err("specimen ids must not be empty".to_owned());
            }
        }
        for control in &self.controls {
            if let Some(name) = &control.panel
                && !self
                    .panels
                    .iter()
                    .any(|panel| panel.name.as_deref() == Some(name))
            {
                return Err(format!(
                    "control `{}`: panel `{name}` is not defined",
                    control.dye
                ));
            }
        }
        let files = self
            .samples
            .iter()
            .map(|sample| &sample.file)
            .chain(self.controls.iter().map(|control| &control.file));
        for file in files {
            if file.as_os_str().is_empty() {
                return Err("file paths must not be empty".to_owned());
            }
        }
        for panel in &self.panels {
            self.check_calibration(panel)?;
        }
        self.check_metadata()
    }

    fn check_panels(&self) -> Result<(), String> {
        if self.panels.is_empty() {
            return Err("`panels` must list at least one panel".to_owned());
        }
        let mut names = HashSet::new();
        for panel in &self.panels {
            match panel.name.as_deref().map(str::trim) {
                Some("") => return Err("panel names must not be empty".to_owned()),
                Some(name) if !names.insert(name) => {
                    return Err(format!("panel `{name}` appears more than once"));
                }
                None if self.panels.len() > 1 => {
                    return Err("every panel needs a `name` when the run has several".to_owned());
                }
                _ => {}
            }
            if panel.markers.is_empty() {
                return Err(format!(
                    "panel `{}`: `markers` must list at least one marker",
                    panel.label()
                ));
            }
            if let Some(qc) = &panel.qc {
                check_qc_policy(panel.label(), qc)?;
            }
            let mut markers = HashSet::new();
            for marker in &panel.markers {
                if marker.marker.trim().is_empty() {
                    return Err("marker names must not be empty".to_owned());
                }
                if !markers.insert(marker.marker.trim().to_ascii_lowercase()) {
                    return Err(format!(
                        "panel `{}`: marker `{}` appears more than once",
                        panel.label(),
                        marker.marker
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_calibration(&self, panel: &Panel) -> Result<(), String> {
        let label = panel.label();
        if self.cytometry == Cytometry::Cytof {
            return if panel.signal_state == SignalState::Raw && panel.compensation.is_none() {
                Ok(())
            } else {
                Err(format!(
                    "panel `{label}`: CyTOF requires raw ion counts and no compensation"
                ))
            };
        }
        match (self.cytometry, panel.signal_state) {
            (Cytometry::Conventional, SignalState::Unmixed) => {
                return Err(format!(
                    "panel `{label}`: `signal_state: unmixed` applies to spectral runs"
                ));
            }
            (Cytometry::Spectral, SignalState::Compensated) => {
                return Err(format!(
                    "panel `{label}`: `signal_state: compensated` applies to conventional runs; spectral exports are `unmixed`"
                ));
            }
            (Cytometry::Conventional, SignalState::Compensated) if panel.compensation.is_some() => {
                return Err(format!(
                    "panel `{label}`: samples declared `compensated` are not compensated again; remove `compensation`"
                ));
            }
            _ => {}
        }
        match (self.cytometry, &panel.compensation) {
            (Cytometry::Spectral, Some(_)) => Err(format!(
                "panel `{label}`: `compensation` applies to conventional runs; spectral runs are unmixed from their controls"
            )),
            (Cytometry::Spectral, None) => {
                if panel.signal_state == SignalState::Unmixed
                    || self.controls_of(panel).any(ControlEntry::is_unstained)
                {
                    Ok(())
                } else {
                    Err(format!(
                        "panel `{label}`: spectral runs need an unstained control (`dye: Autofluorescence`)"
                    ))
                }
            }
            (Cytometry::Conventional, Some(CompensationSource::Matrix { matrix, .. }))
                if matrix.as_os_str().is_empty() =>
            {
                Err(format!(
                    "panel `{label}`: `compensation.matrix` must not be empty"
                ))
            }
            (Cytometry::Conventional, Some(CompensationSource::Controls))
                if !self
                    .controls_of(panel)
                    .any(|control| !control.is_unstained()) =>
            {
                Err(format!(
                    "panel `{label}`: `compensation: {{ source: controls }}` needs single-stain controls"
                ))
            }
            _ => Ok(()),
        }
    }
}

fn check_qc_policy(panel: &str, qc: &QcPolicy) -> Result<(), String> {
    let fractions = qc
        .max_removed_fraction
        .map(|value| ("max_removed_fraction".to_owned(), value))
        .into_iter()
        .chain(qc.filters.iter().flat_map(|(name, filter)| {
            [
                ("min_assessed_fraction", filter.min_assessed_fraction),
                ("max_removed_fraction", filter.max_removed_fraction),
            ]
            .into_iter()
            .filter_map(move |(key, value)| {
                value.map(|value| (format!("filters.{name}.{key}"), value))
            })
        }));
    for (key, value) in fractions {
        if !(0.0..=1.0).contains(&value) {
            return Err(format!(
                "panel `{panel}`: qc `{key}` must be between 0 and 1, got {value}"
            ));
        }
    }
    if let Some((name, _)) = qc.filters.iter().find(|(_, filter)| {
        filter.assessed_of != AssessedOf::Raw && filter.min_assessed_fraction.is_none()
    }) {
        return Err(format!(
            "panel `{panel}`: qc `filters.{name}.assessed_of` needs `min_assessed_fraction`"
        ));
    }
    if qc.filters.keys().any(|name| name.trim().is_empty()) {
        return Err(format!(
            "panel `{panel}`: qc filter names must not be empty"
        ));
    }
    Ok(())
}
