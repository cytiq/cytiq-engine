//! Explicit selective-classification policies. Training and independent
//! scientific validation belong to cytiq-ml; this module checks their contract.
use crate::{ArtifactRef, Result, require, serialization};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OodPolicy {
    pub model: ArtifactRef,
    /// Scores at or above this validated threshold are rejected.
    pub threshold: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPolicy {
    pub schema_version: u32,
    pub artifact_type: String,
    pub bundle_manifest_sha256: String,
    pub model_sha256: String,
    pub profile_sha256: String,
    pub classes: Vec<String>,
    pub normalization: Value,
    pub min_confidence: Option<f32>,
    pub ood: Option<OodPolicy>,
    pub validation: ArtifactRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Validation {
    schema_version: u32,
    artifact_type: String,
    policy_sha256: String,
    evaluation_sha256: String,
    evaluator_sha256: String,
    /// Stable hashed subject/group IDs from the prepared split manifests.
    groups: BTreeMap<String, Vec<String>>,
    known_events: u64,
    ood_events: u64,
    metrics: EvaluationMetrics,
    limits: EvaluationLimits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationMetrics {
    brier: f64,
    ece: f64,
    risk: f64,
    coverage: f64,
    ood_false_accept_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvaluationLimits {
    max_brier: f64,
    max_ece: f64,
    max_risk: f64,
    min_coverage: f64,
    max_ood_false_accept_rate: Option<f64>,
}

impl DecisionPolicy {
    /// Load a policy and its independently evaluated operating point. Relative
    /// references resolve beside the policy. No missing policy threshold defaults.
    /// # Errors
    /// Rejects unsupported policies, changed evidence, split overlap and unmet limits.
    pub fn load(path: &Path) -> Result<Self> {
        let value = serialization::read(path)?;
        let mut policy: Self = serde_json::from_value(value.clone())?;
        require(
            policy.schema_version == 1 && policy.artifact_type == "classifier_decision_policy",
            "unsupported classifier decision policy",
        )?;
        require(
            policy.min_confidence.is_some() || policy.ood.is_some(),
            "empty classifier decision policy",
        )?;
        require(
            policy
                .min_confidence
                .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "invalid confidence cutoff",
        )?;
        require(
            policy
                .ood
                .as_ref()
                .is_none_or(|ood| ood.threshold.is_finite()),
            "invalid OOD threshold",
        )?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        policy.validation.path = base.join(&policy.validation.path);
        policy.validation.verify()?;
        if let Some(ood) = &mut policy.ood {
            ood.model.path = base.join(&ood.model.path);
            ood.model.verify()?;
        }
        let validation: Validation =
            serde_json::from_value(serialization::read(&policy.validation.path)?)?;
        let mut candidate = value;
        candidate
            .as_object_mut()
            .ok_or_else(|| {
                crate::PopulationError::Invalid("decision policy must be an object".into())
            })?
            .remove("validation");
        require(
            validation.schema_version == 1
                && validation.artifact_type == "classifier_decision_validation"
                && validation.policy_sha256 == serialization::digest(&candidate),
            "validation does not describe this decision policy",
        )?;
        validate_evaluation(&validation, policy.ood.is_some())?;
        Ok(policy)
    }

    /// Bind the operating point to exactly the model and signal contract evaluated.
    /// # Errors
    /// Rejects a different model, class order, preprocessing or normalization.
    pub fn check_bundle(&self, manifest: &Path) -> Result<()> {
        let bundle = serialization::read(manifest)?;
        require(
            ArtifactRef::of(manifest)?.sha256 == self.bundle_manifest_sha256
                && bundle["model_sha256"] == self.model_sha256
                && bundle["classes"] == serde_json::to_value(&self.classes)?
                && serialization::digest(&bundle["profile"]) == self.profile_sha256
                && bundle["normalization"] == self.normalization,
            "decision policy model or signal contract mismatch",
        )
    }

    #[must_use]
    pub fn input_files(&self, policy_path: &Path) -> Vec<PathBuf> {
        let mut files = vec![policy_path.to_owned(), self.validation.path.clone()];
        if let Some(ood) = &self.ood {
            files.push(ood.model.path.clone());
        }
        files
    }
}

fn validate_evaluation(report: &Validation, ood: bool) -> Result<()> {
    for hash in [&report.evaluation_sha256, &report.evaluator_sha256] {
        require(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "missing evaluation data/evaluator identity",
        )?;
    }
    let roles = ["train", "calibration", "selection", "test", "ood_test"];
    require(
        report.groups.len() == roles.len()
            && roles.iter().all(|role| report.groups.contains_key(*role)),
        "validation must identify train/calibration/selection/test/OOD groups",
    )?;
    let mut seen = BTreeSet::new();
    for role in roles {
        let groups = &report.groups[role];
        require(
            (role == "ood_test" && !ood) || !groups.is_empty(),
            "missing independent validation groups",
        )?;
        require(groups.len() <= 100_000, "excessive validation groups")?;
        for group in groups {
            require(
                group.len() == 64
                    && group
                        .bytes()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                    && seen.insert(group),
                "overlapping or invalid validation group IDs",
            )?;
        }
    }
    let m = &report.metrics;
    let l = &report.limits;
    require(
        report.known_events > 0 && (!ood || report.ood_events > 0),
        "empty independent evaluation",
    )?;
    let comparisons = [
        (m.brier, l.max_brier, 2.0),
        (m.ece, l.max_ece, 1.0),
        (m.risk, l.max_risk, 1.0),
    ];
    for (value, limit, maximum) in comparisons {
        require(
            value.is_finite()
                && limit.is_finite()
                && (0.0..=maximum).contains(&value)
                && (0.0..=maximum).contains(&limit)
                && value <= limit,
            "decision validation acceptance limit failed",
        )?;
    }
    require(
        m.coverage.is_finite()
            && l.min_coverage.is_finite()
            && (0.0..=1.0).contains(&m.coverage)
            && (0.0..=1.0).contains(&l.min_coverage)
            && m.coverage >= l.min_coverage
            && m.coverage > 0.0,
        "decision validation coverage failed",
    )?;
    match (ood, m.ood_false_accept_rate, l.max_ood_false_accept_rate) {
        (true, Some(value), Some(limit))
            if value.is_finite()
                && limit.is_finite()
                && (0.0..=1.0).contains(&value)
                && (0.0..=1.0).contains(&limit)
                && value <= limit => {}
        (false, None, None) => {}
        _ => require(false, "missing or failed independent OOD evaluation")?,
    }
    Ok(())
}
