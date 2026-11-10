//! Versioned portable bundles; v1 retains its original binary contract.
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::serialization;
use crate::{ClassifyError, Profile};

#[derive(Clone, Debug, Default, serde::Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizationContract {
    #[default]
    None,
    Cytonorm {
        model_sha256: String,
    },
}

impl NormalizationContract {
    /// Check normalization independently of the feature recipe.
    ///
    /// # Errors
    /// Rejects missing, unexpected or mismatched normalization state.
    pub fn check(&self, dataset: &cytiq_fcs::FcsDataset) -> Result<(), ClassifyError> {
        let state = cytiq_fcs::normalization_state(&dataset.text)?;
        match (self, state) {
            (Self::None, None) => Ok(()),
            (Self::Cytonorm { model_sha256 }, Some(state))
                if *model_sha256 == state.model_sha256
                    && cytiq_fcs::TransformContract::of(&dataset.text, &state.channels).is_ok()
                    && state.channels.iter().all(|channel| {
                        dataset
                            .parameters
                            .iter()
                            .filter(|p| p.name == *channel)
                            .count()
                            == 1
                    }) =>
            {
                Ok(())
            }
            _ => Err(invalid("classifier normalization contract mismatch")),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Bundle {
    #[serde(default)]
    pub normalization: Option<NormalizationContract>,
    pub schema_version: u32,
    pub profile: Profile,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    pub classes: Vec<String>,
    pub input: String,
    pub output: String,
    pub model_sha256: String,
    pub run_id: String,
    #[serde(default)]
    pub vocabulary_sha256: Option<String>,
    #[serde(default)]
    pub panel_sha256: Option<String>,
    #[serde(default)]
    pub label_mapping_sha256: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub task_kind: Option<String>,
    #[serde(default)]
    pub decision_rule: Option<String>,
    #[serde(default)]
    pub unknown_policy: Option<String>,
}

const fn default_threshold() -> f32 {
    0.5
}

fn invalid(message: &str) -> ClassifyError {
    ClassifyError::Invalid(message.into())
}

impl Bundle {
    /// Load a bundle and verify its class and knowledge contract.
    ///
    /// # Errors
    /// Returns an error for unreadable, corrupt or incompatible portable files.
    pub fn load(directory: &Path, margin: f32) -> Result<Self, ClassifyError> {
        let (_, payload) = manifest(directory)?;
        let bundle: Self = serde_json::from_value(payload.clone())?;
        if bundle.input != "features" || bundle.output != "probabilities" {
            return Err(invalid("unsupported ONNX interface"));
        }
        match bundle.schema_version {
            1 if bundle.classes == ["non_blast", "blast"] => {}
            2..=4 => bundle.validate_v2(directory, &payload)?,
            _ => return Err(invalid("unsupported bundle version/classes")),
        }
        match (bundle.schema_version, &bundle.normalization) {
            (1 | 2, None) | (3 | 4, Some(NormalizationContract::None)) => {}
            (3 | 4, Some(NormalizationContract::Cytonorm { model_sha256 }))
                if cytiq_fcs::is_sha256(model_sha256) => {}
            _ => {
                return Err(invalid(
                    "missing or unsupported classifier normalization contract",
                ));
            }
        }
        if bundle.multiclass() {
            if margin != 0.0 {
                return Err(invalid(
                    "binary abstain margin cannot be used for multiclass",
                ));
            }
        } else if !bundle.threshold.is_finite()
            || !(0.0..=1.0).contains(&bundle.threshold)
            || margin > bundle.threshold.min(1.0 - bundle.threshold)
        {
            return Err(invalid("invalid binary threshold/abstain margin"));
        }
        Ok(bundle)
    }

    #[must_use]
    pub fn multiclass(&self) -> bool {
        self.task_kind.as_deref() == Some("multiclass")
    }

    fn validate_v2(&self, directory: &Path, payload: &Value) -> Result<(), ClassifyError> {
        let vocabulary_payload =
            self.knowledge(directory, "classes", self.vocabulary_sha256.as_deref())?;
        let panel = self.knowledge(directory, "panel", self.panel_sha256.as_deref())?;
        let mapping = self.knowledge(
            directory,
            "label-mapping",
            self.label_mapping_sha256.as_deref(),
        )?;
        let vocabulary: Vocabulary = serde_json::from_value(vocabulary_payload)?;
        vocabulary.validate()?;
        let ids: Vec<_> = vocabulary.classes.iter().map(|c| c.id.clone()).collect();
        if self.classes != ids
            || self.task.as_deref() != Some(&vocabulary.task)
            || self.task_kind.as_deref() != Some(&vocabulary.task_kind)
            || self.decision_rule.as_deref() != Some(&vocabulary.decision_rule)
            || self.unknown_policy.as_deref() != Some(&vocabulary.unknown_policy)
            || self.panel_sha256.as_deref() != Some(&vocabulary.panel_sha256)
            || panel["panel"].as_str() != Some(&vocabulary.panel_id)
            || payload["profile"] != panel
            || mapping["vocabulary_sha256"].as_str() != self.vocabulary_sha256.as_deref()
            || mapping["task"].as_str() != Some(&vocabulary.task)
        {
            return Err(invalid("bundle knowledge contract mismatch"));
        }
        validate_mapping(&mapping, &self.classes)?;
        Ok(())
    }

    fn knowledge(
        &self,
        directory: &Path,
        stem: &str,
        expected: Option<&str>,
    ) -> Result<Value, ClassifyError> {
        if self.schema_version != 4 {
            let path = directory.join(format!("{stem}.json"));
            verify_hash(&path, expected)?;
            return Ok(serde_json::from_value(serialization::read(&path)?)?);
        }
        let payload = serialization::read(&serialization::resolve(directory, stem)?)?;
        if Some(serialization::digest(&payload).as_str()) != expected {
            return Err(invalid("knowledge checksum mismatch"));
        }
        Ok(payload)
    }
}

pub(crate) fn manifest(directory: &Path) -> Result<(PathBuf, Value), ClassifyError> {
    let json = directory.join("manifest.json");
    let path = if json.exists() {
        json
    } else {
        serialization::resolve(directory, "manifest")?
    };
    let payload = serialization::read(&path)?;
    if payload["schema_version"] == 4 {
        serialization::resolve(directory, "manifest")?;
    } else if path.extension().is_none_or(|extension| extension != "json") {
        return Err(invalid("YAML bundles require schema_version 4"));
    }
    Ok((path, payload))
}

fn validate_mapping(mapping: &Value, classes: &[String]) -> Result<(), ClassifyError> {
    if mapping["schema_version"] != 2 || mapping["artifact_type"] != "label_mapping" {
        return Err(invalid("unsupported label mapping"));
    }
    let labels = mapping["labels"]
        .as_object()
        .ok_or_else(|| invalid("invalid label mappings"))?;
    let ignored = mapping["ignored_labels"]
        .as_array()
        .ok_or_else(|| invalid("invalid ignored labels"))?;
    for (label, target) in labels {
        if !target
            .as_str()
            .is_some_and(|id| classes.iter().any(|known| known == id))
            || ignored.iter().any(|id| id.as_str() == Some(label))
        {
            return Err(invalid("unknown or conflicting label mapping"));
        }
    }
    if ignored.iter().any(|label| label.as_str().is_none()) {
        return Err(invalid("invalid ignored label"));
    }
    Ok(())
}

fn verify_hash(path: &Path, expected: Option<&str>) -> Result<(), ClassifyError> {
    let expected = expected.ok_or_else(|| invalid("missing knowledge checksum"))?;
    if cytiq_provenance::hash_file(path)?.1 != expected {
        return Err(invalid("knowledge checksum mismatch"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Class {
    id: String,
    index: usize,
    kind: String,
    target: String,
    state: Option<bool>,
    parent: Option<String>,
    residual_definition: Option<ResidualDefinition>,
}

#[derive(Debug, Deserialize)]
struct ResidualDefinition {
    scope: String,
    excluded_populations: Vec<String>,
    evidence: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Vocabulary {
    schema_version: u32,
    artifact_type: String,
    task: String,
    task_kind: String,
    decision_rule: String,
    unknown_policy: String,
    panel_id: String,
    scope: String,
    tree_sha256: String,
    panel_sha256: String,
    registry_sha256: String,
    projection_sha256: String,
    classes: Vec<Class>,
    hierarchy: BTreeMap<String, Option<String>>,
}

impl Vocabulary {
    fn validate(&self) -> Result<(), ClassifyError> {
        if self.schema_version != 2
            || self.artifact_type != "class_vocabulary"
            || self.unknown_policy != "disabled"
            || self.classes.len() < 2
            || !self.hierarchy.contains_key(&self.scope)
        {
            return Err(invalid("unsupported class vocabulary"));
        }
        let mut ids = HashSet::new();
        for (i, class) in self.classes.iter().enumerate() {
            if class.index != i
                || class.id.is_empty()
                || class.id == "unknown"
                || !ids.insert(&class.id)
            {
                return Err(invalid("invalid class ids/order"));
            }
        }
        if self
            .hierarchy
            .values()
            .filter(|parent| parent.is_none())
            .count()
            != 1
        {
            return Err(invalid("vocabulary requires one hierarchy root"));
        }
        for node in self.hierarchy.keys() {
            self.ancestors(node)?;
        }
        for hash in [
            &self.tree_sha256,
            &self.panel_sha256,
            &self.registry_sha256,
            &self.projection_sha256,
        ] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            {
                return Err(invalid("invalid vocabulary digest"));
            }
        }
        self.validate_targets()
    }

    fn ancestors<'a>(&'a self, node: &'a str) -> Result<HashSet<&'a str>, ClassifyError> {
        let mut visited = HashSet::new();
        let mut current = Some(node);
        while let Some(id) = current {
            if !visited.insert(id) {
                return Err(invalid("cyclic vocabulary hierarchy"));
            }
            let parent = self
                .hierarchy
                .get(id)
                .ok_or_else(|| invalid("missing vocabulary parent"))?;
            current = parent.as_deref();
        }
        Ok(visited)
    }

    fn validate_targets(&self) -> Result<(), ClassifyError> {
        if self.task_kind == "binary_phenotype" && self.decision_rule == "binary_threshold" {
            if self.classes.len() != 2
                || self.classes[0].state != Some(false)
                || self.classes[1].state != Some(true)
                || self.classes[0].target != self.classes[1].target
                || self.classes.iter().any(|c| c.kind != "phenotype")
            {
                return Err(invalid("invalid binary phenotype vocabulary"));
            }
            return Ok(());
        }
        if self.task_kind != "multiclass" || self.decision_rule != "argmax" {
            return Err(invalid("unsupported task decision rule"));
        }
        let targets: HashSet<_> = self.classes.iter().map(|c| c.target.as_str()).collect();
        if targets.len() != self.classes.len() {
            return Err(invalid("duplicate class targets"));
        }
        for class in &self.classes {
            if class.kind == "residual" {
                let definition = class
                    .residual_definition
                    .as_ref()
                    .ok_or_else(|| invalid("missing residual definition"))?;
                let expected: HashSet<_> = self
                    .classes
                    .iter()
                    .filter(|c| c.kind == "population")
                    .map(|c| c.target.as_str())
                    .collect();
                let excluded: HashSet<_> = definition
                    .excluded_populations
                    .iter()
                    .map(String::as_str)
                    .collect();
                if definition.scope != self.scope
                    || excluded != expected
                    || definition.evidence.is_empty()
                {
                    return Err(invalid("invalid residual scope/exclusions"));
                }
                continue;
            }
            if class.kind != "population"
                || self.hierarchy.get(&class.target) != Some(&class.parent)
            {
                return Err(invalid("invalid class target"));
            }
            let ancestors = self.ancestors(&class.target)?;
            if !ancestors.contains(self.scope.as_str())
                || ancestors
                    .iter()
                    .any(|id| *id != class.target && targets.contains(id))
            {
                return Err(invalid("invalid multiclass frontier"));
            }
        }
        Ok(())
    }
}

/// The complete set of portable files to record before inference.
///
/// # Errors
/// Returns an error when the bundle manifest cannot be read.
pub fn input_files(directory: &Path) -> Result<Vec<PathBuf>, ClassifyError> {
    let (manifest_path, payload) = manifest(directory)?;
    let mut paths = vec![directory.join("model.onnx"), manifest_path];
    if matches!(payload["schema_version"].as_u64(), Some(2 | 3)) {
        paths.extend(
            ["classes.json", "panel.json", "label-mapping.json"].map(|name| directory.join(name)),
        );
    } else if payload["schema_version"] == 4 {
        for stem in ["classes", "panel", "label-mapping"] {
            paths.push(serialization::resolve(directory, stem)?);
        }
    }
    Ok(paths)
}
