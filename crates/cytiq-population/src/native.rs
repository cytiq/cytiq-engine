//! What cytiq's native per-event tables mean, shared by the population
//! adapter (`annotations`), dataset object checks and the label export.
//!
//! The native tables are a mask v2 (technical QC or gate membership) and
//! classifier predictions. Every one of them is an external input: it is
//! read through the bounded scalar reader, never Arrow's packed decoder.

use std::{fs::File, path::Path};

use arrow_array::{Array, BooleanArray, Float32Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::Schema;
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use cytiq_provenance::RunRecord;

use crate::{PopulationError, Result, bitmap::Bitmap, require, serialization};

/// Footer key of the mask schema version.
pub(crate) const MASK_VERSION_KEY: &str = cytiq_mask::SCHEMA_VERSION_KEY;

/// Masks and annotation tables are external inputs: read them through the
/// bounded scalar reader, which rejects malformed or over-budget pages before
/// the decoder allocates from their declared sizes.
pub(crate) fn open_table(path: &Path) -> Result<ScalarParquetReader<File>> {
    open_table_with_limits(path, ParquetReadLimits::default())
}

pub(crate) fn open_table_with_limits(
    path: &Path,
    limits: ParquetReadLimits,
) -> Result<ScalarParquetReader<File>> {
    Ok(ScalarParquetReader::new(File::open(path)?, limits)?)
}

/// [`open_table`] on a table that is already open.
pub(crate) fn table(file: File) -> Result<ScalarParquetReader<File>> {
    Ok(ScalarParquetReader::new(
        file,
        ParquetReadLimits::default(),
    )?)
}

/// A producer's v3 (or readable v2) provenance record.
pub(crate) fn producer_record(path: &Path) -> Result<RunRecord> {
    producer_record_from(File::open(path)?, path)
}

/// [`producer_record`] from an open record; `name` is its location.
pub(crate) fn producer_record_from(file: File, name: &Path) -> Result<RunRecord> {
    let record: RunRecord = serde_json::from_value(serialization::read_from(file, name)?)?;
    require(
        (cytiq_provenance::MIN_READ_SCHEMA_VERSION..=cytiq_provenance::SCHEMA_VERSION)
            .contains(&record.schema_version),
        "unsupported producer provenance version",
    )?;
    Ok(record)
}

/// The table belongs to `acquisition` and numbers events from 0.
pub(crate) fn check_acquisition(schema: &Schema, acquisition: &str) -> Result<()> {
    let metadata = schema.metadata();
    require(
        metadata
            .get(cytiq_mask::ACQUISITION_KEY)
            .map(String::as_str)
            == Some(acquisition)
            && metadata
                .get(cytiq_mask::EVENT_ID_BASE_KEY)
                .map(String::as_str)
                == Some("0"),
        "annotation acquisition or event-base mismatch",
    )
}

/// The `keep` flag of every raw event of a mask v2 that covers the whole
/// acquisition, each raw event exactly once.
pub(crate) fn read_raw_mask(path: &Path, acquisition: &str, raw_events: u64) -> Result<Bitmap> {
    let reader = open_table(path)?;
    check_acquisition(reader.schema(), acquisition)?;
    require(
        reader
            .schema()
            .metadata()
            .get(MASK_VERSION_KEY)
            .map(String::as_str)
            == Some("2")
            && reader.metadata().file_metadata().num_rows()
                == i64::try_from(raw_events).unwrap_or(-1),
        "QC mask must cover every raw event with schema v2",
    )?;
    let size = usize::try_from(raw_events)
        .map_err(|_| PopulationError::Invalid("raw event count overflows platform".into()))?;
    let mut seen = Bitmap::new(size)?;
    let mut eligible = Bitmap::new(size)?;
    for batch in reader.project(&["source_event_id", "keep"])? {
        let batch = batch?;
        let sources = uint(&batch, "source_event_id")?;
        let keep = boolean(&batch, "keep")?;
        for i in 0..batch.num_rows() {
            require(
                !sources.is_null(i) && !keep.is_null(i),
                "null mask lineage or eligibility",
            )?;
            let source = usize::try_from(sources.value(i))
                .map_err(|_| PopulationError::Invalid("invalid event id".into()))?;
            require(
                source < size && !seen.get(source),
                "duplicate or out-of-range raw mask id",
            )?;
            seen.set(source, true);
            eligible.set(source, keep.value(i));
        }
    }
    require(seen.is_full(), "incomplete raw mask")?;
    Ok(eligible)
}

pub(crate) fn uint<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    column(batch, name, "UInt64")
}

pub(crate) fn boolean<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a BooleanArray> {
    column(batch, name, "Boolean")
}

pub(crate) fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    column(batch, name, "Utf8")
}

pub(crate) fn float32<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Float32Array> {
    column(batch, name, "Float32")
}

fn column<'a, T: 'static>(batch: &'a RecordBatch, name: &str, kind: &str) -> Result<&'a T> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .ok_or_else(|| PopulationError::Invalid(format!("expected {kind}: {name}")))
}

/// Why a decision policy abstained (`abstain_reason`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AbstainReason {
    LowConfidence,
    Ood,
    LowConfidenceAndOod,
}

impl AbstainReason {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "low_confidence" => Some(Self::LowConfidence),
            "ood" => Some(Self::Ood),
            "low_confidence_and_ood" => Some(Self::LowConfidenceAndOod),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LowConfidence => "low_confidence",
            Self::Ood => "ood",
            Self::LowConfidenceAndOod => "low_confidence_and_ood",
        }
    }

    fn is_ood(self) -> bool {
        self != Self::LowConfidence
    }
}

/// What one predictions row says about its event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClassifierRow<'a> {
    /// `assessed = false`: masked out, no prediction.
    NotAssessed,
    /// An accepted class assignment.
    Classified(&'a str),
    /// A decision policy (schema 5) declined to assign a class.
    Abstained(AbstainReason),
    /// The legacy binary margin band (`label = "unknown"`, no policy).
    LegacyUnknown,
}

/// The predictions columns that decide a row, decision policy columns only
/// for schema 5.
pub(crate) struct ClassifierColumns<'a> {
    assessed: &'a BooleanArray,
    labels: &'a StringArray,
    policy: Option<PolicyColumns<'a>>,
}

struct PolicyColumns<'a> {
    statuses: &'a StringArray,
    reasons: &'a StringArray,
    ood: &'a BooleanArray,
}

impl<'a> ClassifierColumns<'a> {
    /// Columns a reader must project to interpret rows.
    pub(crate) fn names(policy: bool) -> &'static [&'static str] {
        if policy {
            &[
                "assessed",
                "label",
                "decision_status",
                "abstain_reason",
                "is_ood",
            ]
        } else {
            &["assessed", "label"]
        }
    }

    pub(crate) fn of(batch: &'a RecordBatch, policy: bool) -> Result<Self> {
        Ok(Self {
            assessed: boolean(batch, "assessed")?,
            labels: strings(batch, "label")?,
            policy: if policy {
                Some(PolicyColumns {
                    statuses: strings(batch, "decision_status")?,
                    reasons: strings(batch, "abstain_reason")?,
                    ood: boolean(batch, "is_ood")?,
                })
            } else {
                None
            },
        })
    }

    pub(crate) fn assessed(&self, i: usize) -> Result<bool> {
        require(
            !self.assessed.is_null(i),
            "null or invalid annotation event id",
        )?;
        Ok(self.assessed.value(i))
    }

    /// `is_ood` of a schema 5 row: null when OOD was not assessed.
    pub(crate) fn ood(&self, i: usize) -> Option<bool> {
        let flags = self.policy.as_ref()?.ood;
        (!flags.is_null(i)).then(|| flags.value(i))
    }

    /// Interpret row `i`, refusing every combination the decision policy
    /// contract (`docs/classifier-decision-policy.md`) rules out.
    pub(crate) fn row(&self, i: usize) -> Result<ClassifierRow<'a>> {
        let kept = self.assessed(i)?;
        if let Some(policy) = &self.policy {
            require(
                !policy.statuses.is_null(i),
                "missing classifier decision status",
            )?;
            let status = policy.statuses.value(i);
            let is_ood = !policy.ood.is_null(i) && policy.ood.value(i);
            if !kept {
                require(policy.ood.is_null(i), "OOD assessment/null mismatch")?;
                require(
                    status == "not_assessed" && self.labels.is_null(i) && policy.reasons.is_null(i),
                    "excluded event has a classification decision",
                )?;
                return Ok(ClassifierRow::NotAssessed);
            }
            if status == "abstained" {
                require(
                    self.labels.is_null(i) && !policy.reasons.is_null(i),
                    "abstention requires a null label and a reason",
                )?;
                let reason = AbstainReason::parse(policy.reasons.value(i))
                    .filter(|reason| reason.is_ood() == is_ood)
                    .ok_or_else(|| {
                        PopulationError::Invalid("inconsistent abstention reason/OOD".into())
                    })?;
                return Ok(ClassifierRow::Abstained(reason));
            }
            require(
                status == "classified" && policy.reasons.is_null(i) && !is_ood,
                "invalid accepted classification",
            )?;
        }
        require(
            self.labels.is_null(i) != kept,
            "classification assessment/label mismatch",
        )?;
        if !kept {
            return Ok(ClassifierRow::NotAssessed);
        }
        let label = self.labels.value(i);
        if label == "unknown" {
            require(
                self.policy.is_none(),
                "policy abstention must not become an unknown population",
            )?;
            return Ok(ClassifierRow::LegacyUnknown);
        }
        Ok(ClassifierRow::Classified(label))
    }
}

/// A gate membership row: every gate is null exactly when the event is
/// outside the input mask, and a member of a gate is a member of its parent
/// (`parents[g]` is the index of gate `g`'s parent among `gates`).
pub(crate) fn check_gate_row(
    gates: &[&BooleanArray],
    parents: &[Option<usize>],
    i: usize,
    kept: bool,
) -> Result<()> {
    for (gate, parent) in gates.iter().zip(parents) {
        require(
            gate.is_null(i) != kept,
            "membership assessment/keep mismatch",
        )?;
        if kept
            && gate.value(i)
            && let Some(parent) = parent
        {
            require(
                !gates[*parent].is_null(i) && gates[*parent].value(i),
                "gate membership exceeds its denominator",
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::ArrayRef;

    use super::*;

    fn batch(columns: Vec<(&str, ArrayRef)>) -> RecordBatch {
        RecordBatch::try_from_iter(columns).unwrap()
    }

    fn policy_batch(
        assessed: &[bool],
        labels: &[Option<&str>],
        statuses: &[Option<&str>],
        reasons: &[Option<&str>],
        ood: &[Option<bool>],
    ) -> RecordBatch {
        batch(vec![
            ("assessed", Arc::new(BooleanArray::from(assessed.to_vec()))),
            ("label", Arc::new(StringArray::from(labels.to_vec()))),
            (
                "decision_status",
                Arc::new(StringArray::from(statuses.to_vec())),
            ),
            (
                "abstain_reason",
                Arc::new(StringArray::from(reasons.to_vec())),
            ),
            ("is_ood", Arc::new(BooleanArray::from(ood.to_vec()))),
        ])
    }

    #[test]
    fn classifier_rows_map_every_v5_status() {
        let batch = policy_batch(
            &[false, true, true, true, true],
            &[None, Some("blast"), None, None, None],
            &[
                Some("not_assessed"),
                Some("classified"),
                Some("abstained"),
                Some("abstained"),
                Some("abstained"),
            ],
            &[
                None,
                None,
                Some("low_confidence"),
                Some("ood"),
                Some("low_confidence_and_ood"),
            ],
            &[None, Some(false), Some(false), Some(true), Some(true)],
        );
        let columns = ClassifierColumns::of(&batch, true).unwrap();
        let rows = (0..5).map(|i| columns.row(i).unwrap()).collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                ClassifierRow::NotAssessed,
                ClassifierRow::Classified("blast"),
                ClassifierRow::Abstained(AbstainReason::LowConfidence),
                ClassifierRow::Abstained(AbstainReason::Ood),
                ClassifierRow::Abstained(AbstainReason::LowConfidenceAndOod),
            ]
        );
        assert_eq!(columns.ood(0), None);
        assert_eq!(columns.ood(3), Some(true));
    }

    #[test]
    fn inconsistent_v5_rows_are_refused() {
        let cases = [
            // Excluded with an OOD flag.
            (false, None, "not_assessed", None, Some(false)),
            // Abstained with a label.
            (true, Some("blast"), "abstained", Some("ood"), Some(true)),
            // OOD reason without the OOD flag.
            (true, None, "abstained", Some("ood"), Some(false)),
            // Classified but OOD.
            (true, Some("blast"), "classified", None, Some(true)),
            // A policy never writes `unknown`.
            (true, Some("unknown"), "classified", None, None),
            // An assessed event cannot be `not_assessed`.
            (true, None, "not_assessed", None, None),
        ];
        for (assessed, label, status, reason, ood) in cases {
            let batch = policy_batch(&[assessed], &[label], &[Some(status)], &[reason], &[ood]);
            let columns = ClassifierColumns::of(&batch, true).unwrap();
            assert!(columns.row(0).is_err(), "{status} {label:?} {reason:?}");
        }
    }

    #[test]
    fn legacy_unknown_is_only_a_legacy_row() {
        let batch = batch(vec![
            (
                "assessed",
                Arc::new(BooleanArray::from(vec![true, true, false])),
            ),
            (
                "label",
                Arc::new(StringArray::from(vec![
                    Some("unknown"),
                    Some("blast"),
                    None,
                ])),
            ),
        ]);
        let columns = ClassifierColumns::of(&batch, false).unwrap();
        assert_eq!(columns.row(0).unwrap(), ClassifierRow::LegacyUnknown);
        assert_eq!(columns.row(1).unwrap(), ClassifierRow::Classified("blast"));
        assert_eq!(columns.row(2).unwrap(), ClassifierRow::NotAssessed);
        assert_eq!(columns.ood(0), None);
    }

    #[test]
    fn gate_rows_are_null_outside_the_mask_and_inside_their_parent() {
        let parent = BooleanArray::from(vec![Some(true), Some(false), None]);
        let child = BooleanArray::from(vec![Some(true), Some(true), None]);
        let gates = [&parent, &child];
        let parents = [None, Some(0)];
        check_gate_row(&gates, &parents, 0, true).unwrap();
        assert!(check_gate_row(&gates, &parents, 1, true).is_err());
        check_gate_row(&gates, &parents, 2, false).unwrap();
        assert!(check_gate_row(&gates, &parents, 0, false).is_err());
    }
}
