//! Pin inference settings as well as model bytes before accepting annotations.
use super::SourceDefinition;
use crate::{Result, knowledge::KnowledgeArtifact, require, serialization};
use cytiq_provenance::hash_file;

pub(super) fn validate_classifier_definition(
    source: &SourceDefinition,
    projection: &KnowledgeArtifact,
    vocabulary: &KnowledgeArtifact,
) -> Result<()> {
    let reference = source.bundle_manifest.as_ref().ok_or_else(|| {
        crate::PopulationError::Invalid("classifier source must pin bundle_manifest".into())
    })?;
    reference.verify()?;
    let bundle = serialization::read(&reference.path)?;
    if let Some(policy) = &source.decision_policy {
        policy.verify()?;
        crate::decision::DecisionPolicy::load(&policy.path)?.check_bundle(&reference.path)?;
        require(
            source.abstain_margin == 0.0,
            "decision policy conflicts with binary abstain margin",
        )?;
    }
    let version = bundle["schema_version"].as_u64();
    require(
        matches!(version, Some(2..=4)) && bundle["model_sha256"] == source.definition.sha256,
        "bundle/model identity mismatch",
    )?;
    let profile = KnowledgeArtifact::from_value(bundle["profile"].clone())?;
    require(
        profile.value == projection.value["panel"],
        "bundle feature profile differs from the panel projection",
    )?;
    let expected_vocabulary = if version == Some(4) {
        vocabulary.sha256.clone()
    } else {
        hash_file(
            &source
                .vocabulary
                .as_ref()
                .expect("classifier vocabulary")
                .path,
        )?
        .1
    };
    let classes = vocabulary.value["classes"]
        .as_array()
        .expect("validated vocabulary")
        .iter()
        .map(|c| &c["id"])
        .collect::<Vec<_>>();
    require(
        bundle["vocabulary_sha256"] == expected_vocabulary
            && bundle["classes"] == serde_json::json!(classes),
        "bundle vocabulary/classes mismatch",
    )?;
    require(
        source.abstain_margin.is_finite() && source.abstain_margin >= 0.0,
        "invalid planned abstain margin",
    )?;
    if vocabulary.value["task_kind"] == "multiclass" {
        require(
            source.abstain_margin == 0.0,
            "multiclass does not support binary abstention",
        )?;
    } else {
        let threshold = bundle["threshold"]
            .as_f64()
            .ok_or_else(|| crate::PopulationError::Invalid("missing binary threshold".into()))?;
        require(
            (0.0..=1.0).contains(&threshold)
                && source.abstain_margin <= threshold.min(1.0 - threshold),
            "invalid binary threshold/margin",
        )?;
    }
    let tree_parents = super::super::knowledge::parents(&projection.value["tree"]);
    let hierarchy = vocabulary.value["hierarchy"]
        .as_object()
        .expect("validated hierarchy");
    for (id, parent) in hierarchy {
        require(
            tree_parents
                .get(id)
                .is_some_and(|p| serde_json::to_value(p).ok().as_ref() == Some(parent)),
            "vocabulary hierarchy differs from its tree",
        )?;
    }
    Ok(())
}
