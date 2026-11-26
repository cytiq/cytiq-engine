//! `$PnTYPE`: the measurement types FCS 3.2 defines, and the layer a file
//! declares through them.
//!
//! FCS 3.2, 3.3.55: "The values Forward Scatter, Side Scatter, Raw
//! Fluorescence, Unmixed Fluorescence, Mass, Time, Electronic Volume,
//! Classification and Index shall be used when appropriate." 3.2.14: "Keyword
//! values are case sensitive."
//!
//! One extension, and only one: `_` reads as a space. Every file in the
//! validation corpus that sets `$PnTYPE` (514 `SpectroFlo` files, FCS 3.0 and
//! 3.1, which predate the keyword) writes `Unmixed_Fluorescence`,
//! `Raw_Fluorescence`, `Forward_Scatter` and `Side_Scatter`. Case is never
//! folded and nothing is trimmed: any other value, such as `Fluorescence`,
//! declares nothing, and the caller's statement about the layer stands.

use crate::types::Parameter;

/// A measurement type FCS 3.2 defines (3.3.55).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasurementType {
    ForwardScatter,
    SideScatter,
    RawFluorescence,
    UnmixedFluorescence,
    Mass,
    Time,
    ElectronicVolume,
    Classification,
    Index,
}

const VOCABULARY: [(&str, MeasurementType); 9] = [
    ("Forward Scatter", MeasurementType::ForwardScatter),
    ("Side Scatter", MeasurementType::SideScatter),
    ("Raw Fluorescence", MeasurementType::RawFluorescence),
    ("Unmixed Fluorescence", MeasurementType::UnmixedFluorescence),
    ("Mass", MeasurementType::Mass),
    ("Time", MeasurementType::Time),
    ("Electronic Volume", MeasurementType::ElectronicVolume),
    ("Classification", MeasurementType::Classification),
    ("Index", MeasurementType::Index),
];

impl MeasurementType {
    /// The type a `$PnTYPE` value names, or `None` for a value FCS 3.2 does
    /// not define. Exact and case sensitive, except that `_` reads as a
    /// space.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        VOCABULARY.iter().find_map(|(name, kind)| {
            (value.len() == name.len()
                && value
                    .bytes()
                    .zip(name.bytes())
                    .all(|(found, wanted)| found == wanted || (found == b'_' && wanted == b' ')))
            .then_some(*kind)
        })
    }

    /// The value as FCS 3.2 spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        VOCABULARY
            .iter()
            .find_map(|(name, kind)| (*kind == self).then_some(*name))
            .unwrap_or_default()
    }
}

/// What the measurements of a data set declare in `$PnTYPE`, by `$PnN`.
/// Only measurements 1 to `$PAR` count: a `$PnTYPE` for any other `n`
/// describes no measurement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclaredTypes {
    /// `Raw Fluorescence`.
    pub raw: Vec<String>,
    /// `Unmixed Fluorescence`.
    pub unmixed: Vec<String>,
    /// Values FCS 3.2 does not define, as `(name, value)`.
    pub unknown: Vec<(String, String)>,
}

impl DeclaredTypes {
    /// The `$PnTYPE` of every parameter. A parameter without one, or with
    /// an empty one, declares nothing.
    #[must_use]
    pub fn of(parameters: &[Parameter]) -> Self {
        let mut declared = Self::default();
        for parameter in parameters {
            let key = format!("$P{}TYPE", parameter.index);
            let Some(value) = parameter
                .keywords
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&key))
                .map(|(_, value)| value.as_str())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            match MeasurementType::parse(value) {
                Some(MeasurementType::RawFluorescence) => declared.raw.push(parameter.name.clone()),
                Some(MeasurementType::UnmixedFluorescence) => {
                    declared.unmixed.push(parameter.name.clone());
                }
                Some(_) => {}
                None => declared
                    .unknown
                    .push((parameter.name.clone(), value.to_owned())),
            }
        }
        declared
    }

    /// Raw and unmixed channels together: no single layer.
    #[must_use]
    pub fn is_mixed(&self) -> bool {
        !self.raw.is_empty() && !self.unmixed.is_empty()
    }
}

/// `` `A`, `B` `` for a message.
#[must_use]
pub fn quoted_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ParameterDataType, ParameterWidth};

    fn parameter(index: u16, name: &str, kind: Option<&str>) -> Parameter {
        Parameter {
            index,
            name: name.into(),
            label: None,
            width: ParameterWidth::Bits(32),
            data_type: ParameterDataType::Float,
            keywords: kind
                .map(|kind| vec![(format!("$P{index}TYPE"), kind.to_owned())])
                .unwrap_or_default(),
        }
    }

    #[test]
    fn the_standard_values_and_their_underscore_spelling_are_read() {
        for (name, kind) in VOCABULARY {
            assert_eq!(MeasurementType::parse(name), Some(kind), "{name}");
            assert_eq!(MeasurementType::parse(&name.replace(' ', "_")), Some(kind));
            assert_eq!(kind.as_str(), name);
        }
    }

    #[test]
    fn other_values_are_not_types() {
        for value in [
            "Fluorescence",
            "unmixed_fluorescence",
            "UNMIXED FLUORESCENCE",
            "Unmixed  Fluorescence",
            "Unmixed-Fluorescence",
            " Unmixed Fluorescence",
            "Unmixed Fluorescence ",
            "Unmixed",
            "time",
            "",
            "Mass_",
        ] {
            assert_eq!(MeasurementType::parse(value), None, "{value:?}");
        }
    }

    #[test]
    fn declared_types_name_their_channels() {
        let declared = DeclaredTypes::of(&[
            parameter(1, "FSC-A", Some("Forward_Scatter")),
            parameter(2, "B1-A", Some("Raw_Fluorescence")),
            parameter(3, "PE-A", Some("Unmixed Fluorescence")),
            parameter(4, "FL4-A", Some("Fluorescence")),
            parameter(5, "FL5-A", None),
            parameter(6, "FL6-A", Some("")),
        ]);
        assert_eq!(declared.raw, ["B1-A"]);
        assert_eq!(declared.unmixed, ["PE-A"]);
        assert_eq!(
            declared.unknown,
            [("FL4-A".to_owned(), "Fluorescence".to_owned())]
        );
        assert!(declared.is_mixed());
        assert!(!DeclaredTypes::of(&[parameter(1, "A", Some("Unmixed_Fluorescence"))]).is_mixed());
    }

    #[test]
    fn the_keyword_name_is_case_insensitive_and_belongs_to_its_parameter() {
        let mut lower = parameter(2, "PE-A", None);
        lower.keywords = vec![("$p2type".into(), "Unmixed_Fluorescence".into())];
        let mut other = parameter(3, "APC-A", None);
        other.keywords = vec![("$P2TYPE".into(), "Unmixed_Fluorescence".into())];
        let declared = DeclaredTypes::of(&[lower, other]);
        assert_eq!(declared.unmixed, ["PE-A"]);
    }
}
