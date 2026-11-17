//! Classifier predictions (`cytiq.classify.predictions.v2` … `v5`): one row
//! per input event with `assessed`, a probability per class and `label`;
//! schema 5 adds the decision policy's columns.

use std::collections::BTreeMap;
use std::path::Path;

use arrow_array::{Array, BooleanArray, Float32Array, RecordBatch};

use super::Rows;
use crate::labels::LabelState;
use crate::native::{
    ClassifierColumns, ClassifierRow, check_acquisition, float32, open_table, uint,
};
use crate::{PopulationError, Result, require};

const VERSION_KEY: &str = "cytiq.classify.schema_version";
const BUNDLE_VERSION_KEY: &str = "cytiq.classify.bundle_schema_version";
const CLASSES_KEY: &str = "cytiq.classify.classes";
const VOCABULARY_KEY: &str = "cytiq.classify.vocabulary_sha256";
const POLICY_KEY: &str = "cytiq.classify.decision_policy_sha256";
const NORMALIZATION_KEY: &str = "cytiq.classify.normalization";
/// Schema 5 columns copied into the label table, in its order.
const DECISION_COLUMNS: [&str; 4] = ["confidence", "uncertainty", "ood_score", "is_ood"];

/// What the predictions footer says.
pub(crate) struct Predictions {
    pub version: u32,
    pub classes: Vec<String>,
    /// Footer facts that shape the labels, for the definition.
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// The decision policy's SHA-256 (schema 5).
    pub policy: Option<String>,
}

impl Predictions {
    /// The contract of the predictions file.
    pub(crate) fn contract(&self) -> String {
        format!("cytiq.classify.predictions.v{}", self.version)
    }
}

/// Read the footer of the predictions at `path`, which must belong to
/// `acquisition` and come from a bundle with a class vocabulary (v2+).
pub(crate) fn header(path: &Path, acquisition: &str) -> Result<Predictions> {
    let reader = open_table(path)?;
    let metadata = reader.schema().metadata();
    check_acquisition(reader.schema(), acquisition)?;
    let version = metadata
        .get(VERSION_KEY)
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|version| (2..=5).contains(version))
        .ok_or_else(|| {
            PopulationError::Invalid(
                "the native file is not classifier predictions of schema 2 to 5 (bundle v1 \
                 predictions carry no class vocabulary)"
                    .into(),
            )
        })?;
    let classes: Vec<String> = metadata
        .get(CLASSES_KEY)
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(|| PopulationError::Invalid(format!("`{CLASSES_KEY}` is missing")))?;
    require(
        !classes.is_empty()
            && classes.iter().all(|class| !class.is_empty())
            && classes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == classes.len(),
        "prediction classes must be distinct and named",
    )?;
    let policy = metadata.get(POLICY_KEY).cloned();
    require(
        (version == 5) == policy.is_some(),
        "only schema 5 predictions pin a decision policy",
    )?;
    let fields = reader.schema().fields();
    for class in &classes {
        require(
            fields.iter().any(|f| *f.name() == format!("p_{class}")),
            format!("the predictions have no probability column for class `{class}`"),
        )?;
    }
    let mut parameters = BTreeMap::from([
        ("classes".to_owned(), serde_json::json!(classes)),
        ("schema_version".to_owned(), serde_json::json!(version)),
    ]);
    if let Some(bundle) = metadata.get(BUNDLE_VERSION_KEY) {
        parameters.insert("bundle_schema_version".into(), serde_json::json!(bundle));
    }
    if let Some(vocabulary) = metadata.get(VOCABULARY_KEY) {
        parameters.insert("vocabulary_sha256".into(), serde_json::json!(vocabulary));
    }
    if let Some(normalization) = metadata.get(NORMALIZATION_KEY) {
        parameters.insert("normalization".into(), serde_json::json!(normalization));
    }
    Ok(Predictions {
        version,
        classes,
        parameters,
        policy,
    })
}

/// Stream the predictions at `path` into `rows`: label, abstention reason,
/// the schema 5 decision columns, then the probabilities when
/// `probabilities`.
pub(crate) fn stream(
    path: &Path,
    predictions: &Predictions,
    probabilities: bool,
    rows: &mut Rows<'_>,
) -> Result<()> {
    let policy = predictions.version == 5;
    let mut columns: Vec<String> = vec!["event_id".into(), "source_event_id".into()];
    columns.extend(
        ClassifierColumns::names(policy)
            .iter()
            .map(|name| (*name).to_owned()),
    );
    if policy {
        columns.extend(["confidence", "uncertainty", "ood_score"].map(String::from));
    }
    if probabilities {
        columns.extend(predictions.classes.iter().map(|class| format!("p_{class}")));
    }
    let names = columns.iter().map(String::as_str).collect::<Vec<_>>();
    let mut row = 0_u64;
    for batch in open_table(path)?.project(&names)? {
        let batch = batch?;
        let ids = uint(&batch, "event_id")?;
        let sources = uint(&batch, "source_event_id")?;
        let decisions = ClassifierColumns::of(&batch, policy)?;
        let copied = Copied::of(&batch, predictions, policy, probabilities)?;
        for i in 0..batch.num_rows() {
            require(
                !ids.is_null(i) && ids.value(i) == row && !sources.is_null(i),
                "null or invalid prediction event id",
            )?;
            row += 1;
            let id = sources.value(i);
            rows.at(id)?;
            let (state, label, reason) = match decisions.row(i)? {
                ClassifierRow::NotAssessed => (rows.excluded(id)?, None, None),
                ClassifierRow::Classified(label) => {
                    rows.assessed(id)?;
                    (LabelState::Classified, Some(label), None)
                }
                ClassifierRow::Abstained(reason) => {
                    rows.assessed(id)?;
                    (LabelState::Abstained, None, Some(reason.as_str()))
                }
                ClassifierRow::LegacyUnknown => {
                    require(
                        predictions.classes.len() == 2,
                        "`unknown` is a binary classifier's margin band; a multiclass \
                         prediction cannot hold it",
                    )?;
                    rows.assessed(id)?;
                    (LabelState::Abstained, None, Some("margin"))
                }
            };
            rows.push(state, |builders| {
                builders[0].text(label);
                builders[1].text(reason);
                copied.fill(&mut builders[2..], i);
            })?;
        }
    }
    Ok(())
}

/// Native columns copied as they are: the schema 5 decision columns (null
/// before schema 5) and the probabilities.
struct Copied<'a> {
    decision: Option<[&'a Float32Array; 3]>,
    ood: Option<&'a BooleanArray>,
    probabilities: Vec<&'a Float32Array>,
}

impl<'a> Copied<'a> {
    fn of(
        batch: &'a RecordBatch,
        predictions: &Predictions,
        policy: bool,
        probabilities: bool,
    ) -> Result<Self> {
        let decision = if policy {
            Some([
                float32(batch, DECISION_COLUMNS[0])?,
                float32(batch, DECISION_COLUMNS[1])?,
                float32(batch, DECISION_COLUMNS[2])?,
            ])
        } else {
            None
        };
        let ood = if policy {
            Some(crate::native::boolean(batch, DECISION_COLUMNS[3])?)
        } else {
            None
        };
        let probabilities = if probabilities {
            predictions
                .classes
                .iter()
                .map(|class| float32(batch, &format!("p_{class}")))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            decision,
            ood,
            probabilities,
        })
    }

    /// Fill `confidence`, `uncertainty`, `ood_score`, `is_ood`, then the
    /// probabilities, for row `i`.
    fn fill(&self, builders: &mut [super::Builder], i: usize) {
        let value = |array: &Float32Array| array.is_valid(i).then(|| array.value(i));
        for (index, builder) in builders[..3].iter_mut().enumerate() {
            builder.float(self.decision.and_then(|columns| value(columns[index])));
        }
        builders[3].boolean(
            self.ood
                .and_then(|flags| flags.is_valid(i).then(|| flags.value(i))),
        );
        for (builder, column) in builders[4..].iter_mut().zip(&self.probabilities) {
            builder.float(value(column));
        }
    }
}
