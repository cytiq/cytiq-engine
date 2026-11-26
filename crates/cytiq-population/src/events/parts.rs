//! `cytiq.events.parts.v1`: one layer split into part tables.
//!
//! Each part is an ordinary `cytiq.events.v1` table of a contiguous run of
//! the layer's rows (`row_set=subset` unless it is the only part). The
//! manifest lists the parts in row order with their bytes, digests and
//! `source_event_id` ranges, and the layer's values digest over every row in
//! order, which equals the digest of the same layer exported as one table.

use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};

use cytiq_mask::parquet::ParquetReadLimits;

use super::ValuesHasher;
use super::read::{EventsSummary, verify_with_limits_into};
use crate::identity::ObjectKey;
use crate::issue::{IssueCode, Issues};
use crate::object::{self, ObjectRoot, OpenError};
use crate::serialization;
use crate::{PopulationError, Result};

/// Contract identifier of a parts manifest.
pub const PARTS_CONTRACT: &str = "cytiq.events.parts.v1";
/// File name of the parts manifest in a parts directory.
pub const PARTS_FILE: &str = "parts.json";
/// Most parts in one layer.
pub const PARTS_MAX: u64 = 10_000;

/// Whether a layer's rows are every raw event in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowSet {
    All,
    Subset,
}

/// One part table, in row order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartEntry {
    /// File name in the parts directory: [`part_name`] of its position.
    pub location: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
    /// Absent for the empty part of an empty layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_source_event_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_source_event_id: Option<u64>,
    /// The part table's own `cytiq.events.values_sha256`.
    pub values_sha256: String,
}

/// The parts of one layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartsManifest {
    pub contract: String,
    pub acquisition_id: String,
    pub layer_id: String,
    pub row_set: RowSet,
    /// The raw event count, with `row_set=all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_events: Option<u64>,
    pub rows: u64,
    /// Digest of every row of every part, in order.
    pub values_sha256: String,
    pub parts: Vec<PartEntry>,
}

/// File name of the part at `index`.
#[must_use]
pub fn part_name(index: u64) -> String {
    format!("part-{index:05}.parquet")
}

impl PartsManifest {
    /// Write the manifest as canonical JSON.
    ///
    /// # Errors
    /// Returns serialization and write errors.
    pub fn write(&self, path: &Path) -> Result<()> {
        let text = serialization::canonical(&serde_json::to_value(self)?);
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Read a manifest with the document size and duplicate-key checks.
    ///
    /// # Errors
    /// Returns read, parse and shape errors.
    pub fn read(path: &Path) -> Result<Self> {
        Self::read_from(File::open(path)?, path)
    }

    /// [`Self::read`] from an open manifest; `name` is its location.
    fn read_from(file: File, name: &Path) -> Result<Self> {
        let value = serialization::read_from(file, name)?;
        let manifest: Self = serde_json::from_value(value)?;
        if manifest.parts.len() as u64 > PARTS_MAX {
            return Err(PopulationError::Invalid(format!(
                "{} parts exceed the {PARTS_MAX} a layer holds",
                manifest.parts.len()
            )));
        }
        Ok(manifest)
    }
}

/// Verify the parts manifest at `path` and every part beside it; issues
/// point at `at`. Returns the layer's summary when nothing is wrong.
pub fn verify_parts(path: &Path, at: &str, issues: &mut Issues) -> Option<EventsSummary> {
    verify_parts_with_limits(path, ParquetReadLimits::default(), at, issues)
}

/// [`verify_parts`] with explicit read limits for each part.
pub fn verify_parts_with_limits(
    path: &Path,
    limits: ParquetReadLimits,
    at: &str,
    issues: &mut Issues,
) -> Option<EventsSummary> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let open = |part: &str| File::open(directory.join(part)).map_err(OpenError::from);
    verify_layer(PartsManifest::read(path), &open, limits, at, issues)
}

/// [`verify_parts`] for the open manifest at `key` of a dataset: its parts
/// are opened from the same root.
pub(crate) fn verify_parts_in(
    root: &ObjectRoot,
    key: &str,
    file: File,
    at: &str,
    issues: &mut Issues,
) -> Option<EventsSummary> {
    let directory = key.rsplit_once('/').map_or("", |(directory, _)| directory);
    let open = |part: &str| {
        let key = if directory.is_empty() {
            part.to_owned()
        } else {
            format!("{directory}/{part}")
        };
        // Part names come from `part_name`, so their keys are valid.
        root.object(&ObjectKey::parse(&key).map_err(|_| OpenError::Missing)?)
    };
    verify_layer(
        PartsManifest::read_from(file, Path::new(key)),
        &open,
        ParquetReadLimits::default(),
        at,
        issues,
    )
}

/// Opens a part by its name in the parts directory.
type OpenPart<'a> = dyn Fn(&str) -> std::result::Result<File, OpenError> + 'a;

fn verify_layer(
    manifest: Result<PartsManifest>,
    open: &OpenPart<'_>,
    limits: ParquetReadLimits,
    at: &str,
    issues: &mut Issues,
) -> Option<EventsSummary> {
    let errors = issues.errors();
    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(PopulationError::Io(error)) => {
            issues.error(
                IssueCode::MissingObject,
                at,
                format!("cannot read parts manifest: {error}"),
            );
            return None;
        }
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable parts manifest: {error}"),
            );
            return None;
        }
    };
    if manifest.contract != PARTS_CONTRACT {
        issues.error(
            IssueCode::UnsupportedSchema,
            format!("{at}/contract"),
            format!("parts contract `{}` is not supported", manifest.contract),
        );
        return None;
    }
    if manifest.parts.is_empty() {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            "the manifest lists no part",
        );
        return None;
    }
    let mut state = Layer::new(&manifest);
    for (index, part) in manifest.parts.iter().enumerate() {
        let at = format!("{at}/parts/{index}");
        let summary = verify_part(open, part, index, limits, &at, issues, &mut state.hasher)?;
        state.check(&manifest, part, summary, &at, issues);
    }
    let summary = state.finish(&manifest, at, issues);
    (issues.errors() == errors).then_some(summary?)
}

/// Check one part's location, bytes and table, feeding its rows to the
/// layer digest.
fn verify_part(
    open: &OpenPart<'_>,
    part: &PartEntry,
    index: usize,
    limits: ParquetReadLimits,
    at: &str,
    issues: &mut Issues,
    hasher: &mut ValuesHasher,
) -> Option<EventsSummary> {
    if part.location != part_name(index as u64) {
        issues.error(
            IssueCode::InvalidReference,
            format!("{at}/location"),
            format!(
                "part {index} is `{}`, not `{}`",
                part.location,
                part_name(index as u64)
            ),
        );
        return None;
    }
    let file = match open(&part.location) {
        Ok(file) => file,
        Err(error) => {
            error.report(&part.location, format!("{at}/location"), issues);
            return None;
        }
    };
    let expected = (part.bytes, part.sha256.as_str());
    let file = object::verified(file, &part.location, expected, at, at, issues)?;
    verify_with_limits_into(file, limits, at, issues, Some(hasher))
}

/// What the parts read so far add up to.
struct Layer {
    hasher: ValuesHasher,
    first: Option<EventsSummary>,
    rows: u64,
    min_source_event_id: Option<u64>,
    last: Option<u64>,
    parts: usize,
}

impl Layer {
    fn new(manifest: &PartsManifest) -> Self {
        Self {
            hasher: ValuesHasher::new(&manifest.layer_id),
            first: None,
            rows: 0,
            min_source_event_id: None,
            last: None,
            parts: manifest.parts.len(),
        }
    }

    /// One part agrees with its entry, the manifest and the parts before it.
    fn check(
        &mut self,
        manifest: &PartsManifest,
        part: &PartEntry,
        summary: EventsSummary,
        at: &str,
        issues: &mut Issues,
    ) {
        let corrupt = |issues: &mut Issues, message: &str| {
            issues.error(IssueCode::CorruptArtifactHash, at, message);
        };
        if summary.layer_id != manifest.layer_id {
            corrupt(issues, "the part belongs to another layer");
        }
        if summary.descriptor.acquisition_id != manifest.acquisition_id {
            issues.error(
                IssueCode::InvalidReference,
                at,
                "the part holds events of another acquisition",
            );
        }
        if summary.rows != part.rows
            || summary.values_sha256 != part.values_sha256
            || summary.min_source_event_id != part.first_source_event_id
            || summary.max_source_event_id != part.last_source_event_id
        {
            corrupt(
                issues,
                "the part's rows, ids or values digest differ from its entry",
            );
        }
        let whole = self.parts == 1 && manifest.row_set == RowSet::All;
        if summary.raw_events != manifest.raw_events.filter(|_| whole) {
            corrupt(
                issues,
                "a part's row set must be `subset` unless it is the whole layer",
            );
        }
        if part.rows == 0 && self.parts > 1 {
            corrupt(issues, "only an empty layer has an empty part");
        }
        if let Some(first) = &self.first
            && (first.descriptor != summary.descriptor
                || first.channels != summary.channels
                || first.all_columns != summary.all_columns
                || first.source_sha256 != summary.source_sha256)
        {
            corrupt(
                issues,
                "the part's descriptor, channels or source differ from the first part's",
            );
        }
        if let Some(start) = summary.min_source_event_id {
            let ordered = self.last.is_none_or(|last| start > last)
                && (manifest.row_set == RowSet::Subset || start == self.rows);
            if !ordered {
                issues.error(
                    IssueCode::DuplicateEventIdentity,
                    at,
                    "parts are out of order, overlap or leave a gap",
                );
            }
            self.min_source_event_id = self.min_source_event_id.or(Some(start));
        }
        if summary.max_source_event_id.is_some() {
            self.last = summary.max_source_event_id;
        }
        self.rows += summary.rows;
        if self.first.is_none() {
            self.first = Some(summary);
        }
    }

    /// Totals and the layer digest agree with the manifest; the layer's
    /// summary.
    fn finish(
        self,
        manifest: &PartsManifest,
        at: &str,
        issues: &mut Issues,
    ) -> Option<EventsSummary> {
        let all = manifest.row_set == RowSet::All;
        if self.rows != manifest.rows
            || (all && manifest.raw_events != Some(self.rows))
            || (!all && manifest.raw_events.is_some())
        {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!(
                    "parts hold {} rows; the manifest records {} with row set {:?} and raw \
                     events {:?}",
                    self.rows, manifest.rows, manifest.row_set, manifest.raw_events
                ),
            );
        }
        if self.hasher.finish() != manifest.values_sha256 {
            issues.error(
                IssueCode::CorruptArtifactHash,
                format!("{at}/values_sha256"),
                "the parts' values disagree with the layer's values digest",
            );
        }
        Some(EventsSummary {
            rows: manifest.rows,
            raw_events: manifest.raw_events,
            min_source_event_id: self.min_source_event_id,
            max_source_event_id: self.last,
            values_sha256: manifest.values_sha256.clone(),
            ..self.first?
        })
    }
}
