//! Detector margin bounds (flowCore `minRange`/`maxRange`) that survive
//! derived files.
//!
//! Margins are defined on raw detector values. A file written by a cytiq stage
//! records the bounds of each channel in the `CYTIQ_MARGINS` keyword, already
//! mapped into the space of the written values, so later stages never infer
//! them from a regenerated `$PnR`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::FcsError;
use crate::signal::SignalState;
use crate::types::{FcsDataset, FcsText, Parameter};

pub const MARGINS_KEYWORD: &str = "CYTIQ_MARGINS";

/// flowCore clamps the low margin of channels without an explicit range here.
const DEFAULT_LOWER_BOUND: f64 = -111.0;

/// Data-independent margin bounds of one channel. Events at or below
/// `max(lower, min(values))` and at or above `min(upper, max(values))` are
/// margin events.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarginBounds {
    pub lower: f64,
    pub upper: Option<f64>,
}

impl MarginBounds {
    /// Validate bounds before using them to remove events.
    /// # Errors
    /// Rejects non-finite or reversed detector bounds.
    pub fn validate(&self) -> Result<(), FcsError> {
        if !self.lower.is_finite()
            || self
                .upper
                .is_some_and(|upper| !upper.is_finite() || upper < self.lower)
        {
            return Err(FcsError::InvalidKeywordValue {
                key: MARGINS_KEYWORD.to_owned(),
                value: "nonfinite or reversed margin bounds".to_owned(),
            });
        }
        Ok(())
    }

    /// Bounds from raw acquisition keywords, as `PeacoQC::RemoveMargins` sees
    /// them through flowCore: `minRange = min($PnE zero value, max(-111,
    /// min(e)))`, `maxRange = $PnR - 1`, low threshold `max(min(minRange, 0),
    /// min(e))`. That threshold reduces to `max(lower, min(e))` with `lower`
    /// below, so it can be recorded before the data is known.
    #[must_use]
    pub fn from_raw_parameter(parameter: &Parameter) -> Self {
        let explicit_min =
            keyword_f64(parameter, "minRange").or_else(|| keyword_f64(parameter, "min"));
        let lower = match explicit_min {
            Some(minimum) if !minimum.is_finite() => minimum,
            Some(minimum) => minimum.min(0.0),
            None => zero_value(parameter).map_or(DEFAULT_LOWER_BOUND, |zero| {
                if zero.is_finite() {
                    zero.min(DEFAULT_LOWER_BOUND)
                } else {
                    zero
                }
            }),
        };
        let upper = keyword_f64(parameter, &format!("$P{}R", parameter.index))
            .map(|range| range - 1.0)
            .or_else(|| keyword_f64(parameter, "maxRange"))
            .or_else(|| keyword_f64(parameter, "max"));
        Self { lower, upper }
    }

    /// Bounds as flowCore itself reads them: like [`Self::from_raw_parameter`],
    /// except that the `flowCore_$PnRmin`/`flowCore_$PnRmax` keywords a
    /// flowCore-written file carries replace the `-111` floor and `$PnR - 1`.
    /// flowCore's `minRange` is then `min(zero, max(Rmin, min(e)))` and
    /// `maxRange` is `Rmax`, which is what `PeacoQC::RemoveMargins` and
    /// `estimateLogicle` (`t = maxRange`) see.
    #[must_use]
    pub fn from_flowcore_parameter(text: &FcsText, parameter: &Parameter) -> Self {
        let keyword = |suffix: &str| {
            text.get(&format!("flowCore_$P{}{suffix}", parameter.index))
                .and_then(|value| value.trim().parse::<f64>().ok())
        };
        let mut bounds = Self::from_raw_parameter(parameter);
        if let Some(minimum) = keyword("Rmin") {
            bounds.lower = if minimum.is_finite() {
                zero_value(parameter)
                    .map_or(minimum, |zero| zero.min(minimum))
                    .min(0.0)
            } else {
                minimum
            };
        }
        if let Some(maximum) = keyword("Rmax") {
            bounds.upper = Some(maximum);
        }
        bounds
    }

    /// Low and high thresholds for a channel whose values span
    /// `minimum..=maximum`.
    #[must_use]
    pub fn thresholds(&self, minimum: f64, maximum: f64) -> (f64, f64) {
        (
            self.lower.max(minimum),
            self.upper.map_or(maximum, |upper| upper.min(maximum)),
        )
    }

    /// The detector limit alone: events at or above `upper` are on a margin.
    /// Unlike [`Self::thresholds`] (the `PeacoQC::RemoveMargins` rule) neither
    /// the observed extremes nor `lower` count. A discrete channel keeps its
    /// lowest and highest levels, and float data keeps the noise of negative
    /// populations: FCS declares no low detector limit, and flowCore's fixed
    /// -111 floor flags 31% of the events of a real BD sample
    /// (`parity/REPORT.md` §2.10). Integer data never goes below that floor.
    #[must_use]
    pub fn detector_limit(&self) -> f64 {
        self.upper.unwrap_or(f64::INFINITY)
    }

    /// Map the bounds through a monotonically increasing transform.
    #[must_use]
    pub fn map(&self, transform: impl Fn(f64) -> f64) -> Self {
        Self {
            lower: transform(self.lower),
            upper: self.upper.map(transform),
        }
    }
}

/// Margin bounds recorded in `CYTIQ_MARGINS`, keyed by `$PnN`.
///
/// # Errors
///
/// Returns an error when the keyword is present but is not valid JSON.
pub fn recorded_margin_bounds(
    text: &FcsText,
) -> Result<Option<BTreeMap<String, MarginBounds>>, FcsError> {
    text.get(MARGINS_KEYWORD)
        .map(|value| {
            serde_json::from_str(value).map_err(|_| FcsError::InvalidKeywordValue {
                key: MARGINS_KEYWORD.to_owned(),
                value: value.to_owned(),
            })
        })
        .transpose()
}

/// Encode margin bounds as the `CYTIQ_MARGINS` keyword value.
///
/// # Panics
///
/// Never: the map has string keys and finite or null values. Non-finite
/// bounds serialize as `null`.
#[must_use]
pub fn encode_margin_bounds(bounds: &BTreeMap<String, MarginBounds>) -> String {
    serde_json::to_string(bounds).expect("margin bounds serialize to JSON")
}

/// Margin bounds of every parameter whose bounds are known in the space of
/// its stored values: recorded by an earlier cytiq stage, or taken from raw
/// acquisition keywords for channels no cytiq transform, compensation or
/// unmixing has produced. Such a channel without recorded bounds is left out.
///
/// # Errors
///
/// Returns an error when `CYTIQ_MARGINS`, `CYTIQ_TRANSFORM` or
/// `CYTIQ_COMPENSATION` is present but invalid.
pub fn dataset_margin_bounds(
    text: &FcsText,
    parameters: &[Parameter],
) -> Result<BTreeMap<String, MarginBounds>, FcsError> {
    let recorded = recorded_margin_bounds(text)?.unwrap_or_default();
    let state = SignalState::of(text)?;
    // Compensation and unmixing mix channels, so raw bounds no longer
    // describe them either.
    let transformed =
        |name: &str| state.transformed(name) || state.compensated(name) || state.unmixed(name);

    let bounds: BTreeMap<_, _> = parameters
        .iter()
        .filter(|p| {
            !state
                .normalization
                .as_ref()
                .is_some_and(|n| n.channels.contains(&p.name))
        })
        .filter_map(|parameter| {
            let bounds = recorded.get(&parameter.name).copied().or_else(|| {
                (!transformed(&parameter.name)).then(|| MarginBounds::from_raw_parameter(parameter))
            })?;
            Some((parameter.name.clone(), bounds))
        })
        .collect();
    for parameter in parameters {
        if recorded.contains_key(&parameter.name)
            || transformed(&parameter.name)
            || state
                .normalization
                .as_ref()
                .is_some_and(|n| n.channels.contains(&parameter.name))
        {
            continue;
        }
        for (key, value) in &parameter.keywords {
            let range = key == &format!("$P{}R", parameter.index);
            if (range || ["minRange", "maxRange", "min", "max"].contains(&key.as_str()))
                && value
                    .trim()
                    .parse::<f64>()
                    .map_or(true, |v| !v.is_finite() || (range && v <= 0.0))
            {
                return Err(FcsError::InvalidKeywordValue {
                    key: key.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    for value in bounds.values() {
        value.validate()?;
    }
    Ok(bounds)
}

/// Detector saturation diagnostics; negative signal and observed extrema do
/// not define physical detector saturation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SaturationDiagnostics {
    pub schema_version: u32,
    pub rule: &'static str,
    pub events: usize,
    pub removed_count: usize,
    pub channels: Vec<SaturationChannelCounts>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SaturationChannelCounts {
    pub channel: String,
    /// None means the file declares no upper detector bound.
    pub upper: Option<f64>,
    pub removed_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SaturationAssessment {
    pub pass: Vec<bool>,
    pub diagnostics: SaturationDiagnostics,
}

/// The channels an acquisition's raw margins are judged on: `detectors` (the
/// channels compensation or unmixing mixes) followed by every other scatter
/// channel ([`crate::is_scatter_channel`]). Scatter is never mixed, but its
/// detectors saturate like any other: a `FlowCyt` bone-marrow case had 29% of
/// its events at `$PnR - 1` on `FS INT`, and every one of them stayed in the
/// analysis while only the fluorescence detectors were judged.
#[must_use]
pub fn margin_channels(dataset: &FcsDataset, detectors: &[usize]) -> Vec<usize> {
    let scatter = dataset
        .parameters
        .iter()
        .enumerate()
        .filter(|(index, parameter)| {
            !detectors.contains(index) && crate::is_scatter_channel(&parameter.name)
        })
        .map(|(index, _)| index);
    detectors.iter().copied().chain(scatter).collect()
}

/// Per-event pass flags for the detector margins of `indices`: an event
/// fails when it sits at or above the detector limit (`$PnR - 1`) of any
/// listed channel. Only that limit counts ([`MarginBounds::detector_limit`]);
/// `cytiq peacoqc --remove-margins` keeps the upstream rule that also removes
/// the observed extremes and values at or below flowCore's low bound.
/// Compensation and unmixing mix channels, so this must run on raw values
/// before them.
///
/// # Errors
///
/// Returns an error when `CYTIQ_*` state keywords are invalid.
///
pub fn raw_margin_pass(dataset: &FcsDataset, indices: &[usize]) -> Result<Vec<bool>, FcsError> {
    Ok(assess_detector_saturation(dataset, indices)?.pass)
}

/// Assess physical saturation before compensation/unmixing, with counts per
/// detector and a union count. Memory is bounded by events plus channels.
/// # Errors
/// Rejects invalid bounds, non-finite values, unknown processed bounds and indices.
pub fn assess_detector_saturation(
    dataset: &FcsDataset,
    indices: &[usize],
) -> Result<SaturationAssessment, FcsError> {
    if crate::normalization_state(&dataset.text)?.is_some() {
        return Err(FcsError::InvalidKeywordValue {
            key: crate::NORMALIZATION_KEYWORD.to_owned(),
            value: "acquisition margins must be evaluated before normalization".to_owned(),
        });
    }
    let bounds = dataset_margin_bounds(&dataset.text, &dataset.parameters)?;
    let mut pass = vec![true; dataset.event_count()];
    let mut channels = Vec::with_capacity(indices.len());
    for index in indices {
        let parameter =
            dataset
                .parameters
                .get(*index)
                .ok_or_else(|| FcsError::InvalidKeywordValue {
                    key: "margin channel index".to_owned(),
                    value: index.to_string(),
                })?;
        let bounds =
            bounds
                .get(&parameter.name)
                .copied()
                .ok_or_else(|| FcsError::InvalidKeywordValue {
                    key: MARGINS_KEYWORD.to_owned(),
                    value: format!("unknown bounds for {}", parameter.name),
                })?;
        let values = dataset
            .data
            .column(*index)
            .ok_or_else(|| FcsError::InvalidKeywordValue {
                key: parameter.name.clone(),
                value: "missing detector data column".to_owned(),
            })?
            .to_f64_vec();
        let limit = bounds.detector_limit();
        if values.len() != pass.len() {
            return Err(FcsError::InvalidKeywordValue {
                key: parameter.name.clone(),
                value: "detector column length mismatch".to_owned(),
            });
        }
        let mut removed_count = 0;
        for (event, value) in pass.iter_mut().zip(&values) {
            if !value.is_finite() {
                return Err(FcsError::InvalidKeywordValue {
                    key: parameter.name.clone(),
                    value: "nonfinite detector signal".to_owned(),
                });
            }
            removed_count += usize::from(*value >= limit);
            *event &= *value < limit;
        }
        channels.push(SaturationChannelCounts {
            channel: parameter.name.clone(),
            upper: bounds.upper,
            removed_count,
        });
    }
    let diagnostics = SaturationDiagnostics {
        schema_version: 1,
        rule: "detector_saturation",
        events: pass.len(),
        removed_count: pass.iter().filter(|pass| !**pass).count(),
        channels,
    };
    Ok(SaturationAssessment { pass, diagnostics })
}

fn keyword_f64(parameter: &Parameter, key: &str) -> Option<f64> {
    parameter
        .keywords
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
        .and_then(|value| value.trim().parse::<f64>().ok())
}

/// Second field of `$PnE`, which flowCore uses as the channel's zero value.
fn zero_value(parameter: &Parameter) -> Option<f64> {
    parameter
        .keywords
        .iter()
        .find_map(|(key, value)| (*key == format!("$P{}E", parameter.index)).then_some(value))
        .and_then(|value| value.split(',').nth(1))
        .and_then(|value| value.trim().parse::<f64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ParameterDataType, ParameterWidth};

    fn parameter(keywords: &[(&str, &str)]) -> Parameter {
        Parameter {
            index: 1,
            name: "FL1-A".to_owned(),
            label: None,
            width: ParameterWidth::Bits(32),
            data_type: ParameterDataType::Float,
            keywords: keywords
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn text(pairs: &[(&str, &str)]) -> FcsText {
        let pairs = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        let lookup = pairs.iter().cloned().collect();
        FcsText::new(
            crate::types::TextSegment {
                delimiter: b'/',
                pairs,
            },
            None,
            lookup,
        )
    }

    // Reference: the flowCore/PeacoQC expression the bounds replace.
    fn flowcore_thresholds(parameter: &Parameter, values: &[f64]) -> (f64, f64) {
        let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let real_min = DEFAULT_LOWER_BOUND.max(minimum);
        let min_range = keyword_f64(parameter, "minRange")
            .unwrap_or_else(|| zero_value(parameter).map_or(real_min, |zero| zero.min(real_min)));
        let max_range = keyword_f64(parameter, "$P1R").map_or(maximum, |range| range - 1.0);
        (min_range.min(0.0).max(minimum), max_range.min(maximum))
    }

    #[test]
    fn flowcore_keywords_replace_default_bounds() {
        let mut parameter = parameter(&[("$P1R", "262144"), ("$P1E", "0,0")]);
        parameter.index = 8;
        let flowcore = text(&[
            ("flowCore_$P8Rmin", "-56.25"),
            ("flowCore_$P8Rmax", "262144"),
        ]);
        let bounds = MarginBounds::from_flowcore_parameter(&flowcore, &parameter);
        assert_eq!(bounds.lower, -56.25);
        assert_eq!(bounds.upper, Some(262_144.0));
        // flowCore minRange max(-56.25, min(e)) when min(e) is lower
        assert_eq!(bounds.thresholds(-60.0, 1000.0), (-56.25, 1000.0));

        let plain = MarginBounds::from_flowcore_parameter(&text(&[]), &parameter);
        assert_eq!(plain, MarginBounds::from_raw_parameter(&parameter));
    }

    #[test]
    fn raw_thresholds_match_flowcore() {
        type Case<'a> = (&'a [(&'a str, &'a str)], &'a [f64]);
        let cases: [Case; 4] = [
            (&[("$P1R", "1024"), ("$P1E", "0,0")], &[-5.0, 10.0, 1023.0]),
            (&[("$P1R", "1024"), ("$P1E", "0,0")], &[-500.0, 10.0, 900.0]),
            (&[("$P1R", "1024")], &[-500.0, 10.0, 900.0]),
            (
                &[("$P1R", "262144"), ("$P1E", "4,1")],
                &[1.0, 5.0, 262_143.0],
            ),
        ];
        for (keywords, values) in cases {
            let parameter = parameter(keywords);
            let minimum = values.iter().copied().fold(f64::INFINITY, f64::min);
            let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert_eq!(
                MarginBounds::from_raw_parameter(&parameter).thresholds(minimum, maximum),
                flowcore_thresholds(&parameter, values),
                "keywords {keywords:?}, values {values:?}"
            );
        }
    }

    #[test]
    fn raw_margins_keep_negative_noise_and_flag_saturation() {
        let parameters = [crate::FcsWriteParameter::new("FL1-A").with_range(1024.0)];
        let values = vec![-500.0_f32, -111.0, 0.0, 1022.0, 1023.0, 1500.0];
        let mut bytes = Vec::new();
        crate::write_fcs3_1_f32_to_writer(&mut bytes, &parameters, &[values]).unwrap();
        let dataset = crate::FcsReader::new(std::io::Cursor::new(bytes))
            .read()
            .unwrap();

        assert_eq!(
            raw_margin_pass(&dataset, &[0]).unwrap(),
            [true, true, true, true, false, false]
        );
    }

    #[test]
    fn saturation_keeps_a_point_two_percent_population_at_the_observed_maximum() {
        let parameters = [crate::FcsWriteParameter::new("FL1-A").with_range(1024.0)];
        let mut values = vec![-500.0_f32; 998];
        values.extend([1000.0, 1000.0]);
        let mut bytes = Vec::new();
        crate::write_fcs3_1_f32_to_writer(&mut bytes, &parameters, &[values]).unwrap();
        let dataset = crate::FcsReader::new(std::io::Cursor::new(bytes))
            .read()
            .unwrap();
        let result = assess_detector_saturation(&dataset, &[0]).unwrap();
        assert_eq!(result.pass, vec![true; 1000]);
        assert_eq!(result.diagnostics.removed_count, 0);
        assert_eq!(result.diagnostics.channels[0].upper, Some(1023.0));
    }

    #[test]
    fn saturation_diagnostics_preserve_negative_populations_and_count_overlap() {
        let parameters = [
            crate::FcsWriteParameter::new("FL1-A").with_range(1024.0),
            crate::FcsWriteParameter::new("FSC-A").with_range(1024.0),
        ];
        let columns = [
            vec![-500.0_f32, -111.0, 1023.0, 1023.0, 0.0],
            vec![0.0, 0.0, 0.0, 1023.0, 1023.0],
        ];
        let mut bytes = Vec::new();
        crate::write_fcs3_1_f32_to_writer(&mut bytes, &parameters, &columns).unwrap();
        let dataset = crate::FcsReader::new(std::io::Cursor::new(bytes))
            .read()
            .unwrap();
        let result = assess_detector_saturation(&dataset, &[0, 1]).unwrap();
        assert_eq!(result.pass, [true, true, false, false, false]);
        assert_eq!(result.diagnostics.removed_count, 3);
        assert_eq!(
            result
                .diagnostics
                .channels
                .iter()
                .map(|c| c.removed_count)
                .collect::<Vec<_>>(),
            [2, 2]
        );
        assert!(assess_detector_saturation(&dataset, &[2]).is_err());
        for bounds in [
            MarginBounds {
                lower: 0.0,
                upper: Some(f64::NAN),
            },
            MarginBounds {
                lower: f64::NEG_INFINITY,
                upper: None,
            },
            MarginBounds {
                lower: 10.0,
                upper: Some(1.0),
            },
        ] {
            assert!(bounds.validate().is_err());
        }
    }

    #[test]
    fn round_trips_recorded_bounds() {
        let bounds = BTreeMap::from([(
            "FL1-A".to_owned(),
            MarginBounds {
                lower: -0.25,
                upper: Some(4.5),
            },
        )]);
        let decoded: BTreeMap<String, MarginBounds> =
            serde_json::from_str(&encode_margin_bounds(&bounds)).unwrap();
        assert_eq!(decoded, bounds);
    }
}
