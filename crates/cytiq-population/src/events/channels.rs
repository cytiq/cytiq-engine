//! The channel dictionary: what each stored channel column is, from its
//! `$Pn` keywords and the layer's steps. It is written twice, as Arrow field
//! metadata (`cytiq.channel.*`) and as the JSON footer key
//! `cytiq.events.channels`, which SQL engines read without Arrow.

use std::collections::HashMap;

use cytiq_fcs::Parameter;
use serde::{Deserialize, Serialize};

use super::{FIELD_KEY_PREFIX, LayerDescriptor, StepKind, ValueType};

/// One stored channel. Keyword fields are omitted when the file leaves the
/// keyword out or empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelEntry {
    /// `$Pn` index.
    pub index: u16,
    /// `$PnN`, the column name.
    pub name: String,
    pub value_type: ValueType,
    /// `$PnTYPE`, `unmixed_component` for an unmixed channel, or
    /// `unspecified`.
    pub kind: String,
    /// Steps applied to the channel, in pipeline order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyte: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excitation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<String>,
}

impl ChannelEntry {
    /// A channel without keyword fields.
    #[must_use]
    pub fn new(
        index: u16,
        name: impl Into<String>,
        value_type: ValueType,
        kind: impl Into<String>,
        steps: Vec<StepKind>,
    ) -> Self {
        Self {
            index,
            name: name.into(),
            value_type,
            kind: kind.into(),
            steps,
            label: None,
            analyte: None,
            reporter: None,
            detector: None,
            feature: None,
            excitation: None,
            filter: None,
            voltage: None,
            gain: None,
            range: None,
            amplification: None,
            bits: None,
        }
    }

    /// The keyword fields, by field metadata suffix, in a fixed order.
    fn keywords(&self) -> [(&'static str, &Option<String>); 12] {
        [
            ("label", &self.label),
            ("analyte", &self.analyte),
            ("reporter", &self.reporter),
            ("detector", &self.detector),
            ("feature", &self.feature),
            ("excitation", &self.excitation),
            ("filter", &self.filter),
            ("voltage", &self.voltage),
            ("gain", &self.gain),
            ("range", &self.range),
            ("amplification", &self.amplification),
            ("bits", &self.bits),
        ]
    }

    fn keyword_mut(&mut self, suffix: &str) -> &mut Option<String> {
        match suffix {
            "label" => &mut self.label,
            "analyte" => &mut self.analyte,
            "reporter" => &mut self.reporter,
            "detector" => &mut self.detector,
            "feature" => &mut self.feature,
            "excitation" => &mut self.excitation,
            "filter" => &mut self.filter,
            "voltage" => &mut self.voltage,
            "gain" => &mut self.gain,
            "range" => &mut self.range,
            "amplification" => &mut self.amplification,
            _ => &mut self.bits,
        }
    }

    /// The Arrow field metadata of the channel's column.
    #[must_use]
    pub fn field_metadata(&self) -> HashMap<String, String> {
        let key = |suffix: &str| format!("{FIELD_KEY_PREFIX}{suffix}");
        let mut metadata = HashMap::from([
            (key("index"), self.index.to_string()),
            (key("name"), self.name.clone()),
            (key("value_type"), self.value_type.as_str().to_owned()),
            (key("kind"), self.kind.clone()),
        ]);
        for (suffix, value) in self.keywords() {
            if let Some(value) = value {
                metadata.insert(key(suffix), value.clone());
            }
        }
        if !self.steps.is_empty() {
            let names: Vec<&str> = self.steps.iter().map(|kind| kind.as_str()).collect();
            metadata.insert(key("steps"), names.join(","));
        }
        metadata
    }
}

/// `$Pn` keywords recorded per channel, by field metadata key suffix.
const CHANNEL_KEYWORDS: &[(&str, &str)] = &[
    ("label", "S"),
    ("analyte", "ANALYTE"),
    ("reporter", "TAG"),
    ("detector", "DET"),
    ("feature", "FEATURE"),
    ("excitation", "L"),
    ("filter", "F"),
    ("voltage", "V"),
    ("gain", "G"),
    ("range", "R"),
    ("amplification", "E"),
    ("bits", "B"),
];

fn parameter_keyword<'a>(parameter: &'a Parameter, suffix: &str) -> Option<&'a str> {
    let key = format!("$P{}{suffix}", parameter.index);
    parameter
        .keywords
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&key))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.trim().is_empty())
}

/// The dictionary entries of the descriptor's channels at `stored`, in that
/// order.
///
/// # Panics
/// Never: descriptor channels come from `parameters`.
#[must_use]
pub(crate) fn dictionary(
    descriptor: &LayerDescriptor,
    parameters: &[Parameter],
    stored: &[usize],
) -> Vec<ChannelEntry> {
    stored
        .iter()
        .map(|&position| {
            let channel = &descriptor.channels[position];
            let parameter = parameters
                .iter()
                .find(|p| p.index == channel.index)
                .expect("descriptor channels come from the parameters");
            let unmixed = descriptor.steps.iter().any(|step| {
                step.kind == StepKind::Unmixing && step.channels.contains(&channel.name)
            });
            let kind = if unmixed {
                "unmixed_component".to_owned()
            } else {
                parameter_keyword(parameter, "TYPE")
                    .map_or_else(|| "unspecified".to_owned(), str::to_owned)
            };
            let mut entry = ChannelEntry::new(
                channel.index,
                channel.name.clone(),
                channel.value_type,
                kind,
                descriptor.steps_of(&channel.name),
            );
            for (suffix, keyword) in CHANNEL_KEYWORDS {
                *entry.keyword_mut(suffix) =
                    parameter_keyword(parameter, keyword).map(str::to_owned);
            }
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_metadata_and_json_carry_the_same_fields() {
        let mut entry = ChannelEntry::new(
            3,
            "FL1-A",
            ValueType::Float32,
            "raw_fluorescence",
            vec![StepKind::Compensation, StepKind::Transform],
        );
        entry.label = Some("CD3".into());
        entry.detector = Some("B530".into());
        let metadata = entry.field_metadata();
        assert_eq!(metadata["cytiq.channel.index"], "3");
        assert_eq!(metadata["cytiq.channel.steps"], "compensation,transform");
        assert_eq!(metadata["cytiq.channel.label"], "CD3");
        assert!(!metadata.contains_key("cytiq.channel.reporter"));
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["label"], "CD3");
        assert!(json.get("reporter").is_none());
        assert_eq!(serde_json::from_value::<ChannelEntry>(json).unwrap(), entry);
    }

    #[test]
    fn keyword_suffixes_cover_every_field() {
        let mut entry = ChannelEntry::new(1, "C", ValueType::Uint64, "unspecified", Vec::new());
        for (suffix, _) in CHANNEL_KEYWORDS {
            *entry.keyword_mut(suffix) = Some((*suffix).to_owned());
        }
        for (suffix, value) in entry.keywords() {
            assert_eq!(value.as_deref(), Some(suffix));
        }
    }
}
