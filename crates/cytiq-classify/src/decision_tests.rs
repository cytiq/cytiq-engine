use crate::decision::assess;
use cytiq_population::{
    ArtifactRef,
    decision::{DecisionPolicy, OodPolicy},
};

#[test]
fn validated_cutoffs_keep_confidence_ood_and_unassessed_states_distinct() {
    let reference = ArtifactRef {
        path: "unused".into(),
        sha256: "a".repeat(64),
    };
    let policy = DecisionPolicy {
        schema_version: 1,
        artifact_type: "classifier_decision_policy".into(),
        bundle_manifest_sha256: "a".repeat(64),
        model_sha256: "b".repeat(64),
        profile_sha256: "c".repeat(64),
        classes: vec!["negative".into(), "positive".into()],
        normalization: serde_json::Value::Null,
        min_confidence: Some(0.8),
        ood: Some(OodPolicy {
            model: reference.clone(),
            threshold: 0.5,
        }),
        validation: reference,
    };
    let boundary = assess(&policy, Some(&[0.8, 0.2]), Some(0.49));
    assert_eq!(boundary.status, "classified");
    let ood = assess(&policy, Some(&[0.99, 0.01]), Some(0.5));
    assert_eq!(ood.reason, Some("ood"));
    let both = assess(&policy, Some(&[0.7, 0.3]), Some(0.6));
    assert_eq!(both.reason, Some("low_confidence_and_ood"));
    let excluded = assess(&policy, None, None);
    assert_eq!(excluded.status, "not_assessed");
    assert!(excluded.confidence.is_none() && excluded.is_ood.is_none());
}
