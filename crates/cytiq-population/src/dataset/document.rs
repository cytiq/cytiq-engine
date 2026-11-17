//! Rules a manifest must satisfy on its own, without reading any object.

use std::collections::{BTreeMap, HashMap, HashSet};

use cytiq_manifest::{ControlRole, Cytometry, SignalState};

use super::{
    AcquisitionRole, ArtifactEntry, ArtifactRole, ChannelKind, DatasetManifest, LayerSummary,
    PanelEntry, contracts, profile,
};
use crate::events::{self, LayerState, Origin, StepKind};
use crate::identity::object_key_problem;
use crate::issue::{IssueCode, Issues, pointer};
use crate::observation::is_sha256;

/// Report every rule `manifest` breaks, and what `profile` requires.
pub fn validate_document(
    manifest: &DatasetManifest,
    profile: profile::Profile,
    issues: &mut Issues,
) {
    check_identity(manifest, issues);
    check_contracts(manifest, issues);
    manifest.privacy.check("/privacy", issues);
    check_instruments(manifest, issues);
    for (index, panel) in manifest.panels.iter().enumerate() {
        check_panel(manifest, index, panel, issues);
    }
    check_unique(
        manifest.panels.iter().map(|panel| panel.id.as_str()),
        "panels",
        "panel",
        issues,
    );
    check_unique(
        manifest
            .specimens
            .iter()
            .map(|specimen| specimen.id.as_str()),
        "specimens",
        "specimen",
        issues,
    );
    check_acquisitions(manifest, issues);
    check_samples(manifest, issues);
    check_controls(manifest, issues);
    check_knowledge(manifest, issues);
    check_artifacts(manifest, issues);
    if let Some(design) = &manifest.design {
        check_design(manifest, design, issues);
    }
    profile::check(manifest, profile, issues);
}

/// A factor name: lowercase, an SQL identifier.
fn factor_name_valid(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A factor value: a short code, never free text.
fn factor_value_valid(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:+-".contains(c))
}

/// Whether `value` reads as a calendar date (`2024-01-31`, `31.01.2024`,
/// `01-JAN-2024`), which would identify a visit.
fn looks_like_date(value: &str) -> bool {
    let parts: Vec<&str> = value.split(['-', '.', '/', ':']).collect();
    let numeric = |part: &str, digits: &[usize]| {
        digits.contains(&part.len()) && part.chars().all(|c| c.is_ascii_digit())
    };
    let month = |part: &str| {
        [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .contains(&part.to_ascii_lowercase().as_str())
    };
    parts.len() == 3
        && parts.iter().any(|part| numeric(part, &[4]))
        && parts
            .iter()
            .all(|part| numeric(part, &[1, 2, 4]) || month(part))
}

/// The study design: unique pseudonymous subjects, declared factors given
/// at their level only, with every factor of its level, and references to
/// the manifest's specimens and samples.
fn check_design(manifest: &DatasetManifest, design: &super::Design, issues: &mut Issues) {
    let mut declared: HashMap<&str, super::FactorLevel> = HashMap::new();
    for (index, factor) in design.factors.iter().enumerate() {
        let at = pointer(["design", "factors", &index.to_string(), "name"]);
        if !factor_name_valid(&factor.name) {
            issues.error(
                IssueCode::MalformedDocument,
                at,
                format!(
                    "factor `{}` is not a lowercase identifier of at most 32 characters",
                    factor.name
                ),
            );
        } else if declared.insert(&factor.name, factor.level).is_some() {
            issues.error(
                IssueCode::DuplicateIdentifier,
                at,
                format!("factor `{}` is declared twice", factor.name),
            );
        }
    }
    let subjects: HashSet<&str> = design.subjects.iter().map(|s| s.id.as_str()).collect();
    let mut seen = HashSet::new();
    for (index, subject) in design.subjects.iter().enumerate() {
        let at = pointer(["design", "subjects", &index.to_string()]);
        if !seen.insert(subject.id.to_ascii_lowercase()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                format!("{at}/id"),
                format!("subject `{}` appears more than once", subject.id),
            );
        }
        check_pseudonym(&subject.id, &format!("{at}/id"), issues);
        check_factors(
            &subject.factors,
            super::FactorLevel::Subject,
            &declared,
            &at,
            issues,
        );
    }
    let specimens: HashSet<&str> = manifest.specimens.iter().map(|s| s.id.as_str()).collect();
    let mut seen = HashSet::new();
    for (index, entry) in design.specimens.iter().enumerate() {
        let at = pointer(["design", "specimens", &index.to_string()]);
        if !specimens.contains(entry.specimen.as_str()) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/specimen"),
                format!("specimen `{}` is not defined", entry.specimen),
            );
        }
        if !seen.insert(entry.specimen.as_str()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                format!("{at}/specimen"),
                format!("specimen `{}` is designed twice", entry.specimen),
            );
        }
        if !subjects.contains(entry.subject.as_str()) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/subject"),
                format!("subject `{}` is not defined", entry.subject),
            );
        }
        check_factors(
            &entry.factors,
            super::FactorLevel::Specimen,
            &declared,
            &at,
            issues,
        );
    }
    let samples: HashSet<&str> = manifest.samples.iter().map(|s| s.id.as_str()).collect();
    let mut seen = HashSet::new();
    for (index, entry) in design.samples.iter().enumerate() {
        let at = pointer(["design", "samples", &index.to_string()]);
        if !samples.contains(entry.sample.as_str()) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/sample"),
                format!("sample `{}` is not defined", entry.sample),
            );
        }
        if !seen.insert(entry.sample.as_str()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                format!("{at}/sample"),
                format!("sample `{}` is designed twice", entry.sample),
            );
        }
        check_factors(
            &entry.factors,
            super::FactorLevel::Sample,
            &declared,
            &at,
            issues,
        );
    }
}

/// Subject ids are study pseudonyms: neither a date nor an acquisition id.
fn check_pseudonym(id: &str, at: &str, issues: &mut Issues) {
    if id.trim().is_empty() {
        issues.error(IssueCode::MissingMetadata, at, "subject id is empty");
    } else if looks_like_date(id) {
        issues.warning(
            IssueCode::PrivacyViolation,
            at,
            "a subject id shaped like a date may identify a person",
        );
    } else if is_sha256(id) {
        issues.warning(
            IssueCode::DuplicateIdentifier,
            at,
            "a subject id shaped like a SHA-256 is easily mistaken for an acquisition id",
        );
    }
}

/// An entity's factors: declared, of its level, valid codes, and every
/// factor of its level present.
fn check_factors(
    factors: &BTreeMap<String, String>,
    level: super::FactorLevel,
    declared: &HashMap<&str, super::FactorLevel>,
    at: &str,
    issues: &mut Issues,
) {
    for (name, value) in factors {
        let at = format!("{at}{}", pointer(["factors", name.as_str()]));
        match declared.get(name.as_str()) {
            None => issues.error(
                IssueCode::MalformedDocument,
                at.clone(),
                format!("factor `{name}` is not declared"),
            ),
            Some(declared) if *declared != level => issues.error(
                IssueCode::InvalidReference,
                at.clone(),
                format!(
                    "factor `{name}` is a {} factor, not a {} one",
                    declared.as_str(),
                    level.as_str()
                ),
            ),
            Some(_) => {}
        }
        if !factor_value_valid(value) {
            issues.error(
                IssueCode::MalformedDocument,
                at,
                format!("factor value `{value}` is not a short code"),
            );
        } else if looks_like_date(value) {
            issues.warning(
                IssueCode::PrivacyViolation,
                at,
                "a factor value shaped like a date may identify a visit; use a study timepoint",
            );
        }
    }
    for (name, declared) in declared {
        if *declared == level && !factors.contains_key(*name) {
            issues.error(
                IssueCode::MissingMetadata,
                format!("{at}/factors"),
                format!("the {} lacks factor `{name}`", level.as_str()),
            );
        }
    }
}

fn check_identity(manifest: &DatasetManifest, issues: &mut Issues) {
    if let Some(recorded) = &manifest.dataset_id
        && *recorded != manifest.dataset_id()
    {
        issues.error(
            IssueCode::CorruptArtifactHash,
            "/dataset_id",
            "`dataset_id` does not match the manifest's content",
        );
    }
}

fn check_contracts(manifest: &DatasetManifest, issues: &mut Issues) {
    let mut seen = HashSet::new();
    for (index, contract) in manifest.contracts.iter().enumerate() {
        let at = pointer(["contracts", &index.to_string()]);
        if !contracts::is_known(contract) {
            issues.error(
                IssueCode::UnsupportedSchema,
                at.clone(),
                format!("contract `{contract}` is not supported"),
            );
        }
        if !seen.insert(contract.as_str()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                at,
                format!("contract `{contract}` is listed twice"),
            );
        }
    }
}

/// Report ids of `section` that repeat (ignoring case) or are empty.
fn check_unique<'a>(
    ids: impl Iterator<Item = &'a str>,
    section: &str,
    what: &str,
    issues: &mut Issues,
) {
    let mut seen = HashSet::new();
    for (index, id) in ids.enumerate() {
        let at = pointer([section, &index.to_string(), "id"]);
        if id.trim().is_empty() {
            issues.error(
                IssueCode::MissingMetadata,
                at,
                format!("{what} id is empty"),
            );
        } else if !seen.insert(id.trim().to_ascii_lowercase()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                at,
                format!("{what} `{id}` appears more than once"),
            );
        }
    }
}

fn check_instruments(manifest: &DatasetManifest, issues: &mut Issues) {
    check_unique(
        manifest
            .instruments
            .iter()
            .map(|instrument| instrument.id.as_str()),
        "instruments",
        "instrument",
        issues,
    );
    for (index, instrument) in manifest.instruments.iter().enumerate() {
        let mut seen = HashSet::new();
        for (d, detector) in instrument.detectors.iter().enumerate() {
            if !seen.insert(detector.id.trim().to_ascii_lowercase()) {
                issues.error(
                    IssueCode::DuplicateIdentifier,
                    pointer([
                        "instruments",
                        &index.to_string(),
                        "detectors",
                        &d.to_string(),
                    ]),
                    format!("detector `{}` appears more than once", detector.id),
                );
            }
        }
    }
}

fn check_panel(manifest: &DatasetManifest, index: usize, panel: &PanelEntry, issues: &mut Issues) {
    let at = pointer(["panels", &index.to_string()]);
    let instrument = panel.instrument.as_ref().map(|id| {
        let found = manifest.instruments.iter().find(|i| &i.id == id);
        if found.is_none() {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/instrument"),
                format!("instrument `{id}` is not defined"),
            );
        }
        found
    });
    match (panel.cytometry, panel.signal_state) {
        (Cytometry::Conventional, SignalState::Unmixed)
        | (Cytometry::Spectral, SignalState::Compensated)
        | (Cytometry::Cytof, SignalState::Compensated | SignalState::Unmixed) => issues.error(
            IssueCode::IncompatibleSignalState,
            format!("{at}/signal_state"),
            format!(
                "a {:?} panel cannot hold {:?} files",
                panel.cytometry, panel.signal_state
            ),
        ),
        _ => {}
    }
    if panel.channels.is_empty() {
        issues.error(
            IssueCode::MissingMetadata,
            format!("{at}/channels"),
            "a panel lists its channels",
        );
    }
    let mut previous = 0_u16;
    let mut names = HashSet::new();
    let mut reporters = HashSet::new();
    for (c, channel) in panel.channels.iter().enumerate() {
        let here = format!("{at}{}", pointer(["channels", &c.to_string()]));
        if channel.index <= previous {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                format!("{here}/index"),
                format!("channel `{}` is out of `$Pn` order", channel.name),
            );
        }
        previous = channel.index;
        if let Some(problem) = events::channel_name_problem(&channel.name) {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                format!("{here}/name"),
                format!("{problem}: `{}`", channel.name),
            );
        } else if !names.insert(channel.name.to_ascii_lowercase()) {
            issues.error(
                IssueCode::InconsistentChannelOrder,
                format!("{here}/name"),
                format!("channel `{}` repeats ignoring case", channel.name),
            );
        }
        if let (Some(detector), Some(Some(instrument))) = (&channel.detector, &instrument)
            && !instrument
                .detectors
                .iter()
                .any(|known| known.id.eq_ignore_ascii_case(detector))
        {
            issues.error(
                IssueCode::PanelMismatch,
                format!("{here}/detector"),
                format!(
                    "detector `{detector}` is not defined on instrument `{}`",
                    instrument.id
                ),
            );
        }
        if let Some(reporter) = &channel.reporter
            && !reporters.insert(reporter.to_ascii_lowercase())
        {
            issues.error(
                IssueCode::PanelMismatch,
                format!("{here}/reporter"),
                format!("reporter `{reporter}` labels more than one channel"),
            );
        }
        check_conflation(channel, &here, issues);
        if panel.cytometry == Cytometry::Cytof
            && matches!(
                channel.kind,
                Some(ChannelKind::Fluorescence | ChannelKind::UnmixedComponent)
            )
        {
            issues.error(
                IssueCode::PanelMismatch,
                format!("{here}/kind"),
                "a mass cytometry panel has no fluorescence channels",
            );
        }
    }
}

/// A marker named like its reporter or detector usually means `$PnS`
/// (`CD3-FITC`) was copied into several identities.
fn check_conflation(channel: &super::ChannelEntry, at: &str, issues: &mut Issues) {
    let Some(marker) = &channel.marker else {
        return;
    };
    for (what, other) in [
        ("reporter", &channel.reporter),
        ("detector", &channel.detector),
    ] {
        if other
            .as_ref()
            .is_some_and(|other| other.eq_ignore_ascii_case(marker))
        {
            issues.warning(
                IssueCode::PanelMismatch,
                format!("{at}/marker"),
                format!("marker `{marker}` equals the channel's {what}; check the identities are not conflated"),
            );
        }
    }
}

fn check_acquisitions(manifest: &DatasetManifest, issues: &mut Issues) {
    let panels: HashSet<&str> = manifest.panels.iter().map(|p| p.id.as_str()).collect();
    let mut seen = HashSet::new();
    for (index, acquisition) in manifest.acquisitions.iter().enumerate() {
        let at = pointer(["acquisitions", &index.to_string()]);
        if !is_sha256(&acquisition.id) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/id"),
                "an acquisition id is the lowercase SHA-256 of the raw FCS file",
            );
        }
        if !seen.insert(acquisition.id.as_str()) {
            issues.error(
                IssueCode::DuplicateAcquisition,
                format!("{at}/id"),
                "the acquisition is listed more than once",
            );
        }
        if !panels.contains(acquisition.panel.as_str()) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/panel"),
                format!("panel `{}` is not defined", acquisition.panel),
            );
        }
        for key in acquisition.keywords.keys() {
            if !manifest.privacy.exports(key) {
                issues.error(
                    IssueCode::PrivacyViolation,
                    format!("{at}{}", pointer(["keywords", key])),
                    format!("keyword `{key}` is neither allowed nor disclosed"),
                );
            }
        }
        if let Some(source) = &acquisition.source {
            if !manifest.privacy.discloses_source_locations() {
                issues.error(
                    IssueCode::PrivacyViolation,
                    format!("{at}/source"),
                    "source FCS files carry identifying TEXT; list them only under a disclosure with `source_locations`",
                );
            }
            check_location(&source.location, &format!("{at}/source/location"), issues);
        }
    }
}

fn check_location(location: &str, at: &str, issues: &mut Issues) {
    if let Some(problem) = object_key_problem(location) {
        issues.error(
            IssueCode::PrivacyViolation,
            at,
            format!("{problem}: only relative object keys are recorded"),
        );
    }
}

fn check_samples(manifest: &DatasetManifest, issues: &mut Issues) {
    check_unique(
        manifest.samples.iter().map(|sample| sample.id.as_str()),
        "samples",
        "sample",
        issues,
    );
    let acquisitions: HashMap<&str, &super::AcquisitionEntry> = manifest
        .acquisitions
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let panels: HashSet<&str> = manifest.panels.iter().map(|p| p.id.as_str()).collect();
    let specimens: HashSet<&str> = manifest.specimens.iter().map(|s| s.id.as_str()).collect();
    let mut bound: HashMap<&str, &str> = HashMap::new();
    for (index, sample) in manifest.samples.iter().enumerate() {
        let at = pointer(["samples", &index.to_string()]);
        if is_sha256(&sample.id) {
            issues.warning(
                IssueCode::DuplicateIdentifier,
                format!("{at}/id"),
                "a sample id shaped like a SHA-256 is easily mistaken for an acquisition id",
            );
        }
        if !panels.contains(sample.panel.as_str()) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/panel"),
                format!("panel `{}` is not defined", sample.panel),
            );
        }
        if let Some(specimen) = &sample.specimen
            && !specimens.is_empty()
            && !specimens.contains(specimen.as_str())
        {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/specimen"),
                format!("specimen `{specimen}` is not defined"),
            );
        }
        match acquisitions.get(sample.acquisition.as_str()) {
            None => issues.error(
                IssueCode::InvalidReference,
                format!("{at}/acquisition"),
                "the acquisition is not listed",
            ),
            Some(acquisition) => {
                if acquisition.panel != sample.panel {
                    issues.error(
                        IssueCode::PanelMismatch,
                        format!("{at}/panel"),
                        format!(
                            "the acquisition was recorded on panel `{}`",
                            acquisition.panel
                        ),
                    );
                }
                if acquisition.role != AcquisitionRole::Sample {
                    issues.error(
                        IssueCode::InvalidReference,
                        format!("{at}/acquisition"),
                        "the acquisition is a control",
                    );
                }
            }
        }
        if let Some(other) = bound.insert(sample.acquisition.as_str(), sample.id.as_str()) {
            issues.error(
                IssueCode::DuplicateAcquisition,
                format!("{at}/acquisition"),
                format!("the acquisition is already sample `{other}`"),
            );
        }
    }
    for (index, acquisition) in manifest.acquisitions.iter().enumerate() {
        if acquisition.role == AcquisitionRole::Sample
            && !bound.contains_key(acquisition.id.as_str())
        {
            issues.warning(
                IssueCode::InvalidReference,
                pointer(["acquisitions", &index.to_string()]),
                "no sample refers to this acquisition",
            );
        }
    }
}

fn check_controls(manifest: &DatasetManifest, issues: &mut Issues) {
    for (index, control) in manifest.controls.iter().enumerate() {
        let at = pointer(["controls", &index.to_string()]);
        match manifest
            .acquisitions
            .iter()
            .find(|a| a.id == control.acquisition)
        {
            None => issues.error(
                IssueCode::InvalidReference,
                format!("{at}/acquisition"),
                "the acquisition is not listed",
            ),
            Some(acquisition) if acquisition.role != AcquisitionRole::Control => issues.error(
                IssueCode::InvalidReference,
                format!("{at}/acquisition"),
                "the acquisition is a sample",
            ),
            Some(_) => {}
        }
        let panels: Vec<&PanelEntry> = match &control.panel {
            Some(id) => {
                let found: Vec<_> = manifest.panels.iter().filter(|p| &p.id == id).collect();
                if found.is_empty() {
                    issues.error(
                        IssueCode::InvalidReference,
                        format!("{at}/panel"),
                        format!("panel `{id}` is not defined"),
                    );
                }
                found
            }
            None => manifest.panels.iter().collect(),
        };
        if control.role == ControlRole::SingleStain {
            let Some(reporter) = &control.reporter else {
                issues.error(
                    IssueCode::MissingMetadata,
                    format!("{at}/reporter"),
                    "a single-stain control names its reporter",
                );
                continue;
            };
            let declared = panels.iter().any(|panel| {
                panel.channels.iter().any(|c| {
                    c.reporter
                        .as_ref()
                        .is_some_and(|r| r.eq_ignore_ascii_case(reporter))
                })
            });
            if !declared {
                issues.error(
                    IssueCode::PanelMismatch,
                    format!("{at}/reporter"),
                    format!("no channel of the control's panels carries reporter `{reporter}`"),
                );
            }
        }
    }
}

fn check_knowledge(manifest: &DatasetManifest, issues: &mut Issues) {
    let mut seen = HashSet::new();
    for (index, entry) in manifest.knowledge.iter().enumerate() {
        let at = pointer(["knowledge", &index.to_string()]);
        if entry.kind.trim().is_empty() {
            issues.error(
                IssueCode::MissingMetadata,
                format!("{at}/kind"),
                "knowledge kind is empty",
            );
        }
        if !is_sha256(&entry.sha256) {
            issues.error(
                IssueCode::InvalidReference,
                format!("{at}/sha256"),
                "a knowledge reference is a canonical SHA-256",
            );
        }
        if !seen.insert(entry.sha256.as_str()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                format!("{at}/sha256"),
                "the knowledge artifact is listed twice",
            );
        }
    }
}

/// Which contracts each role takes.
fn role_takes(role: ArtifactRole, contract: &str) -> bool {
    match role {
        ArtifactRole::SignalLayer => {
            contract == contracts::EVENTS || contract == contracts::EVENTS_PARTS
        }
        ArtifactRole::TechnicalMask => contract == contracts::MASK,
        ArtifactRole::Annotation => {
            contract == contracts::MASK
                || contract == contracts::LABELS
                || contracts::CLASSIFY_PREDICTIONS.contains(&contract)
                || contracts::ANNOTATIONS.contains(&contract)
        }
        ArtifactRole::Observations => contract == contracts::OBSERVATIONS,
        ArtifactRole::Model => contract == contracts::CLASSIFY_BUNDLE,
        ArtifactRole::Gating => contract == contracts::GATING_ML,
        ArtifactRole::Knowledge => contract == contracts::CYTIQ_TREE,
        ArtifactRole::Provenance => contract == contracts::PROVENANCE,
    }
}

/// Roles whose objects hold events of exactly one acquisition.
fn per_acquisition(role: ArtifactRole) -> bool {
    matches!(
        role,
        ArtifactRole::SignalLayer | ArtifactRole::TechnicalMask | ArtifactRole::Annotation
    )
}

fn check_artifacts(manifest: &DatasetManifest, issues: &mut Issues) {
    let listed: HashSet<&str> = manifest.contracts.iter().map(String::as_str).collect();
    let acquisitions: HashMap<&str, &super::AcquisitionEntry> = manifest
        .acquisitions
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let mut by_id: HashMap<&str, &ArtifactEntry> = HashMap::new();
    let mut locations = HashSet::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let at = pointer(["artifacts", &index.to_string()]);
        check_artifact_fields(manifest, artifact, &at, &listed, &acquisitions, issues);
        if !locations.insert(artifact.location.as_str()) {
            issues.error(
                IssueCode::DuplicateIdentifier,
                format!("{at}/location"),
                "two artifacts share one location",
            );
        }
        if let Some(previous) = by_id.insert(artifact.semantic_id.as_str(), artifact) {
            let code = if artifact.role == ArtifactRole::SignalLayer
                && previous.role == ArtifactRole::SignalLayer
            {
                IssueCode::DuplicateEventIdentity
            } else {
                IssueCode::DuplicateIdentifier
            };
            issues.error(
                code,
                format!("{at}/semantic_id"),
                "the artifact is listed twice: its events would be counted twice",
            );
        }
    }
    let provenance: HashSet<&str> = manifest
        .artifacts
        .iter()
        .filter(|a| a.role == ArtifactRole::Provenance)
        .map(|a| a.location.as_str())
        .collect();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let at = pointer(["artifacts", &index.to_string()]);
        check_parents(artifact, &at, &by_id, issues);
        check_execution(manifest, artifact, &at, &provenance, issues);
        if let (ArtifactRole::SignalLayer, Some(layer)) = (artifact.role, &artifact.layer) {
            let panel = artifact
                .acquisition
                .as_deref()
                .and_then(|id| acquisitions.get(id))
                .and_then(|a| manifest.panels.iter().find(|p| p.id == a.panel));
            check_layer(layer, panel, &at, issues);
        }
    }
    check_recipes(manifest, &acquisitions, issues);
}

fn check_artifact_fields(
    manifest: &DatasetManifest,
    artifact: &ArtifactEntry,
    at: &str,
    listed: &HashSet<&str>,
    acquisitions: &HashMap<&str, &super::AcquisitionEntry>,
    issues: &mut Issues,
) {
    if !contracts::is_known(&artifact.contract) {
        issues.error(
            IssueCode::UnsupportedSchema,
            format!("{at}/contract"),
            format!("contract `{}` is not supported", artifact.contract),
        );
    } else if !role_takes(artifact.role, &artifact.contract) {
        issues.error(
            IssueCode::UnsupportedSchema,
            format!("{at}/contract"),
            format!(
                "a {:?} artifact cannot be `{}`",
                artifact.role, artifact.contract
            ),
        );
    } else if !listed.contains(artifact.contract.as_str()) {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/contract"),
            format!(
                "contract `{}` is not listed in `contracts`",
                artifact.contract
            ),
        );
    }
    if !is_sha256(&artifact.semantic_id) {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/semantic_id"),
            "a semantic id is a SHA-256",
        );
    }
    check_location(&artifact.location, &format!("{at}/location"), issues);
    let digests = [
        Some(&artifact.content.sha256),
        artifact.content.values_sha256.as_ref(),
    ];
    if digests
        .into_iter()
        .flatten()
        .any(|digest| !is_sha256(digest))
    {
        issues.error(
            IssueCode::CorruptArtifactHash,
            format!("{at}/content"),
            "content digests are lowercase SHA-256",
        );
    }
    if artifact.role == ArtifactRole::Provenance && !manifest.privacy.discloses_provenance_records()
    {
        issues.error(
            IssueCode::PrivacyViolation,
            format!("{at}/role"),
            "provenance records hold paths and command lines; list them only under a disclosure with `provenance_records`",
        );
    }
    if contracts::ANNOTATIONS.contains(&artifact.contract.as_str())
        && !manifest.privacy.discloses_provenance_records()
    {
        issues.error(
            IssueCode::PrivacyViolation,
            format!("{at}/contract"),
            "population annotation descriptors record the absolute paths of their inputs; list \
             them only under a disclosure with `provenance_records`",
        );
    }
    check_artifact_acquisition(artifact, at, acquisitions, issues);
    if let Some(panel) = &artifact.panel
        && !manifest.panels.iter().any(|p| &p.id == panel)
    {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/panel"),
            format!("panel `{panel}` is not defined"),
        );
    }
    if artifact.role == ArtifactRole::SignalLayer && artifact.layer.is_none() {
        issues.error(
            IssueCode::MissingMetadata,
            format!("{at}/layer"),
            "a signal layer summarizes its processing steps",
        );
    }
}

/// An events artifact names a listed acquisition, recorded on its panel.
fn check_artifact_acquisition(
    artifact: &ArtifactEntry,
    at: &str,
    acquisitions: &HashMap<&str, &super::AcquisitionEntry>,
    issues: &mut Issues,
) {
    match (&artifact.acquisition, per_acquisition(artifact.role)) {
        (None, true) => issues.error(
            IssueCode::MissingMetadata,
            format!("{at}/acquisition"),
            "the artifact holds events and names their acquisition",
        ),
        (Some(id), _) => match acquisitions.get(id.as_str()) {
            None => issues.error(
                IssueCode::InvalidReference,
                format!("{at}/acquisition"),
                "the acquisition is not listed",
            ),
            Some(acquisition) => {
                if artifact
                    .panel
                    .as_ref()
                    .is_some_and(|p| *p != acquisition.panel)
                {
                    issues.error(
                        IssueCode::PanelMismatch,
                        format!("{at}/panel"),
                        format!(
                            "the acquisition was recorded on panel `{}`",
                            acquisition.panel
                        ),
                    );
                }
            }
        },
        (None, false) => {}
    }
}

fn check_parents(
    artifact: &ArtifactEntry,
    at: &str,
    by_id: &HashMap<&str, &ArtifactEntry>,
    issues: &mut Issues,
) {
    for (p, parent_id) in artifact.parents.iter().enumerate() {
        let here = format!("{at}{}", pointer(["parents", &p.to_string()]));
        let Some(parent) = by_id.get(parent_id.as_str()) else {
            issues.error(
                IssueCode::InvalidReference,
                here,
                "the parent is not listed",
            );
            continue;
        };
        if parent.semantic_id == artifact.semantic_id {
            issues.error(
                IssueCode::InvalidReference,
                here,
                "an artifact is not its own parent",
            );
            continue;
        }
        if let (Some(mine), Some(theirs)) = (&artifact.acquisition, &parent.acquisition)
            && mine != theirs
        {
            issues.error(
                IssueCode::InvalidReference,
                here.clone(),
                "the parent holds events of another acquisition",
            );
        }
        if let (ArtifactRole::SignalLayer, ArtifactRole::SignalLayer, Some(child), Some(parent)) =
            (artifact.role, parent.role, &artifact.layer, &parent.layer)
            && !(parent.steps.len() < child.steps.len() && child.steps.starts_with(&parent.steps))
        {
            issues.error(
                IssueCode::IncompatibleSignalState,
                here,
                "the parent layer's steps are not a prefix of this layer's",
            );
        }
    }
}

fn check_execution(
    manifest: &DatasetManifest,
    artifact: &ArtifactEntry,
    at: &str,
    provenance: &HashSet<&str>,
    issues: &mut Issues,
) {
    let Some(execution) = &artifact.execution else {
        return;
    };
    if execution
        .fingerprint
        .as_ref()
        .is_some_and(|f| !is_sha256(f))
    {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/execution/fingerprint"),
            "an execution fingerprint is a SHA-256",
        );
    }
    let Some(record) = &execution.provenance else {
        return;
    };
    if !manifest.privacy.discloses_provenance_records() {
        issues.error(
            IssueCode::PrivacyViolation,
            format!("{at}/execution/provenance"),
            "provenance records are referenced only under a disclosure with `provenance_records`",
        );
    }
    if !provenance.contains(record.as_str()) {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/execution/provenance"),
            "no provenance artifact is stored at this location",
        );
    }
}

/// Step order, base state against the panel, and cytometry.
fn check_layer(layer: &LayerSummary, panel: Option<&PanelEntry>, at: &str, issues: &mut Issues) {
    let at = format!("{at}/layer");
    let mut incompatible = |message: String| {
        issues.error(IssueCode::IncompatibleSignalState, at.clone(), message);
    };
    let mut kinds = HashSet::new();
    let mut rank = 0;
    for step in &layer.steps {
        if !kinds.insert(step.kind) || step.kind.rank() < rank {
            incompatible(format!(
                "`{}` is repeated or out of pipeline order",
                step.kind.as_str()
            ));
        }
        rank = step.kind.rank();
    }
    if kinds.contains(&StepKind::Compensation) && kinds.contains(&StepKind::Unmixing) {
        incompatible("a layer is compensated or unmixed, not both".into());
    }
    if kinds.contains(&StepKind::Normalization) && !kinds.contains(&StepKind::Transform) {
        incompatible("normalized values must be transformed first".into());
    }
    let state = layer
        .steps
        .last()
        .map_or(LayerState::Raw, |s| s.kind.state());
    if layer.state != state {
        incompatible(format!("`state` must be {state:?}, the last step's"));
    }
    if let Some(panel) = panel {
        let external = layer.steps.first().filter(|s| s.origin == Origin::External);
        let base = match external.map(|s| s.kind) {
            Some(StepKind::Compensation) => SignalState::Compensated,
            Some(StepKind::Unmixing) => SignalState::Unmixed,
            _ => SignalState::Raw,
        };
        if base != panel.signal_state {
            incompatible(format!(
                "the panel's files are {:?}, but the layer starts from {base:?}",
                panel.signal_state
            ));
        }
        let forbidden = match panel.cytometry {
            Cytometry::Conventional => &[StepKind::Unmixing][..],
            Cytometry::Spectral => &[StepKind::Compensation][..],
            Cytometry::Cytof => &[StepKind::Compensation, StepKind::Unmixing][..],
        };
        if let Some(step) = layer.steps.iter().find(|s| forbidden.contains(&s.kind)) {
            incompatible(format!(
                "a {:?} panel is not processed by {}",
                panel.cytometry,
                step.kind.as_str()
            ));
        }
    }
    for (s, step) in layer.steps.iter().enumerate() {
        let here = format!("{at}{}", pointer(["steps", &s.to_string()]));
        let needs_definition = matches!(
            step.kind,
            StepKind::Compensation | StepKind::Unmixing | StepKind::Normalization
        );
        match step.origin {
            Origin::External => issues.warning(
                IssueCode::MissingMetadata,
                here,
                format!(
                    "{} by other software: its definition is unknown",
                    step.kind.as_str()
                ),
            ),
            Origin::Declared => issues.warning(
                IssueCode::MissingMetadata,
                here,
                format!(
                    "the {} definition is declared, not verified",
                    step.kind.as_str()
                ),
            ),
            Origin::Recorded | Origin::Verified
                if needs_definition
                    && !step.definition_sha256.as_deref().is_some_and(is_sha256) =>
            {
                issues.error(
                    IssueCode::MissingMetadata,
                    format!("{here}/definition_sha256"),
                    format!("{} names the digest of its definition", step.kind.as_str()),
                );
            }
            _ => {}
        }
    }
}

/// Layers of one panel and state share a recipe: they may be stacked in one
/// table only if they were processed the same way.
fn check_recipes(
    manifest: &DatasetManifest,
    acquisitions: &HashMap<&str, &super::AcquisitionEntry>,
    issues: &mut Issues,
) {
    let mut recipes: BTreeMap<(&str, LayerState), &str> = BTreeMap::new();
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let (Some(layer), Some(acquisition)) = (
            &artifact.layer,
            artifact
                .acquisition
                .as_deref()
                .and_then(|id| acquisitions.get(id)),
        ) else {
            continue;
        };
        let key = (acquisition.panel.as_str(), layer.state);
        match recipes.get(&key) {
            Some(recipe) if *recipe != layer.recipe_id => issues.error(
                IssueCode::IncompatibleSignalState,
                pointer(["artifacts", &index.to_string(), "layer", "recipe_id"]),
                format!(
                    "{:?} layers of panel `{}` were processed differently and cannot be combined",
                    layer.state, acquisition.panel
                ),
            ),
            Some(_) => {}
            None => {
                recipes.insert(key, &layer.recipe_id);
            }
        }
    }
}
