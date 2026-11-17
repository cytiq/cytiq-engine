//! Verify a `cytiq.events.labels.v1` table through the bounded Parquet
//! reader, counting its labels exactly on the way.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;

use arrow_array::{Array, ArrayRef, BooleanArray, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use parquet::file::reader::ChunkReader;

use super::digest::LabelsHasher;
use super::{
    ACQUISITION_COLUMN, ACQUISITION_KEY, ANNOTATION_COLUMN, ANNOTATION_ID_KEY, DEFINITION_ID_KEY,
    DESCRIPTOR_CONTRACT, DESCRIPTOR_KEY, EVENT_ID_BASE_KEY, KIND_KEY, LabelDescriptor, LabelKind,
    LabelState, LabelType, MAX_COUNTS, RAW_EVENTS_KEY, SCHEMA_VERSION, SCHEMA_VERSION_KEY,
    SOURCE_EVENT_ID_COLUMN, STATE_COLUMN, VALUES_SHA256_KEY,
};
use crate::issue::{IssueCode, Issues};
use crate::serialization::{self, DocumentFormat};

/// Why a classifier abstained, as a label table stores it: the decision
/// policy reasons, and `margin` for the legacy binary band (`unknown`).
pub const ABSTAIN_REASONS: [&str; 4] =
    ["low_confidence", "ood", "low_confidence_and_ood", "margin"];

/// What a verified label table holds.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelsSummary {
    pub descriptor: LabelDescriptor,
    pub annotation_id: String,
    pub definition_id: Option<String>,
    pub rows: u64,
    pub values_sha256: String,
    /// Rows per state.
    pub states: BTreeMap<LabelState, u64>,
    /// Rows per (column, value, state) of every Boolean, `UInt64` and Utf8
    /// column, in column order; probabilities and scores are not counted.
    pub counts: Vec<CellCount>,
}

/// How many rows of one state hold one value in one column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellCount {
    pub column: String,
    /// The value as text (`true`, `false`, a class, a metacluster number);
    /// `None` for null.
    pub value: Option<String>,
    pub state: LabelState,
    pub count: u64,
}

/// Verify the label table at `path`; issues point at `at`.
pub fn verify_file(path: &Path, at: &str, issues: &mut Issues) -> Option<LabelsSummary> {
    match File::open(path) {
        Ok(file) => verify(file, at, issues),
        Err(error) => {
            issues.error(
                IssueCode::MissingObject,
                at,
                format!("cannot open label table: {error}"),
            );
            None
        }
    }
}

/// Verify a label table: version, descriptor and identities, columns, row
/// identities, the rules of its kind and the values digest.
pub fn verify<R: ChunkReader + 'static>(
    reader: R,
    at: &str,
    issues: &mut Issues,
) -> Option<LabelsSummary> {
    verify_with_limits(reader, ParquetReadLimits::default(), at, issues)
}

/// [`verify`] within explicit read limits.
pub fn verify_with_limits<R: ChunkReader + 'static>(
    reader: R,
    limits: ParquetReadLimits,
    at: &str,
    issues: &mut Issues,
) -> Option<LabelsSummary> {
    let errors = issues.errors();
    let reader = match ScalarParquetReader::new(reader, limits) {
        Ok(reader) => reader,
        Err(error) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                format!("unreadable label table: {error}"),
            );
            return None;
        }
    };
    let schema = reader.schema().clone();
    let header = header(&schema, at, issues)?;
    check_fields(&schema, &header, at, issues);
    let rows = u64::try_from(reader.metadata().file_metadata().num_rows()).unwrap_or(0);
    if rows != header.descriptor.raw_events {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            format!(
                "{rows} rows, but the acquisition has {} events",
                header.descriptor.raw_events
            ),
        );
    }
    if issues.errors() > errors {
        return None;
    }
    let scan = Scan::new(&header).run(reader, at, issues)?;
    if header.values_sha256.as_deref() != Some(scan.values_sha256.as_str()) {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            "decoded rows disagree with `cytiq.labels.values_sha256`",
        );
    }
    (issues.errors() == errors).then_some(LabelsSummary {
        annotation_id: header.annotation_id,
        definition_id: header.definition_id,
        rows,
        values_sha256: scan.values_sha256,
        states: scan.states,
        counts: scan.counts,
        descriptor: header.descriptor,
    })
}

struct Header {
    descriptor: LabelDescriptor,
    annotation_id: String,
    definition_id: Option<String>,
    columns: Vec<(String, LabelType)>,
    values_sha256: Option<String>,
}

fn header(schema: &Schema, at: &str, issues: &mut Issues) -> Option<Header> {
    let metadata = schema.metadata();
    match metadata.get(SCHEMA_VERSION_KEY).map(String::as_str) {
        Some(version) if version == SCHEMA_VERSION.to_string() => {}
        Some(version) => {
            issues.error(
                IssueCode::UnsupportedSchema,
                at,
                format!("label schema version {version} is not supported"),
            );
            return None;
        }
        None => {
            issues.error(
                IssueCode::UnsupportedSchema,
                at,
                format!("not a cytiq label table: `{SCHEMA_VERSION_KEY}` is missing"),
            );
            return None;
        }
    }
    let mut corrupt = |message: String| {
        issues.error(IssueCode::CorruptArtifactHash, at, message);
        None
    };
    let Some(text) = metadata.get(DESCRIPTOR_KEY) else {
        return corrupt(format!("`{DESCRIPTOR_KEY}` is missing"));
    };
    let descriptor = match serialization::parse(text.as_bytes(), DocumentFormat::Json)
        .and_then(|value| Ok(serde_json::from_value::<LabelDescriptor>(value)?))
    {
        Ok(descriptor) => descriptor,
        Err(error) => return corrupt(format!("malformed label descriptor: {error}")),
    };
    if descriptor.contract != DESCRIPTOR_CONTRACT {
        issues.error(
            IssueCode::UnsupportedSchema,
            at,
            format!(
                "label descriptor `{}` is not supported",
                descriptor.contract
            ),
        );
        return None;
    }
    if let Err(error) = descriptor.validate() {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            format!("invalid label descriptor: {error}"),
        );
        return None;
    }
    let annotation_id = descriptor.annotation_id();
    let definition_id = descriptor.definition_id();
    let corrupt = |issues: &mut Issues, message: String| {
        issues.error(IssueCode::CorruptArtifactHash, at, message);
    };
    if metadata.get(KIND_KEY).map(String::as_str) != Some(descriptor.kind.as_str()) {
        corrupt(
            issues,
            format!("`{KIND_KEY}` disagrees with the descriptor"),
        );
    }
    if metadata.get(ANNOTATION_ID_KEY) != Some(&annotation_id) {
        corrupt(
            issues,
            format!("the descriptor does not match `{ANNOTATION_ID_KEY}`"),
        );
    }
    if metadata.get(DEFINITION_ID_KEY) != definition_id.as_ref() {
        corrupt(
            issues,
            format!("the descriptor does not match `{DEFINITION_ID_KEY}`"),
        );
    }
    if metadata.get(ACQUISITION_KEY) != Some(&descriptor.acquisition_id) {
        issues.error(
            IssueCode::InvalidReference,
            at,
            "`cytiq.acquisition_sha256` differs from the descriptor's acquisition",
        );
    }
    if metadata.get(EVENT_ID_BASE_KEY).map(String::as_str) != Some("0") {
        corrupt(issues, format!("`{EVENT_ID_BASE_KEY}` must be 0"));
    }
    if metadata.get(RAW_EVENTS_KEY) != Some(&descriptor.raw_events.to_string()) {
        corrupt(
            issues,
            format!("`{RAW_EVENTS_KEY}` disagrees with the descriptor"),
        );
    }
    Some(Header {
        columns: descriptor.columns(),
        values_sha256: metadata.get(VALUES_SHA256_KEY).cloned(),
        descriptor,
        annotation_id,
        definition_id,
    })
}

fn check_fields(schema: &Schema, header: &Header, at: &str, issues: &mut Issues) {
    let fields = schema.fields();
    if fields.len() != header.columns.len() + 4 {
        issues.error(
            IssueCode::InconsistentChannelOrder,
            at,
            format!(
                "{} columns, but the descriptor has {} label columns and 4 keys",
                fields.len(),
                header.columns.len()
            ),
        );
        return;
    }
    let keys = [
        (ACQUISITION_COLUMN, DataType::Utf8),
        (ANNOTATION_COLUMN, DataType::Utf8),
        (SOURCE_EVENT_ID_COLUMN, DataType::UInt64),
        (STATE_COLUMN, DataType::Utf8),
    ];
    for (field, (name, data_type)) in fields.iter().zip(&keys) {
        check_field(field, name, data_type, false, at, issues);
    }
    for (field, (name, kind)) in fields[4..].iter().zip(&header.columns) {
        check_field(field, name, &kind.arrow(), true, at, issues);
    }
}

fn check_field(
    field: &Field,
    name: &str,
    data_type: &DataType,
    nullable: bool,
    at: &str,
    issues: &mut Issues,
) {
    if field.name() != name {
        issues.error(
            IssueCode::InconsistentChannelOrder,
            at,
            format!("column `{}` where the table has `{name}`", field.name()),
        );
    } else if field.data_type() != data_type || field.is_nullable() != nullable {
        issues.error(
            IssueCode::CorruptArtifactHash,
            at,
            format!(
                "column `{name}` must be a {}{data_type}",
                if nullable { "nullable " } else { "non-null " }
            ),
        );
    }
}

/// Counts of one counted column, per state.
enum Counter {
    /// false, true, null.
    Boolean([[u64; 3]; LabelState::ALL.len()]),
    UInt64(Vec<HashMap<u64, u64>>, [u64; LabelState::ALL.len()]),
    Utf8(Vec<HashMap<String, u64>>, [u64; LabelState::ALL.len()]),
    Uncounted,
}

fn state_index(state: LabelState) -> usize {
    LabelState::ALL
        .iter()
        .position(|s| *s == state)
        .expect("every state is listed")
}

struct Scan<'h> {
    header: &'h Header,
    hasher: LabelsHasher,
    rows: u64,
    states: BTreeMap<LabelState, u64>,
    counters: Vec<Counter>,
    distinct: usize,
    /// Classifier: whether assessed rows carry `is_ood`, `confidence` and
    /// `uncertainty`, fixed by the first assessed row.
    assessed_shape: Option<[bool; 3]>,
    parents: Vec<Option<usize>>,
    classes: Vec<String>,
}

/// What a scan found.
struct Scanned {
    values_sha256: String,
    states: BTreeMap<LabelState, u64>,
    counts: Vec<CellCount>,
}

/// The first rule a row breaks.
type RowResult = std::result::Result<(), String>;

impl<'h> Scan<'h> {
    fn new(header: &'h Header) -> Self {
        let counters = header
            .columns
            .iter()
            .map(|(_, kind)| match kind {
                LabelType::Boolean => Counter::Boolean([[0; 3]; LabelState::ALL.len()]),
                LabelType::UInt64 => Counter::UInt64(
                    vec![HashMap::new(); LabelState::ALL.len()],
                    [0; LabelState::ALL.len()],
                ),
                LabelType::Utf8 => Counter::Utf8(
                    vec![HashMap::new(); LabelState::ALL.len()],
                    [0; LabelState::ALL.len()],
                ),
                LabelType::Float32 => Counter::Uncounted,
            })
            .collect();
        Self {
            hasher: LabelsHasher::new(&header.annotation_id),
            rows: 0,
            states: BTreeMap::new(),
            counters,
            distinct: 0,
            assessed_shape: None,
            parents: header.descriptor.parents(),
            classes: header.descriptor.classes(),
            header,
        }
    }

    fn run<R: ChunkReader + 'static>(
        mut self,
        reader: ScalarParquetReader<R>,
        at: &str,
        issues: &mut Issues,
    ) -> Option<Scanned> {
        let descriptor = &self.header.descriptor;
        for batch in reader {
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    issues.error(
                        IssueCode::CorruptArtifactHash,
                        at,
                        format!("unreadable label batch: {error}"),
                    );
                    return None;
                }
            };
            let Some((acquisitions, annotations, ids, states)) = keys(&batch) else {
                issues.error(
                    IssueCode::CorruptArtifactHash,
                    at,
                    "key columns are not decodable",
                );
                return None;
            };
            if (0..batch.num_rows()).any(|i| acquisitions.value(i) != descriptor.acquisition_id) {
                issues.error(
                    IssueCode::InvalidReference,
                    at,
                    "rows carry another acquisition than the descriptor's",
                );
                return None;
            }
            if (0..batch.num_rows()).any(|i| annotations.value(i) != self.header.annotation_id) {
                issues.error(
                    IssueCode::CorruptArtifactHash,
                    at,
                    "rows carry another annotation id than the table's",
                );
                return None;
            }
            if ids
                .values()
                .iter()
                .enumerate()
                .any(|(offset, id)| *id != self.rows + offset as u64)
            {
                issues.error(
                    IssueCode::DuplicateEventIdentity,
                    at,
                    "`source_event_id` is not every raw row in order",
                );
                return None;
            }
            let columns = &batch.columns()[4..];
            if self.hasher.update(ids, states, columns).is_none() {
                issues.error(
                    IssueCode::CorruptArtifactHash,
                    at,
                    "label columns are not decodable",
                );
                return None;
            }
            if let Err(problem) = self.rows_of(ids, states, columns) {
                issues.error(IssueCode::CorruptArtifactHash, at, problem);
                return None;
            }
            self.rows += batch.num_rows() as u64;
        }
        if self.rows != descriptor.raw_events {
            issues.error(
                IssueCode::CorruptArtifactHash,
                at,
                "decoded rows disagree with the footer",
            );
            return None;
        }
        Some(self.finish())
    }

    fn rows_of(
        &mut self,
        ids: &UInt64Array,
        states: &StringArray,
        columns: &[ArrayRef],
    ) -> RowResult {
        let typed = Columns::of(columns, &self.header.columns)?;
        for i in 0..ids.len() {
            let id = ids.value(i);
            let state = LabelState::parse(states.value(i))
                .filter(|state| self.header.descriptor.kind.states().contains(state))
                .ok_or_else(|| {
                    format!(
                        "event {id}: `{}` is not a {} state",
                        states.value(i),
                        self.header.descriptor.kind.as_str()
                    )
                })?;
            self.check_row(&typed, i, state)
                .map_err(|problem| format!("event {id}: {problem}"))?;
            *self.states.entry(state).or_default() += 1;
            self.count(&typed, i, state)?;
        }
        Ok(())
    }

    fn check_row(&mut self, columns: &Columns<'_>, i: usize, state: LabelState) -> RowResult {
        match self.header.descriptor.kind {
            LabelKind::Qc => Ok(()),
            LabelKind::Gate => self.check_gate(columns, i, state),
            LabelKind::Cluster => self.check_cluster(columns, i, state),
            LabelKind::Classifier => self.check_classifier(columns, i, state),
        }
    }

    fn check_gate(&self, columns: &Columns<'_>, i: usize, state: LabelState) -> RowResult {
        let assessed = state == LabelState::Assessed;
        for (gate, parent) in columns.0.iter().zip(&self.parents) {
            let gate = gate.boolean();
            if gate.is_null(i) == assessed {
                return Err(if assessed {
                    "an assessed event has an undecided gate".into()
                } else {
                    "an unassessed event has a gate decision".into()
                });
            }
            if assessed
                && gate.value(i)
                && let Some(parent) = parent
                && !columns.0[*parent].boolean().value(i)
            {
                return Err("a gate member is outside its parent gate".into());
            }
        }
        Ok(())
    }

    fn check_cluster(&self, columns: &Columns<'_>, i: usize, state: LabelState) -> RowResult {
        let (metacluster, node) = (columns.0[0].uint(), columns.0[1].uint());
        let classified = state == LabelState::Classified;
        if metacluster.is_null(i) == classified || node.is_null(i) == classified {
            return Err("a cluster is assigned exactly to classified events".into());
        }
        if !classified {
            return Ok(());
        }
        let (metacluster, node) = (metacluster.value(i), node.value(i));
        let parameters = self
            .header
            .descriptor
            .definition
            .as_ref()
            .map(|d| &d.parameters);
        let count = |name: &str| {
            parameters
                .and_then(|p| p.get(name))
                .and_then(serde_json::Value::as_u64)
        };
        if metacluster == 0 || node == 0 {
            return Err("nodes and metaclusters are numbered from 1".into());
        }
        if count("nodes").is_some_and(|nodes| node > nodes)
            || count("metaclusters").is_some_and(|metaclusters| metacluster > metaclusters)
        {
            return Err("a node or metacluster beyond the model's".into());
        }
        let mapped = parameters
            .and_then(|p| p.get("node_metaclusters"))
            .and_then(serde_json::Value::as_array)
            .map(|map| {
                usize::try_from(node - 1)
                    .ok()
                    .and_then(|index| map.get(index))
                    .and_then(serde_json::Value::as_u64)
            });
        match mapped {
            Some(Some(expected)) if expected == metacluster => Ok(()),
            Some(_) => Err("a node assigned to another metacluster than the model's".into()),
            None => Ok(()),
        }
    }

    fn check_classifier(
        &mut self,
        columns: &Columns<'_>,
        i: usize,
        state: LabelState,
    ) -> RowResult {
        let [label, reason, confidence, uncertainty, score, ood] =
            [0, 1, 2, 3, 4, 5].map(|index| columns.0[index].array());
        let probabilities = &columns.0[6..];
        let assessed = matches!(state, LabelState::Classified | LabelState::Abstained);
        if !assessed {
            let empty = columns.0.iter().all(|column| column.array().is_null(i));
            return if empty {
                Ok(())
            } else {
                Err("an unassessed event has a prediction".into())
            };
        }
        if probabilities.iter().any(|p| p.array().is_null(i)) {
            return Err("an assessed event lacks a class probability".into());
        }
        let shape = [
            !ood.is_null(i),
            !confidence.is_null(i),
            !uncertainty.is_null(i),
        ];
        if *self.assessed_shape.get_or_insert(shape) != shape {
            return Err(
                "OOD, confidence and uncertainty are given for some assessed events only".into(),
            );
        }
        if ood.is_null(i) != score.is_null(i) {
            return Err("an OOD score and its decision go together".into());
        }
        let is_ood = !ood.is_null(i) && columns.0[5].boolean().value(i);
        let labels = columns.0[0].text();
        let reasons = columns.0[1].text();
        if state == LabelState::Classified {
            if label.is_null(i) || !reason.is_null(i) || is_ood {
                return Err(
                    "a classified event has one class, no abstention reason and is not OOD".into(),
                );
            }
            if !self.classes.iter().any(|class| class == labels.value(i)) {
                return Err(format!(
                    "class `{}` is not a class of the model",
                    labels.value(i)
                ));
            }
            return Ok(());
        }
        if !label.is_null(i) || reason.is_null(i) {
            return Err("an abstention has no class and a reason".into());
        }
        let reason = reasons.value(i);
        let consistent = match reason {
            "low_confidence" => !is_ood,
            "ood" | "low_confidence_and_ood" => is_ood,
            "margin" => ood.is_null(i),
            _ => false,
        };
        if consistent {
            Ok(())
        } else {
            Err(format!(
                "abstention reason `{reason}` disagrees with `is_ood`"
            ))
        }
    }

    fn count(&mut self, columns: &Columns<'_>, i: usize, state: LabelState) -> RowResult {
        let s = state_index(state);
        for (counter, column) in self.counters.iter_mut().zip(&columns.0) {
            match counter {
                Counter::Boolean(counts) => {
                    let values = column.boolean();
                    let slot = if values.is_null(i) {
                        2
                    } else {
                        usize::from(values.value(i))
                    };
                    counts[s][slot] += 1;
                }
                Counter::UInt64(values, nulls) => {
                    let array = column.uint();
                    if array.is_null(i) {
                        nulls[s] += 1;
                    } else if let Some(count) = values[s].get_mut(&array.value(i)) {
                        *count += 1;
                    } else {
                        self.distinct += 1;
                        values[s].insert(array.value(i), 1);
                    }
                }
                Counter::Utf8(values, nulls) => {
                    let array = column.text();
                    if array.is_null(i) {
                        nulls[s] += 1;
                    } else if let Some(count) = values[s].get_mut(array.value(i)) {
                        *count += 1;
                    } else {
                        self.distinct += 1;
                        values[s].insert(array.value(i).to_owned(), 1);
                    }
                }
                Counter::Uncounted => {}
            }
        }
        if self.distinct > MAX_COUNTS {
            return Err(format!("more than {MAX_COUNTS} distinct label values"));
        }
        Ok(())
    }

    fn finish(self) -> Scanned {
        let mut counts = Vec::new();
        for ((name, _), counter) in self.header.columns.iter().zip(&self.counters) {
            let mut column: Vec<CellCount> = Vec::new();
            for (s, state) in LabelState::ALL.into_iter().enumerate() {
                let mut push = |value: Option<String>, count: u64| {
                    if count > 0 {
                        column.push(CellCount {
                            column: name.clone(),
                            value,
                            state,
                            count,
                        });
                    }
                };
                match counter {
                    Counter::Boolean(slots) => {
                        push(Some("false".into()), slots[s][0]);
                        push(Some("true".into()), slots[s][1]);
                        push(None, slots[s][2]);
                    }
                    Counter::UInt64(values, nulls) => {
                        let mut values: Vec<_> = values[s].iter().collect();
                        values.sort_unstable();
                        for (value, count) in values {
                            push(Some(value.to_string()), *count);
                        }
                        push(None, nulls[s]);
                    }
                    Counter::Utf8(values, nulls) => {
                        let mut values: Vec<_> = values[s].iter().collect();
                        values.sort_unstable();
                        for (value, count) in values {
                            push(Some(value.clone()), *count);
                        }
                        push(None, nulls[s]);
                    }
                    Counter::Uncounted => {}
                }
            }
            counts.extend(column);
        }
        Scanned {
            values_sha256: self.hasher.finish(),
            states: self.states,
            counts,
        }
    }
}

/// One batch's label columns, downcast once.
struct Columns<'a>(Vec<Column<'a>>);

enum Column<'a> {
    Boolean(&'a BooleanArray),
    UInt64(&'a UInt64Array),
    Utf8(&'a StringArray),
    Other(&'a ArrayRef),
}

impl<'a> Columns<'a> {
    /// Downcast each array to its column's type, refusing any other.
    fn of(
        arrays: &'a [ArrayRef],
        expected: &[(String, LabelType)],
    ) -> std::result::Result<Self, String> {
        if arrays.len() != expected.len() {
            return Err("label columns disagree with the descriptor".into());
        }
        arrays
            .iter()
            .zip(expected)
            .map(|(array, (name, kind))| {
                let any = array.as_any();
                let column = match kind {
                    LabelType::Boolean => any.downcast_ref().map(Column::Boolean),
                    LabelType::UInt64 => any.downcast_ref().map(Column::UInt64),
                    LabelType::Utf8 => any.downcast_ref().map(Column::Utf8),
                    LabelType::Float32 => {
                        (array.data_type() == &DataType::Float32).then_some(Column::Other(array))
                    }
                };
                column.ok_or_else(|| format!("column `{name}` is not decodable"))
            })
            .collect::<std::result::Result<_, _>>()
            .map(Self)
    }
}

impl Column<'_> {
    fn array(&self) -> &dyn Array {
        match self {
            Self::Boolean(array) => *array,
            Self::UInt64(array) => *array,
            Self::Utf8(array) => *array,
            Self::Other(array) => array.as_ref(),
        }
    }

    // `Columns::of` checked each column's type against the descriptor, and
    // the descriptor fixes which columns each kind's rules read.
    fn boolean(&self) -> &BooleanArray {
        match self {
            Self::Boolean(array) => array,
            _ => unreachable!("checked Boolean column"),
        }
    }

    fn uint(&self) -> &UInt64Array {
        match self {
            Self::UInt64(array) => array,
            _ => unreachable!("checked UInt64 column"),
        }
    }

    fn text(&self) -> &StringArray {
        match self {
            Self::Utf8(array) => array,
            _ => unreachable!("checked Utf8 column"),
        }
    }
}

fn keys(batch: &RecordBatch) -> Option<(&StringArray, &StringArray, &UInt64Array, &StringArray)> {
    let acquisitions = batch.column(0).as_any().downcast_ref::<StringArray>()?;
    let annotations = batch.column(1).as_any().downcast_ref::<StringArray>()?;
    let ids = batch.column(2).as_any().downcast_ref::<UInt64Array>()?;
    let states = batch.column(3).as_any().downcast_ref::<StringArray>()?;
    (acquisitions.null_count() == 0
        && annotations.null_count() == 0
        && ids.null_count() == 0
        && states.null_count() == 0)
        .then_some((acquisitions, annotations, ids, states))
}
