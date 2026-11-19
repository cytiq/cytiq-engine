//! `cytiq.qc.truth.v1`: independent per-event technical-artifact truth for
//! one raw acquisition. The contract is documented in `docs/qc-truth.md`.
//!
//! One Parquet row per raw event, in raw order: `source_event_id` (exactly
//! `0..rows`) and one nullable Boolean column per labelled artifact class.
//! `true` marks an artifact of that class, `false` an event verified free of
//! it, and null an event the class did not label (or found ambiguous). The
//! footer ties the table to its acquisition and carries the descriptor: who
//! labelled each class, how, and with which evidence.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, BooleanArray, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use cytiq_mask::{ACQUISITION_KEY, SOURCE_EVENT_ID_COLUMN};
use cytiq_publish::{Existing, Publication, PublishError};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::file::reader::ChunkReader;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::retention::EventTruth;

/// The contract name reports cite.
pub const TRUTH_CONTRACT: &str = "cytiq.qc.truth.v1";
pub const TRUTH_SCHEMA_VERSION: u32 = 1;
pub const TRUTH_SCHEMA_VERSION_KEY: &str = "cytiq.qc_truth.schema_version";
pub const TRUTH_DESCRIPTOR_KEY: &str = "cytiq.qc_truth.descriptor";
/// The descriptor is metadata, not data: a larger one is refused unread.
pub const MAX_DESCRIPTOR_BYTES: usize = 64 << 10;
pub const MAX_CHANNELS: usize = 64;

#[derive(Debug, Error)]
pub enum TruthError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("invalid artifact truth: {0}")]
    Invalid(String),
    #[error("{0}")]
    Publish(#[from] PublishError),
}

fn invalid(message: impl Into<String>) -> TruthError {
    TruthError::Invalid(message.into())
}

/// A technical-artifact class with a fixed column name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactClass {
    Debris,
    Doublet,
    Dead,
    TimeAnomaly,
    Saturated,
}

impl ArtifactClass {
    pub const ALL: [Self; 5] = [
        Self::Debris,
        Self::Doublet,
        Self::Dead,
        Self::TimeAnomaly,
        Self::Saturated,
    ];

    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Debris => "debris",
            Self::Doublet => "doublet",
            Self::Dead => "dead",
            Self::TimeAnomaly => "time_anomaly",
            Self::Saturated => "saturated",
        }
    }

    #[must_use]
    pub fn from_column(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.column() == name)
    }
}

/// Where a class's labels come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Known by construction: a synthetic generator wrote the event.
    Generative,
    /// Follows from a definition applied to the raw values, such as a value
    /// at the detector limit: it checks consistency, not biology.
    Definitional,
    /// A measurement independent of the channels and algorithms being
    /// evaluated: imaging, barcoded mixtures, a second viability readout.
    OrthogonalAssay,
    /// Several annotators, adjudicated.
    ExpertConsensus,
    /// One annotator.
    SingleExpert,
}

impl Evidence {
    const fn is_expert(self) -> bool {
        matches!(self, Self::ExpertConsensus | Self::SingleExpert)
    }
}

/// Which events a class labelled: point estimates are unbiased only when
/// the labelled events represent the acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    AllEvents,
    RandomSubset,
    Targeted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgreementStatistic {
    CohenKappa,
    FleissKappa,
    KrippendorffAlpha,
    /// The share of events on which annotators agreed, from 0 to 1.
    PercentAgreement,
}

/// Agreement between annotators on `events` events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agreement {
    pub statistic: AgreementStatistic,
    pub value: f64,
    pub events: u64,
}

impl Agreement {
    fn validate(&self) -> Result<(), String> {
        let lowest = match self.statistic {
            AgreementStatistic::PercentAgreement => 0.0,
            _ => -1.0,
        };
        if !(lowest..=1.0).contains(&self.value) {
            return Err(format!("agreement {} is outside [{lowest}, 1]", self.value));
        }
        if self.events == 0 {
            return Err("agreement was measured on no event".into());
        }
        Ok(())
    }
}

/// What a report may claim from a class's labels, derived from its
/// declared evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthClass {
    Synthetic,
    Definitional,
    Independent,
    ExpertReference,
    Exploratory,
}

/// How one class was labelled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassLabels {
    pub evidence: Evidence,
    pub selection: Selection,
    /// The labelling procedure, enough to repeat it.
    pub method: String,
    /// Measurement channels the labels were drawn on; empty for generated
    /// truth.
    pub channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotators: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreement: Option<Agreement>,
}

impl ClassLabels {
    /// Expert consensus counts as a reference only with measured agreement;
    /// a single expert is exploratory.
    #[must_use]
    pub const fn truth_class(&self) -> TruthClass {
        match (self.evidence, &self.agreement) {
            (Evidence::Generative, _) => TruthClass::Synthetic,
            (Evidence::Definitional, _) => TruthClass::Definitional,
            (Evidence::OrthogonalAssay, _) => TruthClass::Independent,
            (Evidence::ExpertConsensus, Some(_)) => TruthClass::ExpertReference,
            (Evidence::ExpertConsensus, None) | (Evidence::SingleExpert, _) => {
                TruthClass::Exploratory
            }
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.method.trim().is_empty() {
            return Err("`method` is empty".into());
        }
        if self.channels.len() > MAX_CHANNELS {
            return Err(format!("more than {MAX_CHANNELS} channels"));
        }
        if self
            .channels
            .iter()
            .any(|channel| channel.trim().is_empty())
        {
            return Err("a channel name is empty".into());
        }
        match (self.evidence, self.annotators) {
            (Evidence::SingleExpert, Some(1)) => {}
            (Evidence::ExpertConsensus, Some(n)) if n >= 2 => {}
            (Evidence::SingleExpert, _) => {
                return Err("`single_expert` needs `annotators: 1`".into());
            }
            (Evidence::ExpertConsensus, _) => {
                return Err("`expert_consensus` needs at least 2 `annotators`".into());
            }
            (_, Some(_)) => return Err("`annotators` applies to expert evidence only".into()),
            (_, None) => {}
        }
        if let Some(agreement) = &self.agreement {
            if !self.evidence.is_expert() {
                return Err("`agreement` applies to expert evidence only".into());
            }
            agreement.validate()?;
        }
        Ok(())
    }
}

/// Who labelled the acquisition and under which protocol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotation {
    pub producer: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// One optional entry per class: a fixed vocabulary, so a repeated or
/// unknown class is a parse error.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruthClasses {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debris: Option<ClassLabels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doublet: Option<ClassLabels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead: Option<ClassLabels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_anomaly: Option<ClassLabels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturated: Option<ClassLabels>,
}

impl TruthClasses {
    #[must_use]
    pub const fn get(&self, class: ArtifactClass) -> Option<&ClassLabels> {
        match class {
            ArtifactClass::Debris => self.debris.as_ref(),
            ArtifactClass::Doublet => self.doublet.as_ref(),
            ArtifactClass::Dead => self.dead.as_ref(),
            ArtifactClass::TimeAnomaly => self.time_anomaly.as_ref(),
            ArtifactClass::Saturated => self.saturated.as_ref(),
        }
    }

    /// The described classes, in class order.
    #[must_use]
    pub fn described(&self) -> Vec<ArtifactClass> {
        ArtifactClass::ALL
            .into_iter()
            .filter(|class| self.get(*class).is_some())
            .collect()
    }
}

/// The footer descriptor of a truth table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruthDescriptor {
    pub annotation: Annotation,
    pub classes: TruthClasses,
}

impl TruthDescriptor {
    fn validate(&self) -> Result<(), String> {
        let annotation = &self.annotation;
        if annotation.producer.trim().is_empty() || annotation.protocol.trim().is_empty() {
            return Err("`annotation` needs a `producer` and a `protocol`".into());
        }
        if annotation
            .reference
            .as_ref()
            .is_some_and(|reference| reference.trim().is_empty())
        {
            return Err("`annotation.reference` is empty".into());
        }
        for class in self.classes.described() {
            if let Some(labels) = self.classes.get(class) {
                labels
                    .validate()
                    .map_err(|reason| format!("class `{}`: {reason}", class.column()))?;
            }
        }
        Ok(())
    }
}

/// Artifact truth of one raw acquisition, one value per raw event and class.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactTruth {
    /// SHA-256 of the raw acquisition (`cytiq_fcs::acquisition_id`).
    pub acquisition: String,
    pub descriptor: TruthDescriptor,
    /// Labelled classes in class order; `None` means not labelled.
    pub classes: Vec<(ArtifactClass, Vec<Option<bool>>)>,
}

impl ArtifactTruth {
    /// Validated truth: classes are sorted into class order.
    ///
    /// # Errors
    /// [`TruthError::Invalid`] when the parts break the contract.
    pub fn new(
        acquisition: impl Into<String>,
        descriptor: TruthDescriptor,
        mut classes: Vec<(ArtifactClass, Vec<Option<bool>>)>,
    ) -> Result<Self, TruthError> {
        classes.sort_by_key(|(class, _)| *class);
        let truth = Self {
            acquisition: acquisition.into(),
            descriptor,
            classes,
        };
        truth.validate()?;
        Ok(truth)
    }

    /// # Errors
    /// [`TruthError::Invalid`] for a malformed acquisition, no class, classes
    /// out of order or repeated, columns of different lengths, a descriptor
    /// that does not describe exactly the labelled classes, or a descriptor
    /// rule.
    pub fn validate(&self) -> Result<(), TruthError> {
        if !cytiq_fcs::is_sha256(&self.acquisition) {
            return Err(invalid(format!(
                "`{ACQUISITION_KEY}` is not a SHA-256: `{}`",
                self.acquisition
            )));
        }
        if self.classes.is_empty() {
            return Err(invalid("no artifact class is labelled"));
        }
        if self.classes.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(invalid("artifact classes are repeated or out of order"));
        }
        let events = self.events();
        if let Some((class, _)) = self.classes.iter().find(|(_, v)| v.len() != events) {
            return Err(invalid(format!(
                "class `{}` does not have {events} events",
                class.column()
            )));
        }
        let labelled = self
            .classes
            .iter()
            .map(|(class, _)| *class)
            .collect::<Vec<_>>();
        if labelled != self.descriptor.classes.described() {
            return Err(invalid(
                "the descriptor must describe exactly the labelled class columns",
            ));
        }
        self.descriptor.validate().map_err(invalid)
    }

    /// Raw events the truth covers.
    #[must_use]
    pub fn events(&self) -> usize {
        self.classes.first().map_or(0, |(_, values)| values.len())
    }

    #[must_use]
    pub fn class(&self, class: ArtifactClass) -> Option<&[Option<bool>]> {
        self.classes
            .iter()
            .find(|(labelled, _)| *labelled == class)
            .map(|(_, values)| values.as_slice())
    }

    /// Per raw event: an artifact when any class says so, clean when every
    /// labelled class verified it free, undetermined otherwise.
    #[must_use]
    pub fn event_states(&self) -> Vec<EventTruth> {
        (0..self.events())
            .map(|row| {
                let mut unlabelled = false;
                for (_, values) in &self.classes {
                    match values[row] {
                        Some(true) => return EventTruth::Artifact,
                        Some(false) => {}
                        None => unlabelled = true,
                    }
                }
                if unlabelled {
                    EventTruth::Undetermined
                } else {
                    EventTruth::Clean
                }
            })
            .collect()
    }

    /// # Errors
    /// [`TruthError`] when the file cannot be opened or breaks the contract.
    pub fn read_parquet(path: &Path) -> Result<Self, TruthError> {
        Self::read_from(File::open(path)?, ParquetReadLimits::default())
    }

    /// Read untrusted truth through the bounded Parquet reader.
    ///
    /// # Errors
    /// [`TruthError::Invalid`] for malformed or over-budget Parquet, a footer
    /// or schema outside the contract, `source_event_id` other than
    /// `0..rows`, or an invalid descriptor.
    pub fn read_from<R: ChunkReader + 'static>(
        reader: R,
        limits: ParquetReadLimits,
    ) -> Result<Self, TruthError> {
        let parquet = |error: parquet::errors::ParquetError| invalid(error.to_string());
        let reader = ScalarParquetReader::new(reader, limits).map_err(parquet)?;
        let (acquisition, descriptor) =
            read_footer(reader.metadata().file_metadata().key_value_metadata())?;
        let labelled = schema_classes(reader.schema())?;
        let mut values = vec![Vec::new(); labelled.len()];
        let mut next_id = 0_u64;
        for batch in reader {
            let batch = batch.map_err(parquet)?;
            let ids = batch
                .column_by_name(SOURCE_EVENT_ID_COLUMN)
                .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
                .filter(|ids| ids.null_count() == 0)
                .ok_or_else(|| invalid("`source_event_id` must be a non-null UInt64 column"))?;
            for &id in ids.values() {
                if id != next_id {
                    return Err(invalid(format!(
                        "`source_event_id` must be 0..rows in order: found {id} at row {next_id}"
                    )));
                }
                next_id += 1;
            }
            for (class, values) in labelled.iter().zip(&mut values) {
                let column = batch
                    .column_by_name(class.column())
                    .and_then(|column| column.as_any().downcast_ref::<BooleanArray>())
                    .ok_or_else(|| invalid(format!("`{}` must be Boolean", class.column())))?;
                values.extend(column.iter());
            }
        }
        Self::new(
            acquisition,
            descriptor,
            labelled.into_iter().zip(values).collect(),
        )
    }

    /// Write the truth uncompressed, through a temporary sibling.
    ///
    /// # Errors
    /// [`TruthError`] when the truth is invalid or the file cannot be
    /// written.
    pub fn write_parquet(&self, path: &Path, existing: Existing) -> Result<(), TruthError> {
        self.validate()?;
        let encode = |error: &dyn std::fmt::Display| invalid(format!("Parquet: {error}"));
        let mut fields = vec![Field::new(SOURCE_EVENT_ID_COLUMN, DataType::UInt64, false)];
        let mut columns: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from_iter_values(
            0..self.events() as u64,
        ))];
        for (class, values) in &self.classes {
            fields.push(Field::new(class.column(), DataType::Boolean, true));
            columns.push(Arc::new(BooleanArray::from(values.clone())));
        }
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|e| encode(&e))?;
        let descriptor = serde_json::to_string(&self.descriptor).map_err(|e| encode(&e))?;
        let properties = WriterProperties::builder()
            .set_compression(Compression::UNCOMPRESSED)
            .set_key_value_metadata(Some(vec![
                KeyValue::new(
                    TRUTH_SCHEMA_VERSION_KEY.to_owned(),
                    TRUTH_SCHEMA_VERSION.to_string(),
                ),
                KeyValue::new(ACQUISITION_KEY.to_owned(), self.acquisition.clone()),
                KeyValue::new(TRUTH_DESCRIPTOR_KEY.to_owned(), descriptor),
            ]))
            .build();
        let mut publication = Publication::new(existing);
        publication.stage_file(path, |file| {
            let mut writer =
                ArrowWriter::try_new(file, schema, Some(properties)).map_err(|e| encode(&e))?;
            writer.write(&batch).map_err(|e| encode(&e))?;
            writer.close().map_err(|e| encode(&e))?;
            Ok::<(), TruthError>(())
        })?;
        Ok(publication.commit()?)
    }
}

/// The acquisition and descriptor from the footer. Every `cytiq.*` key must
/// be one of the three this contract defines, once and with a value, so a
/// mask or label table cannot pass as truth and two readers cannot pick
/// different duplicates.
fn read_footer(metadata: Option<&Vec<KeyValue>>) -> Result<(String, TruthDescriptor), TruthError> {
    let (mut version, mut acquisition, mut descriptor) = (None, None, None);
    for entry in metadata.into_iter().flatten() {
        if !entry.key.starts_with("cytiq.") {
            continue;
        }
        let value = entry
            .value
            .as_deref()
            .ok_or_else(|| invalid(format!("footer key `{}` has no value", entry.key)))?;
        let slot = match entry.key.as_str() {
            TRUTH_SCHEMA_VERSION_KEY => &mut version,
            ACQUISITION_KEY => &mut acquisition,
            TRUTH_DESCRIPTOR_KEY => &mut descriptor,
            other => {
                return Err(invalid(format!(
                    "unexpected footer key `{other}`: not a `{TRUTH_CONTRACT}` table"
                )));
            }
        };
        if slot.replace(value).is_some() {
            return Err(invalid(format!(
                "footer key `{}` appears more than once",
                entry.key
            )));
        }
    }
    match version {
        Some(version) if version == TRUTH_SCHEMA_VERSION.to_string() => {}
        Some(version) => {
            return Err(invalid(format!(
                "unsupported `{TRUTH_SCHEMA_VERSION_KEY}` {version}; this cytiq reads version {TRUTH_SCHEMA_VERSION}"
            )));
        }
        None => {
            return Err(invalid(format!(
                "no `{TRUTH_SCHEMA_VERSION_KEY}`: not a `{TRUTH_CONTRACT}` table"
            )));
        }
    }
    let acquisition =
        acquisition.ok_or_else(|| invalid(format!("no `{ACQUISITION_KEY}` in the footer")))?;
    let descriptor =
        descriptor.ok_or_else(|| invalid(format!("no `{TRUTH_DESCRIPTOR_KEY}` in the footer")))?;
    if descriptor.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid(format!(
            "the descriptor has {} bytes, more than {MAX_DESCRIPTOR_BYTES}",
            descriptor.len()
        )));
    }
    let descriptor = serde_json::from_str(descriptor)
        .map_err(|error| invalid(format!("descriptor: {error}")))?;
    Ok((acquisition.to_owned(), descriptor))
}

/// The class columns, in file order, after checking that the schema holds
/// `source_event_id` and class columns only.
fn schema_classes(schema: &Schema) -> Result<Vec<ArtifactClass>, TruthError> {
    let mut has_ids = false;
    let mut classes = Vec::new();
    for field in schema.fields() {
        let name = field.name().as_str();
        let repeated = || invalid(format!("column `{name}` appears more than once"));
        if name == SOURCE_EVENT_ID_COLUMN {
            if std::mem::replace(&mut has_ids, true) {
                return Err(repeated());
            }
            if field.data_type() != &DataType::UInt64 {
                return Err(invalid("`source_event_id` must be UInt64"));
            }
        } else if let Some(class) = ArtifactClass::from_column(name) {
            if classes.contains(&class) {
                return Err(repeated());
            }
            if field.data_type() != &DataType::Boolean {
                return Err(invalid(format!("`{name}` must be Boolean")));
            }
            classes.push(class);
        } else {
            return Err(invalid(format!(
                "unexpected column `{name}`: artifact truth holds `source_event_id` and the class columns debris, doublet, dead, time_anomaly and saturated only"
            )));
        }
    }
    if !has_ids {
        return Err(invalid("no `source_event_id` column"));
    }
    if classes.is_empty() {
        return Err(invalid("no artifact class column"));
    }
    Ok(classes)
}

#[cfg(test)]
#[path = "truth_tests.rs"]
mod tests;
