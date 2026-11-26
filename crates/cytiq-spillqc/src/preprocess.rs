//! Raw FCS to `CompensAID`'s input, following the `CompensAID` vignette chain:
//! `PeacoQC::RemoveMargins`, `flowCore::compensate`, `estimateLogicle` +
//! `transform` over the matrix channels, `PeacoQC`, then the marker channels.
//!
//! Values stay in double precision between the steps, as in flowCore
//! (`compensate` and `transform` do not round to the single precision of a
//! `flowFrame` built from scratch).

#![allow(clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::Path;

use cytiq_compensate::CompensationMatrix;
use cytiq_fcs::{
    FcsDataset, MarginBounds, Parameter, SOURCE_SPILLOVER_KEYWORD, SignalState, Spillover,
};
use cytiq_peacoqc::PeacoQcConfig;
use cytiq_transform::scales::{Logicle, estimate_logicle_width};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::compensaid::MarkerChannel;
use crate::error::SpillQcError;

/// flowCore's `estimateLogicle(m = 4.5, q = 0.05)` and `a = 0`.
const LOGICLE_M: f64 = 4.5;
const LOGICLE_Q: f64 = 0.05;
const LOGICLE_A: f64 = 0.0;
/// `T` when a channel has no `$PnR` (flowCore's `logicleTransform` default).
const DEFAULT_LOGICLE_T: f64 = 262_144.0;

/// Which steps of the vignette chain run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreprocessOptions {
    /// The stored values are already compensated and transformed
    /// (`CompensAID`'s own input contract): no step runs.
    pub preprocessed: bool,
    pub remove_margins: bool,
    pub peacoqc: bool,
    /// Channels to assess; `None` selects flowCore's `markernames`.
    pub channels: Option<Vec<String>>,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            preprocessed: false,
            remove_margins: true,
            peacoqc: true,
            channels: None,
        }
    }
}

/// The compensation matrix applied to (or recorded in) a file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MatrixRecord {
    /// `fcs` (the file's own spillover keyword) or `file` (`--matrix`).
    pub source: &'static str,
    /// SHA-256 of the channels and compensation values: files sharing it were
    /// compensated with the same matrix.
    pub id: String,
    pub channels: Vec<String>,
    /// Whether the matrix was applied by this run.
    pub applied: bool,
}

/// Logicle parameters estimated for one channel.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogicleRecord {
    pub channel: String,
    pub t: f64,
    pub w: f64,
    pub m: f64,
    pub a: f64,
}

/// Marker columns ready for `CompensAID`.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    pub markers: Vec<MarkerChannel>,
    pub columns: Vec<Vec<f64>>,
    pub events_total: usize,
    pub margin_removed: usize,
    pub peacoqc_removed: usize,
    pub matrix: Option<MatrixRecord>,
    pub logicle: Vec<LogicleRecord>,
    pub warnings: Vec<String>,
}

/// A prepared file, or why it cannot be assessed.
#[derive(Clone, Debug, PartialEq)]
pub enum Preparation {
    Ready(Box<Prepared>),
    Skipped(String),
}

/// An external matrix (`--matrix`), read once for every input.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalMatrix {
    pub matrix: CompensationMatrix,
}

/// Prepare `dataset` for `CompensAID`.
///
/// # Errors
///
/// Returns an error when the input was already compensated, unmixed or
/// transformed by cytiq (without `preprocessed`). Data problems specific to
/// the file (no matrix, missing channels, too few markers, an unusable logicle
/// width, `PeacoQC` failing) skip it instead.
pub fn prepare(
    path: &Path,
    dataset: &FcsDataset,
    external: Option<&ExternalMatrix>,
    options: &PreprocessOptions,
) -> Result<Preparation, SpillQcError> {
    if options.preprocessed {
        return Ok(prepare_stored(dataset, external, options));
    }
    ensure_raw(path, dataset)?;

    let matrix = match external {
        Some(external) => external.matrix.clone(),
        None => match Spillover::from_dataset(dataset) {
            Ok(Some(spillover)) => match CompensationMatrix::from_spillover(&spillover) {
                Ok(matrix) => matrix,
                Err(error) => return Ok(skip(format!("spillover matrix is unusable: {error}"))),
            },
            Ok(None) => {
                return Ok(skip(
                    "no `$SPILLOVER`/`$SPILL` keyword; pass --matrix with --matrix-kind",
                ));
            }
            Err(error) => return Ok(skip(format!("spillover keyword is invalid: {error}"))),
        },
    };
    let mut indices = Vec::with_capacity(matrix.channels.len());
    for channel in &matrix.channels {
        match dataset.find_parameter(channel) {
            Some(index) => indices.push(index),
            None => {
                return Ok(skip(format!(
                    "matrix channel `{channel}` is not in the file"
                )));
            }
        }
    }
    let state = SignalState::of(&dataset.text).map_err(|source| SpillQcError::FcsRead {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(index) = indices
        .iter()
        .find(|index| state.transformed(&dataset.parameters[**index].name))
    {
        return Err(SpillQcError::TransformedInput {
            path: path.to_path_buf(),
            channel: dataset.parameters[*index].name.clone(),
        });
    }
    let markers = match select_markers(dataset, &indices, options) {
        Ok(markers) => markers,
        Err(reason) => return Ok(skip(reason)),
    };

    let events_total = dataset.event_count();
    let mut warnings = Vec::new();
    // PeacoQC::RemoveMargins on the raw values of the matrix channels
    let mut keep = vec![true; events_total];
    if options.remove_margins {
        for &index in &indices {
            let values = stored_values(dataset, index);
            let bounds = flowcore_bounds(dataset, &dataset.parameters[index]);
            let (minimum, maximum) = extremes(&values);
            let (low, high) = bounds.thresholds(minimum, maximum);
            for (flag, value) in keep.iter_mut().zip(&values) {
                *flag &= *value > low && *value < high;
            }
        }
    }
    let margin_removed = keep.iter().filter(|flag| !**flag).count();
    if margin_removed * 10 > events_total {
        warnings.push(format!(
            "{margin_removed} of {events_total} events are margin events"
        ));
    }
    let kept = (0..events_total)
        .filter(|event| keep[*event])
        .collect::<Vec<_>>();
    if kept.is_empty() {
        return Ok(skip("every event is a margin event"));
    }

    // flowCore::compensate on the linearised values
    let linear = indices
        .iter()
        .map(|&index| {
            let values = cytiq_fcs::linear_values(dataset, index);
            kept.iter().map(|&event| values[event]).collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let compensated = cytiq_fcs::compensate(&linear, &matrix.values);

    // estimateLogicle + transform, M = 4.5, T = maxRange
    let (logicle, transformed) = match prepare_logicle_columns(dataset, &indices, &compensated) {
        Ok(columns) => columns,
        Err(reason) => return Ok(skip(reason)),
    };

    let mut good = vec![true; kept.len()];
    if options.peacoqc {
        let names = indices
            .iter()
            .map(|&index| dataset.parameters[index].name.clone())
            .collect::<Vec<_>>();
        let channels = names
            .iter()
            .zip(&transformed)
            .map(|(name, values)| (name.as_str(), values.as_slice()))
            .collect::<Vec<_>>();
        match cytiq_peacoqc::peacoqc_mask(path, &channels, &PeacoQcConfig::new(names.clone())) {
            Ok(mask) => good = mask,
            Err(error) => {
                return Ok(skip(format!(
                    "PeacoQC failed ({error}); pass --no-peacoqc to assess without it"
                )));
            }
        }
    }
    let peacoqc_removed = good.iter().filter(|flag| !**flag).count();

    let columns = markers
        .iter()
        .map(|(_, index)| {
            let position = indices
                .iter()
                .position(|candidate| candidate == index)
                .expect("markers are matrix channels");
            transformed[position]
                .iter()
                .zip(&good)
                .filter_map(|(value, keep)| keep.then_some(*value))
                .collect::<Vec<_>>()
        })
        .collect();

    Ok(Preparation::Ready(Box::new(Prepared {
        markers: markers.into_iter().map(|(marker, _)| marker).collect(),
        columns,
        events_total,
        margin_removed,
        peacoqc_removed,
        matrix: Some(MatrixRecord {
            source: if external.is_some() { "file" } else { "fcs" },
            id: matrix_id(&matrix),
            channels: matrix.channels.clone(),
            applied: true,
        }),
        logicle,
        warnings,
    })))
}

fn prepare_logicle_columns(
    dataset: &FcsDataset,
    indices: &[usize],
    compensated: &[Vec<f64>],
) -> Result<(Vec<LogicleRecord>, Vec<Vec<f64>>), String> {
    let mut logicle = Vec::with_capacity(indices.len());
    let mut transformed = Vec::with_capacity(indices.len());
    for (&index, values) in indices.iter().zip(compensated) {
        let parameter = &dataset.parameters[index];
        let t = flowcore_bounds(dataset, parameter)
            .upper
            .unwrap_or(DEFAULT_LOGICLE_T);
        let scale = estimate_logicle_width(values, t, LOGICLE_M, LOGICLE_Q)
            .and_then(|w| Logicle::new(t, w, LOGICLE_M, LOGICLE_A).map(|scale| (w, scale)));
        let (w, scale) = scale.map_err(|error| {
            format!("logicle estimate for `{}` failed: {error}", parameter.name)
        })?;
        logicle.push(LogicleRecord {
            channel: parameter.name.clone(),
            t,
            w,
            m: LOGICLE_M,
            a: LOGICLE_A,
        });
        transformed.push(
            values
                .iter()
                .map(|value| LOGICLE_M * scale.scale(*value))
                .collect::<Vec<_>>(),
        );
    }

    Ok((logicle, transformed))
}

/// `--preprocessed`: the stored values of the marker channels, as
/// `flowCore::read.FCS` returns them.
fn prepare_stored(
    dataset: &FcsDataset,
    external: Option<&ExternalMatrix>,
    options: &PreprocessOptions,
) -> Preparation {
    let candidates = (0..dataset.parameters.len()).collect::<Vec<_>>();
    let markers = match select_markers(dataset, &candidates, options) {
        Ok(markers) => markers,
        Err(reason) => return skip(reason),
    };
    let matrix = match external {
        Some(external) => Some((external.matrix.clone(), "file")),
        None => recorded_spillover(dataset).map(|matrix| (matrix, "fcs")),
    };
    Preparation::Ready(Box::new(Prepared {
        columns: markers
            .iter()
            .map(|(_, index)| stored_values(dataset, *index))
            .collect(),
        markers: markers.into_iter().map(|(marker, _)| marker).collect(),
        events_total: dataset.event_count(),
        margin_removed: 0,
        peacoqc_removed: 0,
        matrix: matrix.map(|(matrix, source)| MatrixRecord {
            source,
            id: matrix_id(&matrix),
            channels: matrix.channels,
            applied: false,
        }),
        logicle: Vec::new(),
        warnings: Vec::new(),
    }))
}

fn ensure_raw(path: &Path, dataset: &FcsDataset) -> Result<(), SpillQcError> {
    let components = cytiq_fcs::DeclaredTypes::of(&dataset.parameters).unmixed;
    if !components.is_empty() {
        return Err(SpillQcError::UnmixedInput {
            path: path.to_path_buf(),
            components,
        });
    }
    let state = SignalState::of(&dataset.text).map_err(|source| SpillQcError::FcsRead {
        path: path.to_path_buf(),
        source,
    })?;
    if let Some(compensation) = state.compensation {
        return Err(SpillQcError::AlreadyCompensated {
            path: path.to_path_buf(),
            channels: compensation.channels,
        });
    }
    if let Some(unmixing) = state.unmixing {
        return Err(SpillQcError::UnmixedInput {
            path: path.to_path_buf(),
            components: unmixing.components,
        });
    }
    Ok(())
}

/// The spillover the stored values were compensated with, when the file still
/// names it (its own keyword, or the one `cytiq compensate` moved aside).
fn recorded_spillover(dataset: &FcsDataset) -> Option<CompensationMatrix> {
    let spillover = match Spillover::from_dataset(dataset) {
        Ok(Some(spillover)) => spillover,
        _ => Spillover::parse_keyword(dataset.text.get(SOURCE_SPILLOVER_KEYWORD)?).ok()?,
    };
    CompensationMatrix::from_spillover(&spillover).ok()
}

/// The assessed channels among `candidates` (dataset indices): the requested
/// ones, or flowCore's `markernames` (a `$PnS` label, not time or scatter).
/// Falls back to every candidate when none is labelled and the candidates are
/// matrix channels.
fn select_markers(
    dataset: &FcsDataset,
    candidates: &[usize],
    options: &PreprocessOptions,
) -> Result<Vec<(MarkerChannel, usize)>, String> {
    let marker = |index: usize| {
        let parameter = &dataset.parameters[index];
        MarkerChannel {
            channel: parameter.name.clone(),
            marker: label(parameter).unwrap_or(&parameter.name).to_owned(),
        }
    };
    let selected = if let Some(requested) = &options.channels {
        let mut selected = Vec::with_capacity(requested.len());
        for channel in requested {
            let Some(index) = dataset.find_parameter(channel) else {
                return Err(format!("channel `{channel}` is not in the file"));
            };
            if !candidates.contains(&index) {
                return Err(format!(
                    "channel `{channel}` is not compensated by the matrix"
                ));
            }
            if selected.contains(&index) {
                return Err(format!("channel `{channel}` is listed twice"));
            }
            selected.push(index);
        }
        selected
    } else {
        let labelled = candidates
            .iter()
            .copied()
            .filter(|index| {
                let parameter = &dataset.parameters[*index];
                label(parameter).is_some() && !is_time_or_scatter(&parameter.name)
            })
            .collect::<Vec<_>>();
        if labelled.is_empty() && !options.preprocessed {
            candidates.to_vec()
        } else {
            labelled
        }
    };
    if selected.len() < 2 {
        return Err(format!(
            "{} marker channel(s) to assess; at least two are needed (label channels with $PnS or pass --channels)",
            selected.len()
        ));
    }
    Ok(selected
        .into_iter()
        .map(|index| (marker(index), index))
        .collect())
}

fn label(parameter: &Parameter) -> Option<&str> {
    parameter
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
}

/// flowCore's `markernames` drops `grepl("time|fsc|ssc", name, ignore.case = TRUE)`.
fn is_time_or_scatter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["time", "fsc", "ssc"]
        .iter()
        .any(|pattern| name.contains(pattern))
}

fn flowcore_bounds(dataset: &FcsDataset, parameter: &Parameter) -> MarginBounds {
    MarginBounds::from_flowcore_parameter(&dataset.text, parameter)
}

fn stored_values(dataset: &FcsDataset, index: usize) -> Vec<f64> {
    dataset
        .data
        .column(index)
        .expect("every parameter has a data column")
        .to_f64_vec()
}

fn extremes(values: &[f64]) -> (f64, f64) {
    values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
            (low.min(*value), high.max(*value))
        })
}

/// SHA-256 over the channel names and the exact compensation values.
fn matrix_id(matrix: &CompensationMatrix) -> String {
    let mut hasher = Sha256::new();
    for channel in &matrix.channels {
        hasher.update(channel.as_bytes());
        hasher.update([0]);
    }
    for value in &matrix.values {
        hasher.update(value.to_bits().to_le_bytes());
    }
    hasher
        .finalize()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn skip(reason: impl Into<String>) -> Preparation {
    Preparation::Skipped(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markernames_drop_time_and_scatter() {
        assert!(is_time_or_scatter("FSC-A"));
        assert!(is_time_or_scatter("Time"));
        assert!(is_time_or_scatter("ssc-h"));
        assert!(!is_time_or_scatter("PE-Cy7-A"));
    }

    #[test]
    fn matrix_id_depends_on_channels_and_values() {
        let matrix = |channels: &[&str], values: &[f64]| CompensationMatrix {
            channels: channels
                .iter()
                .map(|channel| (*channel).to_owned())
                .collect(),
            values: values.to_vec(),
        };
        let base = matrix_id(&matrix(&["A", "B"], &[1.0, 0.0, 0.1, 1.0]));
        assert_eq!(base.len(), 64);
        assert_eq!(base, matrix_id(&matrix(&["A", "B"], &[1.0, 0.0, 0.1, 1.0])));
        assert_ne!(base, matrix_id(&matrix(&["A", "C"], &[1.0, 0.0, 0.1, 1.0])));
        assert_ne!(base, matrix_id(&matrix(&["A", "B"], &[1.0, 0.0, 0.2, 1.0])));
    }

    use cytiq_fcs::{FcsReader, FcsWriteOptions, FcsWriteParameter};
    use std::io::Cursor;

    fn dataset(columns: &[Vec<f32>], keywords: &[(&str, &str)]) -> FcsDataset {
        let parameters = [
            FcsWriteParameter::with_label("A", "CD3").with_range(100.0),
            FcsWriteParameter::with_label("B", "CD4").with_range(100.0),
        ];
        let options = FcsWriteOptions::new()
            .with_text_keywords(keywords.iter().map(|(k, v)| ((*k).into(), (*v).into())));
        let mut bytes = Vec::new();
        cytiq_fcs::write_fcs3_1_f32_to_writer_with_options(
            &mut bytes,
            &parameters,
            columns,
            &options,
        )
        .unwrap();
        FcsReader::new(Cursor::new(bytes)).read().unwrap()
    }
    fn columns() -> Vec<Vec<f32>> {
        vec![
            vec![0.0, 10.0, 20.0, 30.0, 100.0],
            vec![0.0, 20.0, 30.0, 40.0, 100.0],
        ]
    }
    fn identity_matrix() -> ExternalMatrix {
        ExternalMatrix {
            matrix: CompensationMatrix {
                channels: vec!["A".into(), "B".into()],
                values: vec![1.0, 0.0, 0.0, 1.0],
            },
        }
    }
    fn options() -> PreprocessOptions {
        PreprocessOptions {
            peacoqc: false,
            ..PreprocessOptions::default()
        }
    }

    #[test]
    fn margin_warning_uses_the_fraction_of_events_removed() {
        let a = (0_u16..20).map(f32::from).collect::<Vec<_>>();
        let mut b = a.clone();
        b[1] = 19.0;
        let data = dataset(&[a.clone(), b], &[]);
        let output = ready(prepare(
            Path::new("x"),
            &data,
            Some(&identity_matrix()),
            &options(),
        ));
        assert_eq!(output.margin_removed, 3);
        assert_eq!(output.warnings, ["3 of 20 events are margin events"]);
        let data = dataset(&[a.clone(), a], &[]);
        let output = ready(prepare(
            Path::new("x"),
            &data,
            Some(&identity_matrix()),
            &options(),
        ));
        assert_eq!(output.margin_removed, 2);
        assert_eq!(output.warnings, [] as [&str; 0]);
    }
    fn ready(result: Result<Preparation, SpillQcError>) -> Box<Prepared> {
        let Preparation::Ready(prepared) = result.unwrap() else {
            panic!("expected prepared data");
        };
        prepared
    }

    #[test]
    fn preparation_removes_margins_before_transforming_and_keeps_requested_marker_order() {
        let data = dataset(&columns(), &[]);
        let external = identity_matrix();
        let options = PreprocessOptions {
            channels: Some(vec!["B".into(), "A".into()]),
            ..options()
        };
        let output = ready(prepare(
            Path::new("x.fcs"),
            &data,
            Some(&external),
            &options,
        ));
        assert_eq!(
            (
                output.events_total,
                output.margin_removed,
                output.peacoqc_removed
            ),
            (5, 2, 0)
        );
        assert_eq!(output.warnings, ["2 of 5 events are margin events"]);
        assert_eq!(
            output
                .markers
                .iter()
                .map(|m| m.channel.as_str())
                .collect::<Vec<_>>(),
            ["B", "A"]
        );
        assert_eq!(
            output
                .markers
                .iter()
                .map(|m| m.marker.as_str())
                .collect::<Vec<_>>(),
            ["CD4", "CD3"]
        );
        assert_eq!(
            output.columns.iter().map(Vec::len).collect::<Vec<_>>(),
            [3, 3]
        );
        // W = A = 0 reduces logicle to asinh(x / (2*a)) / ln(10),
        // a = T / (10^M - 10^-M), with T = PnR - 1 = 99.
        for (actual, expected) in output.columns[1].iter().zip([
            3.504_364_847_533_351_3,
            3.805_394_811_273_436_6,
            3.981_486_064_417_284_3,
        ]) {
            assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
        }
        assert_eq!(
            output.logicle,
            [
                LogicleRecord {
                    channel: "A".into(),
                    t: 99.0,
                    w: 0.0,
                    m: 4.5,
                    a: 0.0
                },
                LogicleRecord {
                    channel: "B".into(),
                    t: 99.0,
                    w: 0.0,
                    m: 4.5,
                    a: 0.0
                },
            ]
        );
        let record = output.matrix.unwrap();
        assert_eq!(record.source, "file");
        assert!(record.applied);
        assert_eq!(record.channels, ["A", "B"]);
        let stored = ready(prepare(
            Path::new("x"),
            &data,
            Some(&external),
            &PreprocessOptions {
                preprocessed: true,
                channels: Some(vec!["A".into(), "B".into()]),
                ..options
            },
        ));
        assert_eq!(
            stored.columns,
            [
                vec![0.0, 10.0, 20.0, 30.0, 100.0],
                vec![0.0, 20.0, 30.0, 40.0, 100.0]
            ]
        );
        assert!(!stored.matrix.unwrap().applied);
        assert_eq!(stored.logicle, []);
    }

    #[test]
    fn preparation_skips_unusable_inputs_with_specific_reasons() {
        let data = dataset(&columns(), &[]);
        let external = identity_matrix();
        let path = Path::new("x.fcs");
        assert!(
            matches!(prepare(path,&data,None,&options()).unwrap(), Preparation::Skipped(reason) if reason.contains("no `$SPILLOVER`"))
        );
        let invalid = dataset(&columns(), &[("$SPILLOVER", "bad")]);
        assert!(
            matches!(prepare(path,&invalid,None,&options()).unwrap(), Preparation::Skipped(reason) if reason.starts_with("spillover keyword is invalid:"))
        );
        let mut missing = identity_matrix();
        missing.matrix.channels[0] = "missing".into();
        assert_eq!(
            prepare(path, &data, Some(&missing), &options()).unwrap(),
            Preparation::Skipped("matrix channel `missing` is not in the file".into())
        );
        let only_one = PreprocessOptions {
            channels: Some(vec!["A".into()]),
            ..options()
        };
        assert!(
            matches!(prepare(path,&data,Some(&external),&only_one).unwrap(), Preparation::Skipped(reason) if reason.starts_with("1 marker channel(s)"))
        );
        let flat = dataset(&[vec![10.0; 5], vec![20.0; 5]], &[]);
        assert_eq!(
            prepare(path, &flat, Some(&external), &options()).unwrap(),
            Preparation::Skipped("every event is a margin event".into())
        );
        let qc = PreprocessOptions {
            peacoqc: true,
            ..options()
        };
        assert_eq!(
            ready(prepare(path, &data, Some(&external), &qc)).peacoqc_removed,
            3
        );
        let mut nonfinite = data.clone();
        nonfinite.data.columns[0] = cytiq_fcs::ColumnData::F64(vec![f64::NAN; 5]);
        let qc = PreprocessOptions {
            remove_margins: false,
            ..qc
        };
        assert!(
            matches!(prepare(path,&nonfinite,Some(&external),&qc).unwrap(), Preparation::Skipped(reason) if reason.starts_with("PeacoQC failed"))
        );
        let negative = dataset(&[vec![-1e-9, 10.0, 20.0], vec![1.0, 20.0, 30.0]], &[]);
        assert!(
            matches!(prepare(path,&negative,Some(&external),&PreprocessOptions { remove_margins:false,..options() }).unwrap(), Preparation::Skipped(reason) if reason.starts_with("logicle estimate for `A` failed:"))
        );
    }

    #[test]
    fn preparation_refuses_acquisition_declared_unmixed_by_instrument() {
        let mut data = dataset(&columns(), &[]);
        data.parameters[0]
            .keywords
            .push(("$P1TYPE".into(), "Unmixed Fluorescence".into()));
        assert!(matches!(
            prepare(Path::new("x"), &data, Some(&identity_matrix()), &options()),
            Err(SpillQcError::UnmixedInput { components, .. }) if components == ["A"]
        ));
    }

    #[test]
    fn preparation_uses_internal_matrix_and_rejects_recorded_processing() {
        let data = dataset(&columns(), &[("$SPILLOVER", "2,A,B,1,0,0,1")]);
        let output = ready(prepare(Path::new("x"), &data, None, &options()));
        assert_eq!(output.matrix.unwrap().source, "fcs");
        let data = dataset(
            &columns(),
            &[(
                "CYTIQ_TRANSFORM",
                r#"{"method":"arcsinh","parameters":{"cofactor":150},"files":[{"channels":["A"]}]}"#,
            )],
        );
        assert!(
            matches!(prepare(Path::new("x"),&data,Some(&identity_matrix()),&options()), Err(SpillQcError::TransformedInput { channel, .. }) if channel == "A")
        );
        let data = dataset(
            &columns(),
            &[(
                "CYTIQ_COMPENSATION",
                r#"{"source":"fcs","channels":["A","B"]}"#,
            )],
        );
        assert!(matches!(
            prepare(Path::new("x"), &data, Some(&identity_matrix()), &options()),
            Err(SpillQcError::AlreadyCompensated { .. })
        ));
    }
}
