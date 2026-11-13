//! Everything a run reads, loaded once and shared by every plot.

use std::path::{Path, PathBuf};

use cytiq_fcs::{FcsDataset, FcsReader, SignalState};
use cytiq_gating::{GatingResults, GatingStrategy};
use log::debug;

use crate::classes::{self, Classes};
use crate::config::PlotConfig;
use crate::error::PlotError;

pub(crate) struct Sample {
    pub input: PathBuf,
    /// `$FIL`, else the input file stem.
    pub name: String,
    pub dataset: FcsDataset,
    pub signal: SignalState,
    /// Combined mask over the FCS rows; `None` without `--mask`.
    pub keep: Option<Vec<bool>>,
    pub strategy: Option<GatingStrategy>,
    /// Gating evaluated without the mask: masked membership is this AND
    /// `keep`, because the mask is applied to every gate before its parent.
    pub gating: Option<GatingResults>,
    pub classes: Option<Classes>,
}

impl Sample {
    pub(crate) fn load(config: &PlotConfig) -> Result<Self, PlotError> {
        let input = config.input.clone();
        let dataset = FcsReader::open(&input)
            .and_then(FcsReader::read)
            .map_err(|source| PlotError::FcsRead {
                path: input.clone(),
                source,
            })?;
        let invalid = |source| PlotError::Fcs {
            path: input.clone(),
            source,
        };
        let signal = SignalState::of(&dataset.text).map_err(invalid)?;
        let predictions = config.color_by.as_ref().is_some_and(|source| {
            source
                .path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("parquet"))
        });
        let acquisition = if config.masks.is_empty() && !predictions {
            None
        } else {
            Some(cytiq_fcs::acquisition_id(&input, &dataset.text).map_err(invalid)?)
        };
        let keep = match (cytiq_mask::read_combined(&config.masks)?, &acquisition) {
            (Some(mask), Some(acquisition)) => Some(mask.keep_for(&dataset, acquisition)?),
            _ => None,
        };
        let strategy = config.gating_ml.as_deref().map(read_strategy).transpose()?;
        let needs_gating = config
            .jobs
            .iter()
            .any(|job| !job.request.gates.is_empty() || job.request.population.is_some());
        let gating = match &strategy {
            Some(strategy) if needs_gating => Some(strategy.gate_sample_masked(&dataset, None)?),
            _ => None,
        };
        let classes = config
            .color_by
            .as_ref()
            .map(|source| classes::load(source, &dataset, &input, acquisition.as_deref()))
            .transpose()?;
        debug!(
            "loaded {} event(s) from {}",
            dataset.event_count(),
            input.display()
        );
        Ok(Self {
            name: sample_name(&dataset, &input),
            input,
            dataset,
            signal,
            keep,
            strategy,
            gating,
            classes,
        })
    }
}

fn read_strategy(path: &Path) -> Result<GatingStrategy, PlotError> {
    let xml = cytiq_gating::read_gatingml(path).map_err(|source| PlotError::GatingMlRead {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(cytiq_gating::parse_gatingml(&xml)?)
}

fn sample_name(dataset: &FcsDataset, input: &Path) -> String {
    dataset
        .text
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("$FIL"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                input
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default()
            },
            str::to_owned,
        )
}
