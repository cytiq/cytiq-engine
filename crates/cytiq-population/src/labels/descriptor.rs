//! Where a label table's labels come from, and its two identities.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    DEFINITION_CONTRACT, DESCRIPTOR_CONTRACT, KEY_COLUMNS, LabelKind, LabelType, MAX_COLUMNS,
    label_name_problem,
};
use crate::observation::is_sha256;
use crate::{Result, require, serialization};

/// Path-free description of a label table. Its canonical digest is the
/// table's `annotation_id`: two tables share it only when they hold the
/// labels of the same native file, produced from the same input with the
/// same definition and eligibility.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelDescriptor {
    /// [`DESCRIPTOR_CONTRACT`].
    pub contract: String,
    pub kind: LabelKind,
    /// SHA-256 of the raw FCS file (`CYTIQ_ACQUISITION`).
    pub acquisition_id: String,
    pub raw_events: u64,
    pub producer: Producer,
    /// What the labels were decided by; `None` when no definition artifact
    /// exists (a QC mask, a `FlowSOM` trained on this acquisition alone).
    pub definition: Option<Definition>,
    pub eligibility: Eligibility,
    /// The label columns after the kind's fixed columns, in order.
    pub labels: Vec<LabelColumn>,
}

/// The cytiq run that wrote the native file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    /// The run's `tool`, such as `cytiq gate`.
    pub tool: String,
    /// The contract of the native file, such as `cytiq.mask.v2`.
    pub native_contract: String,
    /// SHA-256 of the native file.
    pub native_sha256: String,
    /// SHA-256 of the FCS file the producer read (its `input`), when its
    /// signal layer is not known. FCS files a cytiq stage writes may record
    /// paths, so their bytes are no path-free identity.
    pub input_sha256: Option<String>,
    /// The signal layer of that input, when the events table given is the
    /// export of exactly that file. At most one of the two is set; neither
    /// only for a QC mask combined from other masks.
    pub input_layer_id: Option<String>,
}

/// The producer's definition: its definition inputs by provenance role,
/// and the parameters that change the labels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Role (`gating_ml`, `model`, `decision_policy`) to the sorted
    /// SHA-256 of each file the producer read in that role.
    pub inputs: BTreeMap<String, Vec<String>>,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// Which events the producer was allowed to label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eligibility {
    /// Sorted SHA-256 of every mask the producer read.
    pub masks: Vec<String>,
    /// The masks were supplied, matched and agree with the native file, so
    /// excluded events are `qc_rejected` rather than `not_assessed`.
    pub verified: bool,
}

/// One label column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelColumn {
    pub name: String,
    /// The parent gate of a gate, an earlier column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The class of a classifier probability column `p_<class>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

impl LabelDescriptor {
    /// The table's `annotation_id`.
    ///
    /// # Panics
    /// Never: the descriptor holds only JSON-representable values.
    #[must_use]
    pub fn annotation_id(&self) -> String {
        serialization::digest(&serde_json::to_value(self).expect("descriptor serializes"))
    }

    /// The `definition_id`: the digest of the definition and the label
    /// columns, without the acquisition, input, native file or eligibility.
    /// Labels of different acquisitions are comparable only when they share
    /// it.
    #[must_use]
    pub fn definition_id(&self) -> Option<String> {
        let definition = self.definition.as_ref()?;
        Some(serialization::digest(&json!({
            "contract": DEFINITION_CONTRACT,
            "kind": self.kind,
            "tool": self.producer.tool,
            "definition": definition,
            "labels": self.labels,
        })))
    }

    /// Every column after the keys, with its type, in table order.
    #[must_use]
    pub fn columns(&self) -> Vec<(String, LabelType)> {
        let fixed = self
            .kind
            .fixed_columns()
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), *kind));
        let labels = self.kind.label_type().into_iter().flat_map(|kind| {
            self.labels
                .iter()
                .map(move |label| (label.name.clone(), kind))
        });
        fixed.chain(labels).collect()
    }

    /// The parent column index (among [`Self::labels`]) of each label.
    #[must_use]
    pub fn parents(&self) -> Vec<Option<usize>> {
        self.labels
            .iter()
            .map(|label| {
                label
                    .parent
                    .as_ref()
                    .and_then(|parent| self.labels.iter().position(|l| &l.name == parent))
            })
            .collect()
    }

    /// Check what the contract requires of a descriptor.
    ///
    /// # Errors
    /// Names the first violation.
    pub fn validate(&self) -> Result<()> {
        require(
            self.contract == DESCRIPTOR_CONTRACT,
            format!(
                "label descriptor contract `{}` is not supported",
                self.contract
            ),
        )?;
        require(
            is_sha256(&self.acquisition_id)
                && is_sha256(&self.producer.native_sha256)
                && self.producer.input_sha256.as_deref().is_none_or(is_sha256)
                && self
                    .producer
                    .input_layer_id
                    .as_deref()
                    .is_none_or(is_sha256),
            "label descriptor digests must be lowercase SHA-256",
        )?;
        require(
            !self.producer.tool.is_empty() && !self.producer.native_contract.is_empty(),
            "the label producer needs a tool and a native contract",
        )?;
        require(
            self.producer.input_sha256.is_none() || self.producer.input_layer_id.is_none(),
            "a producer input is its layer or, when that is unknown, its digest",
        )?;
        require(
            self.kind == LabelKind::Qc
                || self.producer.input_sha256.is_some()
                || self.producer.input_layer_id.is_some(),
            "only QC labels may come from a producer without an FCS input",
        )?;
        require(
            sorted_digests(&self.eligibility.masks),
            "eligibility masks must be sorted, distinct SHA-256 digests",
        )?;
        require(
            !self.eligibility.verified || !self.eligibility.masks.is_empty(),
            "verified eligibility needs the masks it was verified against",
        )?;
        if let Some(definition) = &self.definition {
            require(
                !definition.inputs.is_empty()
                    && definition.inputs.iter().all(|(role, digests)| {
                        !role.is_empty() && !digests.is_empty() && sorted_digests(digests)
                    }),
                "definition inputs must name roles with sorted, distinct SHA-256 digests",
            )?;
        }
        self.validate_labels()
    }

    fn validate_labels(&self) -> Result<()> {
        let columns = self.columns();
        require(
            KEY_COLUMNS.len() + columns.len() <= MAX_COLUMNS,
            format!(
                "{} label columns exceed the {MAX_COLUMNS} columns a table holds",
                columns.len()
            ),
        )?;
        let mut names = BTreeSet::new();
        for (index, label) in self.labels.iter().enumerate() {
            if let Some(problem) = label_name_problem(self.kind, &label.name) {
                return Err(crate::PopulationError::Invalid(format!(
                    "{problem}: `{}`",
                    label.name
                )));
            }
            require(
                names.insert(label.name.to_ascii_lowercase()),
                format!("label `{}` repeats another ignoring case", label.name),
            )?;
            match self.kind {
                LabelKind::Gate => {
                    require(label.class.is_none(), "a gate has no class")?;
                    if let Some(parent) = &label.parent {
                        require(
                            self.labels[..index].iter().any(|l| &l.name == parent),
                            format!(
                                "gate `{}` names a parent that is not an earlier gate",
                                label.name
                            ),
                        )?;
                    }
                }
                LabelKind::Classifier => {
                    let class = label.class.as_deref().unwrap_or_default();
                    require(
                        label.parent.is_none()
                            && !class.is_empty()
                            && label.name == format!("p_{class}"),
                        format!("probability column `{}` must be `p_<class>`", label.name),
                    )?;
                }
                LabelKind::Qc => require(
                    label.parent.is_none() && label.class.is_none(),
                    "a QC filter has no parent or class",
                )?,
                LabelKind::Cluster => {}
            }
        }
        match self.kind {
            LabelKind::Cluster => require(
                self.labels.is_empty(),
                "cluster labels have no label columns",
            ),
            LabelKind::Gate => require(
                !self.labels.is_empty(),
                "gate labels need at least one gate",
            ),
            LabelKind::Classifier => self.validate_classes(),
            LabelKind::Qc => Ok(()),
        }
    }
}

impl LabelDescriptor {
    /// The classifier's classes, in vocabulary order: the definition's
    /// `classes` parameter.
    #[must_use]
    pub fn classes(&self) -> Vec<String> {
        self.definition
            .as_ref()
            .and_then(|definition| definition.parameters.get("classes"))
            .and_then(|classes| serde_json::from_value(classes.clone()).ok())
            .unwrap_or_default()
    }

    /// A classifier names its classes, and its probability columns are
    /// either absent or exactly one per class, in order.
    fn validate_classes(&self) -> Result<()> {
        let classes = self.classes();
        require(
            !classes.is_empty()
                && classes.iter().all(|class| !class.is_empty())
                && classes.iter().collect::<BTreeSet<_>>().len() == classes.len(),
            "classifier labels need the model's distinct classes as the `classes` parameter",
        )?;
        require(
            self.labels.is_empty()
                || (self.labels.len() == classes.len()
                    && self
                        .labels
                        .iter()
                        .zip(&classes)
                        .all(|(label, class)| label.class.as_ref() == Some(class))),
            "probability columns must be one per class, in class order",
        )
    }
}

fn sorted_digests(digests: &[String]) -> bool {
    digests.iter().all(|digest| is_sha256(digest)) && digests.windows(2).all(|w| w[0] < w[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn gate_descriptor() -> LabelDescriptor {
        LabelDescriptor {
            contract: DESCRIPTOR_CONTRACT.into(),
            kind: LabelKind::Gate,
            acquisition_id: "a".repeat(64),
            raw_events: 4,
            producer: Producer {
                tool: "cytiq gate".into(),
                native_contract: "cytiq.mask.v2".into(),
                native_sha256: "b".repeat(64),
                input_sha256: Some("c".repeat(64)),
                input_layer_id: None,
            },
            definition: Some(Definition {
                inputs: BTreeMap::from([("gating_ml".into(), vec!["d".repeat(64)])]),
                parameters: BTreeMap::new(),
            }),
            eligibility: Eligibility {
                masks: vec!["e".repeat(64)],
                verified: true,
            },
            labels: vec![
                LabelColumn {
                    name: "cells".into(),
                    parent: None,
                    class: None,
                },
                LabelColumn {
                    name: "CD3+".into(),
                    parent: Some("cells".into()),
                    class: None,
                },
            ],
        }
    }

    #[test]
    fn definition_id_ignores_acquisition_input_and_eligibility() {
        let one = gate_descriptor();
        let mut other = one.clone();
        other.acquisition_id = "f".repeat(64);
        other.producer.native_sha256 = "0".repeat(64);
        other.producer.input_sha256 = Some("1".repeat(64));
        other.eligibility.verified = false;
        assert_ne!(one.annotation_id(), other.annotation_id());
        assert_eq!(one.definition_id(), other.definition_id());
        other.labels.pop();
        assert_ne!(one.definition_id(), other.definition_id());
    }

    #[test]
    fn labels_without_a_definition_have_no_definition_id() {
        let mut descriptor = gate_descriptor();
        descriptor.definition = None;
        assert_eq!(descriptor.definition_id(), None);
    }

    #[test]
    fn invalid_labels_are_refused() {
        let mut case_collision = gate_descriptor();
        case_collision.labels[1].name = "CELLS".into();
        case_collision.labels[1].parent = None;
        let mut reserved = gate_descriptor();
        reserved.labels[0].name = "State".into();
        reserved.labels[1].parent = Some("State".into());
        let mut late_parent = gate_descriptor();
        late_parent.labels.swap(0, 1);
        let mut unverified = gate_descriptor();
        unverified.eligibility.masks.clear();
        for descriptor in [case_collision, reserved, late_parent, unverified] {
            assert!(descriptor.validate().is_err(), "{:?}", descriptor.labels);
        }
        gate_descriptor().validate().unwrap();
    }
}
