//! Synthetic operating-point evidence for software-contract tests only.
use cytiq_population::{ArtifactRef, serialization};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
pub fn policy(
    directory: &Path,
    bundle: &Path,
    cutoff: Option<f32>,
    scorer: Option<&Path>,
) -> PathBuf {
    let manifest = serialization::resolve(bundle, "manifest").unwrap();
    let bundle = serialization::read(&manifest).unwrap();
    let mut candidate = json!({"schema_version":1,"artifact_type":"classifier_decision_policy",
        "bundle_manifest_sha256":ArtifactRef::of(&manifest).unwrap().sha256,
        "model_sha256":bundle["model_sha256"],"profile_sha256":serialization::digest(&bundle["profile"]),
        "classes":bundle["classes"],"normalization":bundle["normalization"],"min_confidence":cutoff,
        "ood":scorer.map(|path|json!({"model":ArtifactRef::of(path).unwrap(),"threshold":0.0}))});
    let report = json!({"schema_version":1,"artifact_type":"classifier_decision_validation",
        "policy_sha256":serialization::digest(&candidate),
        "evaluation_sha256":"a".repeat(64),"evaluator_sha256":"b".repeat(64),
        "groups":{"train":[format!("{:064x}",1)],"calibration":[format!("{:064x}",2)],
            "selection":[format!("{:064x}",3)],"test":[format!("{:064x}",4)],
            "ood_test":if scorer.is_some(){vec![format!("{:064x}",5)]}else{vec![]}},
        "known_events":10,"ood_events":if scorer.is_some(){10}else{0},
        "metrics":{"brier":0.01,"ece":0.01,"risk":0.01,"coverage":0.9,
            "ood_false_accept_rate":scorer.map(|_|0.0)},
        "limits":{"max_brier":0.1,"max_ece":0.1,"max_risk":0.1,"min_coverage":0.8,
            "max_ood_false_accept_rate":scorer.map(|_|0.1)}});
    let validation = directory.join("validation.json");
    write(&validation, &report);
    candidate["validation"] = serde_json::to_value(ArtifactRef::of(&validation).unwrap()).unwrap();
    let path = directory.join("policy.json");
    write(&path, &candidate);
    path
}
