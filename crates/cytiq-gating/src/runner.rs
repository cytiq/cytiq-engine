use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cytiq_fcs::FcsReader;
use cytiq_mask::{EVENT_ID_COLUMN, EventMask, KEEP_COLUMN, SOURCE_EVENT_ID_COLUMN};
use cytiq_publish::{Existing, Publication, Staging};
use log::{debug, info};

use crate::error::GatingError;
use crate::evaluate::{GateCount, GatingResults};
use crate::parse::parse_gatingml;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Csv,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatingConfig {
    pub input: PathBuf,
    pub gating_ml: PathBuf,
    pub output: PathBuf,
    pub format: OutputFormat,
    /// Event masks (`masks.parquet`) restricting which events are gated;
    /// several are combined with AND.
    pub masks: Vec<PathBuf>,
    /// Optional Parquet file for per-event gate membership.
    pub membership: Option<PathBuf>,
    /// Replace existing outputs; otherwise an existing output is refused
    /// before any input is read.
    pub overwrite: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GatingRunResult {
    pub event_count: usize,
    pub gates: Vec<GateCount>,
    pub output: PathBuf,
}

#[derive(Debug, Default)]
pub struct GatingRunner;

impl GatingRunner {
    /// Gate one FCS file with a `GatingML` 2.0 strategy and write per-gate counts.
    ///
    /// # Errors
    ///
    /// Returns an error when the inputs cannot be read or parsed, gating
    /// fails, or the report cannot be written.
    pub fn run(config: &GatingConfig) -> Result<GatingRunResult, GatingError> {
        run_inner(config)
    }
}

fn run_inner(config: &GatingConfig) -> Result<GatingRunResult, GatingError> {
    // Counts and membership mask appear together or not at all. Existing
    // outputs are refused before any input is read.
    let mut publication = Publication::new(Existing::overwrite(config.overwrite));
    publication.check(&config.output)?;
    if let Some(membership) = &config.membership {
        publication.check(membership)?;
    }
    let xml =
        fs::read_to_string(&config.gating_ml).map_err(|source| GatingError::GatingMlRead {
            path: config.gating_ml.clone(),
            source,
        })?;
    let strategy = parse_gatingml(&xml)?;
    info!(
        "parsed {} gate(s) from {}",
        strategy.gates().len(),
        config.gating_ml.display()
    );

    let dataset = FcsReader::open(&config.input)
        .and_then(FcsReader::read)
        .map_err(|source| GatingError::FcsRead {
            path: config.input.clone(),
            source,
        })?;
    let read_error = |source| GatingError::FcsRead {
        path: config.input.clone(),
        source,
    };
    // Hashing a raw file is only worth it when an event mask is involved.
    let acquisition = if config.masks.is_empty() && config.membership.is_none() {
        None
    } else {
        Some(cytiq_fcs::acquisition_id(&config.input, &dataset.text).map_err(read_error)?)
    };
    let mask = cytiq_mask::read_combined(&config.masks)?
        .zip(acquisition.as_deref())
        .map(|(mask, acquisition)| mask.keep_for(&dataset, acquisition))
        .transpose()?;
    if let Some(mask) = &mask {
        info!(
            "gating {} of {} event(s) inside the event mask",
            mask.iter().filter(|kept| **kept).count(),
            mask.len()
        );
    }
    let results = strategy.gate_sample_masked(&dataset, mask.as_deref())?;
    let report = results.report();

    publication.stage_file(&config.output, |writer| match config.format {
        OutputFormat::Csv => write_csv(writer, &config.output, &report),
        OutputFormat::Json => write_json(writer, &config.output, &report),
    })?;
    if let Some((path, acquisition)) = config.membership.as_ref().zip(acquisition) {
        let source_event_ids = cytiq_fcs::source_event_ids(&dataset).map_err(read_error)?;
        stage_membership(
            &mut publication,
            path,
            &results,
            dataset.event_count(),
            acquisition,
            source_event_ids,
        )?;
    }
    publication.commit()?;

    Ok(GatingRunResult {
        event_count: results.event_count(),
        gates: report,
        output: config.output.clone(),
    })
}

/// One row per input event in the cytiq mask layout: a nullable Boolean
/// column per gate in evaluation order (null outside the event mask), and
/// `keep` marking events inside the event mask.
fn stage_membership(
    publication: &mut Publication,
    path: &Path,
    results: &GatingResults,
    rows: usize,
    acquisition: String,
    source_event_ids: Option<Vec<u64>>,
) -> Result<(), GatingError> {
    let in_mask = results
        .event_mask()
        .map_or_else(|| vec![true; rows], <[bool]>::to_vec);
    let mut mask = EventMask::new(acquisition, in_mask.clone(), source_event_ids);
    for id in results.gate_ids() {
        if [EVENT_ID_COLUMN, SOURCE_EVENT_ID_COLUMN, KEEP_COLUMN].contains(&id) {
            return Err(GatingError::ReservedGateId { id: id.to_owned() });
        }
        let membership = results
            .membership(id)
            .expect("gate ids come from the results");
        let column = membership
            .iter()
            .zip(&in_mask)
            .map(|(member, kept)| kept.then_some(*member))
            .collect();
        mask = mask.with_filter(id, column);
    }
    debug!("writing gate membership to {}", path.display());
    mask.stage_parquet(publication, path)?;
    Ok(())
}

fn write_csv(file: &mut Staging<'_>, path: &Path, report: &[GateCount]) -> Result<(), GatingError> {
    let csv_error = |source| GatingError::CsvWrite {
        path: path.to_path_buf(),
        source,
    };
    let mut writer = csv::Writer::from_writer(file);
    for row in report {
        writer.serialize(row).map_err(csv_error)?;
    }
    writer.flush().map_err(|source| GatingError::OutputWrite {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json(
    writer: &mut Staging<'_>,
    path: &Path,
    report: &[GateCount],
) -> Result<(), GatingError> {
    serde_json::to_writer_pretty(&mut *writer, report).map_err(|source| {
        GatingError::JsonWrite {
            path: path.to_path_buf(),
            source,
        }
    })?;
    writer
        .write_all(b"\n")
        .map_err(|source| GatingError::OutputWrite {
            path: path.to_path_buf(),
            source,
        })
}
