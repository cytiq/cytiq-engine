//! Build a [`LayerDescriptor`] from what a cytiq stage recorded in an FCS
//! file, and what the exporter declares about processing done elsewhere.

use std::collections::HashSet;

use cytiq_fcs::{
    ColumnData, FcsMetadata, ORIGINAL_ID_PARAMETER, SOURCE_SPILLOVER_KEYWORD, SignalState,
    Spillover, TransformContract,
};
use serde_json::json;

use super::{
    ChannelSpec, LAYER_CONTRACT, LayerDescriptor, LayerState, MAX_CHANNELS, Origin, Step, StepKind,
    ValueType, channel_name_problem,
};
use crate::{PopulationError, Result, require, serialization};

/// What the stored values already were before any cytiq stage, for files
/// processed by other software (the run manifest's panel `signal_state`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseState {
    #[default]
    Raw,
    Compensated,
    Unmixed,
}

/// The digest of a step definition the file records only by path (a
/// compensation matrix) or not at all (an unmixing reference).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub sha256: String,
    /// Matched against the provenance record of the run that applied it.
    pub verified: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportOptions {
    pub base_state: BaseState,
    /// Required when `cytiq compensate` applied a matrix file.
    pub compensation_matrix: Option<Definition>,
    /// Required when `cytiq autospectral` unmixed the file.
    pub unmixing_reference: Option<Definition>,
}

fn invalid(message: impl Into<String>) -> PopulationError {
    PopulationError::Invalid(message.into())
}

fn value_type(column: &ColumnData) -> ValueType {
    match column {
        ColumnData::UInt(_) => ValueType::Uint64,
        ColumnData::F32(_) => ValueType::Float32,
        ColumnData::F64(_) => ValueType::Float64,
    }
}

/// The channel columns of a file: every parameter except `Original_ID`,
/// with the type of its decoded column.
fn channels(metadata: &FcsMetadata, empty_batch: &[ColumnData]) -> Result<Vec<ChannelSpec>> {
    require(
        empty_batch.len() == metadata.parameters.len(),
        "FCS batch columns disagree with its parameters",
    )?;
    let mut seen = HashSet::new();
    let mut specs = Vec::with_capacity(metadata.parameters.len());
    for (parameter, column) in metadata.parameters.iter().zip(empty_batch) {
        if parameter.name == ORIGINAL_ID_PARAMETER {
            continue;
        }
        if let Some(problem) = channel_name_problem(&parameter.name) {
            return Err(invalid(format!("{problem}: `{}`", parameter.name)));
        }
        require(
            seen.insert(parameter.name.to_ascii_lowercase()),
            format!(
                "channel `{}` repeats ignoring case; lakehouse columns are case-insensitive",
                parameter.name
            ),
        )?;
        specs.push(ChannelSpec {
            index: parameter.index,
            name: parameter.name.clone(),
            value_type: value_type(column),
        });
    }
    require(!specs.is_empty(), "the file has no channels")?;
    require(
        specs.len() <= MAX_CHANNELS,
        format!(
            "{} channels exceed the {MAX_CHANNELS} an events table holds",
            specs.len()
        ),
    )?;
    Ok(specs)
}

fn external(kind: StepKind) -> Step {
    Step {
        kind,
        method: "unknown".into(),
        origin: Origin::External,
        definition_sha256: None,
        parameters: None,
        channels: Vec::new(),
    }
}

fn declared(definition: &Definition) -> Origin {
    if definition.verified {
        Origin::Verified
    } else {
        Origin::Declared
    }
}

/// Digest of the spillover matrix a `source=fcs` compensation applied: the
/// file's own `$SPILLOVER`, which `cytiq compensate` moved aside.
fn recorded_spillover(metadata: &FcsMetadata) -> Result<String> {
    let raw = metadata.text.get(SOURCE_SPILLOVER_KEYWORD).ok_or_else(|| {
        invalid(format!(
            "compensated with the file's own spillover, but `{SOURCE_SPILLOVER_KEYWORD}` is missing"
        ))
    })?;
    let spillover = Spillover::parse_keyword(raw)?;
    Ok(serialization::digest(&json!({
        "channels": spillover.channels(),
        "matrix": spillover.matrix(),
    })))
}

impl LayerDescriptor {
    /// Describe the layer a file holds.
    ///
    /// # Errors
    /// Refuses invalid or reserved channel names, more than [`MAX_CHANNELS`]
    /// channels, unreadable signal keywords, a compensation matrix or
    /// unmixing reference whose definition is not supplied, processing
    /// applied twice (compensation on compensated or unmixed values, or both
    /// compensation and unmixing), normalization without transform, and steps
    /// over channels the file does not have.
    pub fn from_fcs(
        metadata: &FcsMetadata,
        empty_batch: &[ColumnData],
        acquisition_id: &str,
        options: &ExportOptions,
    ) -> Result<Self> {
        require(
            cytiq_fcs::is_sha256(acquisition_id),
            "acquisition id is not a lowercase SHA-256",
        )?;
        let channels = channels(metadata, empty_batch)?;
        let names: Vec<String> = channels.iter().map(|c| c.name.clone()).collect();
        let state = SignalState::of(&metadata.text)?;
        check_options(&state, options)?;
        let mut steps = Vec::new();
        match options.base_state {
            BaseState::Raw => {}
            BaseState::Compensated => steps.push(external(StepKind::Compensation)),
            BaseState::Unmixed => steps.push(external(StepKind::Unmixing)),
        }
        steps.extend(unmixing_step(&state, options)?);
        steps.extend(compensation_step(metadata, &state, options)?);
        steps.extend(transform_step(metadata, &state, &names)?);
        steps.extend(normalization_step(&state)?);
        check_step_channels(&steps, &names)?;
        let layer_state = steps
            .last()
            .map_or(LayerState::Raw, |step| step.kind.state());
        Ok(Self {
            contract: LAYER_CONTRACT.into(),
            acquisition_id: acquisition_id.to_owned(),
            state: layer_state,
            steps,
            channels,
        })
    }
}

/// Refuse combinations that would apply a step twice, and definitions for
/// steps the file did not go through.
fn check_options(state: &SignalState, options: &ExportOptions) -> Result<()> {
    require(
        !(state.compensation.is_some() && state.unmixing.is_some()),
        "the file is both compensated and unmixed",
    )?;
    require(
        options.base_state == BaseState::Raw
            || (state.compensation.is_none() && state.unmixing.is_none()),
        "compensated or unmixed by cytiq, but declared already processed",
    )?;
    require(
        options.compensation_matrix.is_none() || state.compensation.is_some(),
        "a compensation matrix was supplied, but the file is not compensated by cytiq",
    )?;
    require(
        options.unmixing_reference.is_none() || state.unmixing.is_some(),
        "an unmixing reference was supplied, but the file is not unmixed by cytiq",
    )
}

fn unmixing_step(state: &SignalState, options: &ExportOptions) -> Result<Option<Step>> {
    let Some(unmixing) = &state.unmixing else {
        return Ok(None);
    };
    let definition = options
        .unmixing_reference
        .as_ref()
        .ok_or_else(|| invalid("unmixed by `cytiq autospectral`: supply the unmixing reference"))?;
    Ok(Some(Step {
        kind: StepKind::Unmixing,
        method: "autospectral".into(),
        origin: declared(definition),
        definition_sha256: Some(definition.sha256.clone()),
        parameters: None,
        channels: unmixing.components.clone(),
    }))
}

fn compensation_step(
    metadata: &FcsMetadata,
    state: &SignalState,
    options: &ExportOptions,
) -> Result<Option<Step>> {
    let Some(compensation) = &state.compensation else {
        return Ok(None);
    };
    let (origin, definition) = if compensation.source == "fcs" {
        (Origin::Recorded, recorded_spillover(metadata)?)
    } else {
        // The keyword records the matrix by path only; never copy the path.
        let definition = options.compensation_matrix.as_ref().ok_or_else(|| {
            invalid("compensated with a matrix file: supply the compensation matrix")
        })?;
        (declared(definition), definition.sha256.clone())
    };
    Ok(Some(Step {
        kind: StepKind::Compensation,
        method: "spillover".into(),
        origin,
        definition_sha256: Some(definition),
        parameters: None,
        channels: compensation.channels.clone(),
    }))
}

fn transform_step(
    metadata: &FcsMetadata,
    state: &SignalState,
    names: &[String],
) -> Result<Option<Step>> {
    let Some(transform) = &state.transform else {
        return Ok(None);
    };
    let contract = TransformContract::of(&metadata.text, &transform.channels)?;
    // Older files do not list their channels: their transform covers all.
    let covered = if transform.channels.is_empty() {
        names.to_vec()
    } else {
        transform.channels.clone()
    };
    Ok(Some(Step {
        kind: StepKind::Transform,
        method: contract.method,
        origin: Origin::Recorded,
        definition_sha256: None,
        parameters: Some(contract.parameters),
        channels: covered,
    }))
}

fn normalization_step(state: &SignalState) -> Result<Option<Step>> {
    let Some(normalization) = &state.normalization else {
        return Ok(None);
    };
    require(
        state.transform.is_some(),
        "normalized values must be transformed first",
    )?;
    Ok(Some(Step {
        kind: StepKind::Normalization,
        method: "cytonorm".into(),
        origin: Origin::Recorded,
        definition_sha256: Some(normalization.model_sha256.clone()),
        parameters: Some(json!({
            "batch": normalization.batch,
            "panel_sha256": normalization.panel_sha256,
        })),
        channels: normalization.channels.clone(),
    }))
}

fn check_step_channels(steps: &[Step], names: &[String]) -> Result<()> {
    for step in steps {
        let mut seen = HashSet::new();
        for channel in &step.channels {
            require(
                names.contains(channel),
                format!(
                    "{} names channel `{channel}`, which the file does not have",
                    step.kind.as_str()
                ),
            )?;
            require(
                seen.insert(channel),
                format!("{} lists `{channel}` twice", step.kind.as_str()),
            )?;
        }
    }
    Ok(())
}
