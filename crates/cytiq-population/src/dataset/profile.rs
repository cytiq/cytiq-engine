//! Metadata completeness profiles.

use serde::{Deserialize, Serialize};

use super::DatasetManifest;
use crate::issue::{IssueCode, Issues, pointer};

/// Which descriptive metadata a dataset must carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// Identities, references, signal states, channels and privacy only.
    #[default]
    Structural,
    /// `Structural` plus the minimum `MIFlowCyt` description: study purpose,
    /// instrument manufacturer and model per panel, detector, marker and
    /// reporter of every marked channel, and the organism and material of
    /// every sample's specimen.
    MiflowcytMinimal,
}

impl Profile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Structural => "structural",
            Self::MiflowcytMinimal => "miflowcyt-minimal",
        }
    }
}

impl std::str::FromStr for Profile {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "structural" => Ok(Self::Structural),
            "miflowcyt-minimal" => Ok(Self::MiflowcytMinimal),
            other => Err(format!(
                "unknown profile `{other}`; expected `structural` or `miflowcyt-minimal`"
            )),
        }
    }
}

fn missing(issues: &mut Issues, at: String, what: &str) {
    issues.error(
        IssueCode::MissingMetadata,
        at,
        format!("the miflowcyt-minimal profile needs {what}"),
    );
}

fn blank(value: Option<&String>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

/// Report what `profile` requires and `manifest` lacks.
pub(super) fn check(manifest: &DatasetManifest, profile: Profile, issues: &mut Issues) {
    if profile == Profile::Structural {
        return;
    }
    if blank(
        manifest
            .study
            .as_ref()
            .and_then(|study| study.purpose.as_ref()),
    ) {
        missing(issues, "/study".into(), "the study purpose");
    }
    for (p, panel) in manifest.panels.iter().enumerate() {
        let at = pointer(["panels", &p.to_string()]);
        let instrument = panel.instrument.as_ref().and_then(|id| {
            manifest
                .instruments
                .iter()
                .find(|instrument| &instrument.id == id)
        });
        match instrument {
            None => missing(issues, format!("{at}/instrument"), "the panel's instrument"),
            Some(instrument)
                if blank(instrument.manufacturer.as_ref()) || blank(instrument.model.as_ref()) =>
            {
                missing(
                    issues,
                    format!("{at}/instrument"),
                    "the instrument's manufacturer and model",
                );
            }
            Some(_) => {}
        }
        for (c, channel) in panel.channels.iter().enumerate() {
            if channel.marker.is_none() {
                continue;
            }
            let here = format!("{at}{}", pointer(["channels", &c.to_string()]));
            if blank(channel.reporter.as_ref()) {
                missing(
                    issues,
                    format!("{here}/reporter"),
                    "the reporter of every marked channel",
                );
            }
            if blank(channel.detector.as_ref()) {
                missing(
                    issues,
                    format!("{here}/detector"),
                    "the detector of every marked channel",
                );
            }
        }
        if !panel
            .channels
            .iter()
            .any(|channel| channel.marker.is_some())
        {
            missing(
                issues,
                format!("{at}/channels"),
                "at least one marked channel per panel",
            );
        }
    }
    for (s, sample) in manifest.samples.iter().enumerate() {
        let at = pointer(["samples", &s.to_string(), "specimen"]);
        let specimen = sample.specimen.as_ref().and_then(|id| {
            manifest
                .specimens
                .iter()
                .find(|specimen| &specimen.id == id)
        });
        match specimen {
            None => missing(issues, at, "every sample's specimen"),
            Some(specimen)
                if blank(specimen.organism.as_ref()) || blank(specimen.material.as_ref()) =>
            {
                missing(issues, at, "the specimen's organism and material");
            }
            Some(_) => {}
        }
    }
}
