//! What is wrong with a file, and whether that can be repaired.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::num::NonZeroUsize;
use std::path::Path;

use cytiq_fcs::{Error as FcsError, FcsReadLimits, FcsReader};
use serde::Serialize;

use crate::codes::{Refusal, RefusalCode};
use crate::error::RepairError;
use crate::plan::{Change, DataWindow, Dropped, Finding, Plan, plan};
use crate::scan::{Stop, scan};

/// Contract of [`PlanDocument`].
pub const PLAN_CONTRACT: &str = "cytiq.repair.plan.v1";

/// What [`diagnose`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnosis {
    /// The strict reader accepts the file.
    Conforming,
    /// It does not, and accepts what the plan would write.
    Repairable { plan: Box<Plan>, reader: String },
    /// It does not, and the file is not repaired.
    Refused { refusal: Refusal, reader: String },
}

/// Diagnose a file. Nothing is written, and memory holds TEXT, not DATA.
///
/// A file the strict reader accepts is conforming whatever else it holds. A
/// repair is planned only when DATA can be read in one way, and only when the
/// strict reader accepts the repaired file, read as if it were written.
///
/// # Errors
/// Returns read errors of `reader`.
pub fn diagnose<R: Read + Seek>(reader: &mut R) -> io::Result<Diagnosis> {
    let Some(refused) = strict(&mut *reader)? else {
        return Ok(Diagnosis::Conforming);
    };
    let refuse = |refusal| {
        Ok(Diagnosis::Refused {
            refusal,
            reader: refused.clone(),
        })
    };
    let planned = scan(reader).and_then(|scan| plan(reader, &scan));
    let plan = match planned {
        Ok(plan) => plan,
        Err(Stop::Refused(refusal)) => return refuse(refusal),
        Err(Stop::Io(error)) => return Err(error),
    };
    if plan.findings.is_empty() {
        return refuse(Refusal::new(
            RefusalCode::StillRefused,
            format!("no repair is known for: {refused}"),
        ));
    }
    let repaired = plan.repaired(&mut *reader, &"0".repeat(64));
    match strict(repaired)? {
        None => Ok(Diagnosis::Repairable {
            plan: Box::new(plan),
            reader: refused,
        }),
        Some(still) => refuse(Refusal::new(
            RefusalCode::StillRefused,
            format!("after the known repairs: {still}"),
        )),
    }
}

/// Why the strict reader refuses a file; `None` when it accepts it. DATA is
/// checked against the layout, not decoded.
fn strict<R: Read + Seek>(reader: R) -> io::Result<Option<String>> {
    match FcsReader::new(reader).stream(NonZeroUsize::MIN, FcsReadLimits::default()) {
        Ok(_) => Ok(None),
        // A file that ends inside a segment is malformed, not unreadable.
        Err(FcsError::Io(error)) if error.kind() != io::ErrorKind::UnexpectedEof => Err(error),
        Err(error) => Ok(Some(error.to_string())),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Conforming,
    Repairable,
    Refused,
}

/// The diagnosis of one file, as `cytiq repair --plan` prints it. It names no
/// path and no keyword value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanDocument {
    pub contract: &'static str,
    /// SHA-256 of the file: its acquisition, repaired or not.
    pub acquisition_id: String,
    pub source_bytes: u64,
    pub status: PlanStatus,
    /// What the strict reader says about the file as it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reader: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<DataWindow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<Dropped>,
}

impl PlanDocument {
    /// Diagnose the file at `path`.
    ///
    /// # Errors
    /// Returns a missing file and read errors.
    pub fn of(path: &Path) -> Result<Self, RepairError> {
        if !path.is_file() {
            return Err(RepairError::MissingInput {
                path: path.to_path_buf(),
            });
        }
        let read = |source| RepairError::Read {
            path: path.to_path_buf(),
            source,
        };
        let (source_bytes, acquisition_id) = cytiq_provenance::hash_file(path).map_err(read)?;
        let mut reader = BufReader::new(File::open(path).map_err(read)?);
        let diagnosis = diagnose(&mut reader).map_err(read)?;
        let mut document = Self {
            contract: PLAN_CONTRACT,
            acquisition_id,
            source_bytes,
            status: PlanStatus::Conforming,
            reader: None,
            refusal: None,
            repairs: Vec::new(),
            data: None,
            changes: Vec::new(),
            dropped: Vec::new(),
        };
        match diagnosis {
            Diagnosis::Conforming => {}
            Diagnosis::Repairable { plan, reader } => {
                document.status = PlanStatus::Repairable;
                document.reader = Some(reader);
                document.repairs = plan.findings;
                document.data = Some(plan.data);
                document.changes = plan.changes;
                document.dropped = plan.dropped;
            }
            Diagnosis::Refused { refusal, reader } => {
                document.status = PlanStatus::Refused;
                document.reader = Some(reader);
                document.refusal = Some(refusal);
            }
        }
        Ok(document)
    }
}
