//! `cytiq repair`: write the repaired file and the report of what it changed.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use cytiq_fcs::{FcsReadLimits, FcsReader};
use cytiq_provenance::hash_file;
use cytiq_publish::{Existing, Prepared, Publication, PublishError};
use log::info;
use serde::Serialize;

use crate::codes::RepairCode;
use crate::diagnosis::{Diagnosis, diagnose};
use crate::error::RepairError;
use crate::plan::{Change, Dropped, Finding, Plan, range_digest};
use crate::{REPAIR_VERSION, Repaired};

/// Contract of [`RepairReport`].
pub const REPORT_CONTRACT: &str = "cytiq.repair.report.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairConfig {
    pub input: PathBuf,
    /// The repaired FCS file; its report goes beside it ([`report_path`]).
    pub output: PathBuf,
    /// Repairs the caller consents to. Nothing else is repaired.
    pub allow: BTreeSet<RepairCode>,
    pub overwrite: bool,
}

/// `sample.fcs` -> `sample.fcs.repair.json`.
#[must_use]
pub fn report_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".repair.json");
    PathBuf::from(name)
}

/// DATA, the same bytes in both files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepairedData {
    pub source_offset: u64,
    pub output_offset: u64,
    pub bytes: u64,
    pub sha256: String,
    pub events: u64,
    pub parameters: u64,
}

/// What a repair changed, as written beside the repaired file. It names no path
/// and no keyword value, so it can be published where a provenance record, which
/// holds paths, cannot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RepairReport {
    pub contract: &'static str,
    /// Version of the repairs.
    pub version: u32,
    /// SHA-256 of the file the instrument wrote.
    pub acquisition_id: String,
    pub source_bytes: u64,
    pub output_sha256: String,
    pub output_bytes: u64,
    pub fcs_version: String,
    /// What the strict reader said about the source.
    pub reader: String,
    pub repairs: Vec<Finding>,
    pub data: RepairedData,
    pub changes: Vec<Change>,
    pub dropped: Vec<Dropped>,
}

pub struct RepairRunner;

impl RepairRunner {
    /// [`Self::prepare`], then publish the file and its report, without a
    /// provenance record.
    ///
    /// # Errors
    /// As [`Self::prepare`], and what publishing returns.
    pub fn run(config: &RepairConfig) -> Result<RepairReport, RepairError> {
        Ok(Self::prepare(config)?.commit()?)
    }

    /// Stage the repaired file and its report, and verify the file as it was
    /// written: the strict reader accepts it, it needs no repair, its DATA is
    /// the source's byte for byte, and the source did not change. Nothing is
    /// published.
    ///
    /// # Errors
    /// Returns a missing or unreadable input, an output that is refused, a
    /// conforming or refused file, repairs that were not allowed, and a staged
    /// file that does not verify.
    pub fn prepare(config: &RepairConfig) -> Result<Prepared<RepairReport>, RepairError> {
        check_paths(config)?;
        let read = |source| RepairError::Read {
            path: config.input.clone(),
            source,
        };
        let (source_bytes, acquisition) = hash_file(&config.input).map_err(read)?;
        let mut source = BufReader::new(File::open(&config.input).map_err(read)?);
        let (plan, reader) = allowed(config, diagnose(&mut source).map_err(read)?)?;
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        publication.stage_file(&config.output, |writer| {
            let mut repaired = plan.repaired(&mut source, &acquisition);
            copy(&mut repaired, writer, &config.output)
        })?;
        let staged = staged(&publication, &config.output)?;
        let data = verify(&plan, &mut source, &staged, &acquisition)?;
        if hash_file(&config.input).map_err(read)? != (source_bytes, acquisition.clone()) {
            return Err(RepairError::InputChanged {
                path: config.input.clone(),
            });
        }
        let (output_bytes, output_sha256) =
            hash_file(&staged).map_err(|error| not_verified(error.to_string()))?;
        let report = RepairReport {
            contract: REPORT_CONTRACT,
            version: REPAIR_VERSION,
            acquisition_id: acquisition,
            source_bytes,
            output_sha256,
            output_bytes,
            fcs_version: plan.version,
            reader,
            repairs: plan.findings,
            data,
            changes: plan.changes,
            dropped: plan.dropped,
        };
        let path = report_path(&config.output);
        let mut bytes = serde_json::to_vec_pretty(&report)?;
        bytes.push(b'\n');
        publication.stage_file(&path, |writer| {
            writer
                .write_all(&bytes)
                .map_err(|source| RepairError::Publish(PublishError::io(&path, source)))
        })?;
        info!(
            "repaired acquisition {}: {} change(s), {} dropped range(s)",
            report.acquisition_id,
            report.changes.len(),
            report.dropped.len()
        );
        Ok(Prepared {
            summary: report,
            publication,
        })
    }
}

fn not_verified(reason: impl Into<String>) -> RepairError {
    RepairError::NotVerified {
        reason: reason.into(),
    }
}

fn check_paths(config: &RepairConfig) -> Result<(), RepairError> {
    if !config.input.is_file() {
        return Err(RepairError::MissingInput {
            path: config.input.clone(),
        });
    }
    let is_fcs = config
        .output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fcs"));
    let same = match (
        std::fs::canonicalize(&config.input),
        std::fs::canonicalize(&config.output),
    ) {
        (Ok(input), Ok(output)) => input == output,
        _ => false,
    };
    if !is_fcs || same {
        return Err(RepairError::InvalidOutput {
            path: config.output.clone(),
        });
    }
    for path in [config.output.clone(), report_path(&config.output)] {
        if path.exists() && !config.overwrite {
            return Err(RepairError::OutputExists { path });
        }
    }
    Ok(())
}

/// The plan, when the caller allowed every repair in it.
fn allowed(config: &RepairConfig, diagnosis: Diagnosis) -> Result<(Plan, String), RepairError> {
    let path = || config.input.clone();
    match diagnosis {
        Diagnosis::Conforming => Err(RepairError::NothingToRepair { path: path() }),
        Diagnosis::Refused { refusal, .. } => Err(RepairError::Refused {
            path: path(),
            refusal,
        }),
        Diagnosis::Repairable { plan, reader } => {
            let missing: Vec<RepairCode> =
                plan.codes().difference(&config.allow).copied().collect();
            if missing.is_empty() {
                Ok((*plan, reader))
            } else {
                Err(RepairError::NotAllowed {
                    path: path(),
                    missing,
                })
            }
        }
    }
}

fn copy<R: Read + Seek>(
    repaired: &mut Repaired<R>,
    writer: &mut impl io::Write,
    output: &Path,
) -> Result<(), RepairError> {
    let expected = repaired.length();
    let copied = io::copy(repaired, writer)
        .map_err(|source| RepairError::Publish(PublishError::io(output, source)))?;
    if copied == expected {
        Ok(())
    } else {
        Err(not_verified(format!(
            "{copied} bytes were written of {expected}"
        )))
    }
}

fn staged(publication: &Publication, output: &Path) -> Result<PathBuf, RepairError> {
    publication
        .staged()?
        .into_iter()
        .find_map(|(target, staged)| (target == output).then_some(staged))
        .ok_or_else(|| not_verified("nothing was staged"))
}

/// Read the staged file back and compare its DATA with the source's.
fn verify<R: Read + Seek>(
    plan: &Plan,
    source: &mut R,
    staged: &Path,
    acquisition: &str,
) -> Result<RepairedData, RepairError> {
    let failed = |error: &dyn std::fmt::Display| not_verified(error.to_string());
    let mut written = BufReader::new(File::open(staged).map_err(|error| failed(&error))?);
    if diagnose(&mut written).map_err(|error| failed(&error))? != Diagnosis::Conforming {
        return Err(not_verified("it still needs a repair"));
    }
    let metadata = FcsReader::open(staged)
        .and_then(|reader| reader.stream(NonZeroUsize::MIN, FcsReadLimits::default()))
        .map_err(|error| failed(&error))?
        .metadata;
    let text = &metadata.text;
    let data = plan.data;
    let same = metadata.event_count as u64 == data.events
        && metadata.parameters.len() as u64 == data.parameters
        && text.get(cytiq_fcs::ACQUISITION_KEYWORD) == Some(acquisition)
        && cytiq_fcs::is_repaired_acquisition(text, &metadata.parameters).unwrap_or(false);
    if !same {
        return Err(not_verified(
            "it states other events or another acquisition",
        ));
    }
    let offset = text
        .get("$BEGINDATA")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| not_verified("it states no DATA offset"))?;
    let before = digest(source, data.offset, data.bytes).map_err(|error| failed(&error))?;
    let after = digest(&mut written, offset, data.bytes).map_err(|error| failed(&error))?;
    if before != after {
        return Err(not_verified("its DATA differs from the source's"));
    }
    Ok(RepairedData {
        source_offset: data.offset,
        output_offset: offset,
        bytes: data.bytes,
        sha256: after,
        events: data.events,
        parameters: data.parameters,
    })
}

/// SHA-256 of `bytes` bytes at `offset`; an error when there are fewer.
fn digest<R: Read + Seek>(reader: &mut R, offset: u64, bytes: u64) -> io::Result<String> {
    Ok(range_digest(reader, offset..offset + bytes)?.0)
}
