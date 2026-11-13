//! Build manifest entries from a run manifest and the acquired files.

use std::collections::HashMap;
use std::path::Path;

use cytiq_fcs::{FcsMetadata, ORIGINAL_ID_PARAMETER, Parameter};
use cytiq_manifest::{Cytometry, Marker, Panel, RunManifest};

use super::{AcquisitionEntry, AcquisitionRole, ChannelEntry, ChannelKind, PanelEntry};
use crate::privacy::PrivacyPolicy;
use crate::{PopulationError, Result, require};

fn keyword<'a>(parameter: &'a Parameter, suffix: &str) -> Option<&'a str> {
    let key = format!("$P{}{suffix}", parameter.index);
    parameter
        .keywords
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&key))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// FCS 3.2 `$PnTYPE`, ignoring case and `_`/space/`-` differences.
fn kind_of(value: &str) -> ChannelKind {
    let normalized: String = value
        .chars()
        .map(|c| {
            if c == ' ' || c == '-' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    match normalized.as_str() {
        "forward_scatter" => ChannelKind::ForwardScatter,
        "side_scatter" => ChannelKind::SideScatter,
        "raw_fluorescence" => ChannelKind::Fluorescence,
        "unmixed_fluorescence" => ChannelKind::UnmixedComponent,
        "mass" => ChannelKind::Mass,
        "time" => ChannelKind::Time,
        _ => ChannelKind::Other,
    }
}

/// The channel a marker is on: its `channel` (`$PnN`), else the parameter
/// whose `$PnS` is the marker name, as `cytiq preflight` matches them.
fn locate<'a>(marker: &Marker, parameters: &'a [&'a Parameter]) -> Option<&'a Parameter> {
    match &marker.channel {
        Some(channel) => parameters.iter().copied().find(|p| &p.name == channel),
        None => parameters.iter().copied().find(|p| {
            p.label
                .as_deref()
                .is_some_and(|label| label.trim().eq_ignore_ascii_case(marker.marker.trim()))
        }),
    }
}

/// The panel entry of `panel`, with the channels of `metadata` (a raw file
/// of the panel) in `$Pn` order. Markers, reporters, detectors and reagents
/// come from the run manifest, or from the FCS 3.2 `$PnANALYTE`, `$PnTAG`
/// and `$PnDET` keywords; `$PnS` is used only to locate a marker without a
/// `channel`, never copied as a marker or reporter.
///
/// # Errors
/// Refuses a required marker that no channel carries, and two markers on
/// one channel.
pub fn panel_from_run(
    run: &RunManifest,
    panel: &Panel,
    metadata: &FcsMetadata,
) -> Result<PanelEntry> {
    let parameters: Vec<&Parameter> = metadata
        .parameters
        .iter()
        .filter(|p| p.name != ORIGINAL_ID_PARAMETER)
        .collect();
    let mut assigned: HashMap<u16, &Marker> = HashMap::new();
    for marker in &panel.markers {
        let Some(parameter) = locate(marker, &parameters) else {
            require(
                !marker.required,
                format!(
                    "panel `{}`: no channel carries marker `{}`",
                    panel.label(),
                    marker.marker
                ),
            )?;
            continue;
        };
        if assigned.insert(parameter.index, marker).is_some() {
            return Err(PopulationError::Invalid(format!(
                "panel `{}`: channel `{}` carries two markers",
                panel.label(),
                parameter.name
            )));
        }
    }
    let channels = parameters
        .iter()
        .map(|parameter| {
            let marker = assigned.get(&parameter.index).copied();
            let kind = keyword(parameter, "TYPE").map(kind_of).or_else(|| {
                marker.map(|_| match run.cytometry {
                    Cytometry::Cytof => ChannelKind::Mass,
                    Cytometry::Conventional | Cytometry::Spectral => ChannelKind::Fluorescence,
                })
            });
            ChannelEntry {
                index: parameter.index,
                name: parameter.name.clone(),
                kind,
                detector: marker
                    .and_then(|m| m.detector.clone())
                    .or_else(|| keyword(parameter, "DET").map(str::to_owned)),
                marker: marker
                    .map(|m| m.marker.clone())
                    .or_else(|| keyword(parameter, "ANALYTE").map(str::to_owned)),
                reporter: marker
                    .and_then(|m| m.reporter.clone())
                    .or_else(|| keyword(parameter, "TAG").map(str::to_owned)),
                reagent: marker.and_then(|m| m.reagent.clone()),
            }
        })
        .collect();
    Ok(PanelEntry {
        id: panel.label().to_owned(),
        cytometry: run.cytometry,
        signal_state: panel.signal_state,
        instrument: panel.instrument.clone(),
        channels,
    })
}

/// The acquisition entry of the raw FCS file at `path`, exporting only the
/// dataset-level keywords `policy` allows.
///
/// # Errors
/// Refuses a file a cytiq stage derived (describe the raw acquisition) and
/// returns read errors.
pub fn acquisition_entry(
    path: &Path,
    metadata: &FcsMetadata,
    panel: &str,
    role: AcquisitionRole,
    policy: &PrivacyPolicy,
) -> Result<AcquisitionEntry> {
    require(
        metadata.text.get(cytiq_fcs::ACQUISITION_KEYWORD).is_none(),
        "describe an acquisition from its raw FCS file, not a derived one",
    )?;
    let id = cytiq_fcs::acquisition_id(path, &metadata.text)?;
    let keywords = policy.export(
        metadata
            .text
            .iter()
            .filter(|(key, _)| !is_parameter_keyword(key)),
    );
    Ok(AcquisitionEntry {
        id,
        panel: panel.to_owned(),
        raw_events: metadata.event_count as u64,
        role,
        keywords,
        source: None,
    })
}

/// `$P<n>...`: channel keywords, described by the panel instead.
fn is_parameter_keyword(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() > 2
        && bytes[0] == b'$'
        && bytes[1].eq_ignore_ascii_case(&b'P')
        && bytes[2].is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_types_map_to_channel_kinds() {
        assert_eq!(kind_of("Raw Fluorescence"), ChannelKind::Fluorescence);
        assert_eq!(kind_of("Forward_Scatter"), ChannelKind::ForwardScatter);
        assert_eq!(kind_of("side-scatter"), ChannelKind::SideScatter);
        assert_eq!(
            kind_of("Unmixed_Fluorescence"),
            ChannelKind::UnmixedComponent
        );
        assert_eq!(kind_of("Mass"), ChannelKind::Mass);
        assert_eq!(kind_of("TIME"), ChannelKind::Time);
        assert_eq!(kind_of("Electronic_Volume"), ChannelKind::Other);
    }

    #[test]
    fn channel_keywords_are_recognized() {
        assert!(is_parameter_keyword("$P1N") && is_parameter_keyword("$p12S"));
        assert!(
            !is_parameter_keyword("$PAR")
                && !is_parameter_keyword("$P")
                && !is_parameter_keyword("$CYT")
        );
    }
}
