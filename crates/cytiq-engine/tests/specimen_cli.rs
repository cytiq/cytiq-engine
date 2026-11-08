#[path = "../../cytiq-classify/tests/support/decision.rs"]
mod decision_support;
use cytiq_mask::EventMask;
use cytiq_population::{
    ArtifactRef,
    artifact::{verify_package, write_json},
    knowledge::KnowledgeArtifact,
    serialization,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn cytiq(args: &[&str], code: i32) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn outcome(args: &[&str], code: i32) -> Value {
    let output = cytiq(args, code);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["exit_code"], code);
    result
}
fn digest(path: &Path) -> String {
    KnowledgeArtifact::load(path).unwrap().sha256().into()
}
fn raw_mask(raw: &Path, path: &Path) {
    let dataset = cytiq_fcs::FcsReader::open(raw).unwrap().read().unwrap();
    let sha = cytiq_fcs::acquisition_id(raw, &dataset.text).unwrap();
    EventMask::new(sha, vec![true; dataset.event_count()], None)
        .write_parquet(path)
        .unwrap();
}
fn target(id: &str) -> Value {
    json!({"kind":"population","id":id,"scope":"cells"})
}
fn gate_panel(name: &str) -> Value {
    let projection = fixture(&format!("specimen/{name}.projection.json"));
    json!({"projection":{"path":projection,"sha256":digest(&projection)},"sources":{"gates":{
    "format":"gate_membership","definition":ArtifactRef::of(&fixture("specimen/gates.xml")).unwrap(),"bindings":[
        {"label":"All","target":target("cells")},{"label":"T","target":target("t_cells"),"denominator":"All"},
        {"label":"Activated","target":target("activated"),"denominator":"All"},{"label":"Zero","target":target("zero"),"denominator":"All"}
    ]}}})
}

#[allow(clippy::too_many_arguments)] // Mirrors the independently supplied CLI artifacts.
fn observe_args<'a>(
    plan: &'a Path,
    acquisition: &'a str,
    source: &'a str,
    annotations: &'a Path,
    producer: &'a Path,
    mask: &'a Path,
    qc: &'a Path,
    qc_record: &'a Path,
    output: &'a Path,
) -> Vec<&'a str> {
    vec![
        "population",
        "observe",
        "--plan",
        text(plan),
        "--acquisition",
        acquisition,
        "--source",
        source,
        "--annotations",
        text(annotations),
        "--producer-provenance",
        text(producer),
        "--mask",
        text(mask),
        "--qc-report",
        text(qc),
        "--qc-provenance",
        text(qc_record),
        "--output",
        text(output),
    ]
}

#[test]
#[allow(clippy::too_many_lines)] // One complete user workflow, including recovery checks.
fn explicit_retake_selection_to_gates_to_specimen_is_atomic_and_preserves_overlap() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let manifest = directory.join("run.json");
    let layout = directory.join("layout.json");
    let raw = fixture("pipeline/sample.fcs");
    write_json(&manifest,&json!({"schema_version":2,"cytometry":"conventional","panels":[
        {"name":"P1","markers":[{"marker":"CD3"}],"qc":{"min_events":1}},
        {"name":"P2","markers":[{"marker":"CD3"}],"qc":{"min_events":1}}],
        "samples":[{"id":"A01","file":raw,"panel":"P1","specimen":"S01"},{"id":"A02","file":raw,"panel":"P1","specimen":"S01"},{"id":"A03","file":raw,"panel":"P2","specimen":"S01"}]})).unwrap();
    write_json(&layout,&json!({"schema_version":1,"protocol_id":"synthetic-gates","panels":{"P1":gate_panel("P1"),"P2":gate_panel("P2")}})).unwrap();
    let planned = directory.join("planned");
    let plan = planned.join("plan.json");
    let base = vec![
        "specimen",
        "plan",
        "--manifest",
        text(&manifest),
        "--layout",
        text(&layout),
        "--specimen",
        "S01",
        "--output",
        text(&planned),
    ];
    assert_eq!(outcome(&base, 2)["reason"], "invalid_configuration");
    let mut args = base;
    args.extend(["--select", "P1=A02"]);
    outcome(&args, 0);
    let planned_value = serialization::read(&plan).unwrap();
    assert_eq!(planned_value["selections"][0]["selected"], "A02");
    assert_eq!(
        planned_value["selections"][0]["excluded"]["A01"],
        "not_selected"
    );
    let mask = directory.join("mask.parquet");
    raw_mask(&raw, &mask);
    let mut packages = Vec::new();
    for acquisition in ["A02", "A03"] {
        let qc = directory.join(format!("{acquisition}.qc.json"));
        cytiq(
            &[
                "qc",
                "--manifest",
                text(&manifest),
                "--sample",
                acquisition,
                "--mask",
                text(&mask),
                "--output",
                text(&qc),
            ],
            0,
        );
        let counts = directory.join(format!("{acquisition}.counts.json"));
        let membership = directory.join(format!("{acquisition}.parquet"));
        cytiq(
            &[
                "gate",
                "--input",
                text(&raw),
                "--gating-ml",
                text(&fixture("specimen/gates.xml")),
                "--mask",
                text(&mask),
                "--membership",
                text(&membership),
                "--output",
                text(&counts),
                "--format",
                "json",
            ],
            0,
        );
        let producer = cytiq_provenance::record_path_for_file(&counts);
        let qc_record = cytiq_provenance::record_path_for_file(&qc);
        let package = directory.join(acquisition);
        outcome(
            &observe_args(
                &plan,
                acquisition,
                "gates",
                &membership,
                &producer,
                &mask,
                &qc,
                &qc_record,
                &package,
            ),
            0,
        );
        verify_package(&package).unwrap();
        packages.push(package);
    }
    let result = directory.join("result");
    let run = vec![
        "specimen",
        "run",
        "--plan",
        text(&plan),
        "--observations",
        text(&packages[0]),
        "--observations",
        text(&packages[1]),
        "--output",
        text(&result),
    ];
    outcome(&run, 0);
    verify_package(&result).unwrap();
    cytiq_specimen::SpecimenResult::load(&result).unwrap();
    let summary = serialization::read(&result.join("result.json")).unwrap();
    assert_eq!(summary["complete"], true);
    assert!(
        summary["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["target"]["id"] == "zero"
                && r["value_u64"] == 0
                && r["status"] == "measured")
    );
    assert!(
        summary["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["acquisition_id"] != "A01")
    );
    let original = fs::read(result.join("outcome.json")).unwrap();
    let stamp = fs::metadata(result.join("result.json"))
        .unwrap()
        .modified()
        .unwrap();
    let mut reused = run.clone();
    reused.extend(["--reuse", "skip"]);
    assert_eq!(outcome(&reused, 0)["execution"]["reused"], true);
    assert_eq!(
        fs::metadata(result.join("result.json"))
            .unwrap()
            .modified()
            .unwrap(),
        stamp
    );
    assert_eq!(fs::read(result.join("outcome.json")).unwrap(), original);
    // Simulate a process stopping after moving the previous package aside.
    let backup = directory.join(".result.abc123.old.tmp");
    fs::create_dir(&backup).unwrap();
    fs::rename(&result, backup.join("previous")).unwrap();
    assert_eq!(outcome(&reused, 0)["execution"]["reused"], true);
    assert!(!backup.exists());
    assert_eq!(outcome(&run, 4)["reason"], "output_conflict");
    assert_eq!(fs::read(result.join("outcome.json")).unwrap(), original);
    let mut overwrite = run;
    overwrite.push("--overwrite");
    outcome(&overwrite, 0);
    let partial = directory.join("partial");
    outcome(
        &[
            "specimen",
            "run",
            "--plan",
            text(&plan),
            "--observations",
            text(&packages[0]),
            "--output",
            text(&partial),
        ],
        4,
    );
    assert_eq!(
        serialization::read(&partial.join("result.json")).unwrap()["complete"],
        false
    );
    fs::write(packages[1].join("observations.parquet"), b"broken").unwrap();
    assert_eq!(outcome(&overwrite, 2)["completed"], false);
    verify_package(&result).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)] // Complete QC -> population -> specimen release boundary.
fn release_refuses_missing_empty_and_unassessed_mandatory_policies_but_keeps_evidence() {
    for policy in [
        Value::Null,
        json!({}),
        json!({"filters":{"live":{"required":true,"severity":"warn"}}}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let raw = fixture("pipeline/sample.fcs");
        let manifest = root.join("run.json");
        let layout = root.join("layout.json");
        write_json(
            &manifest,
            &json!({"schema_version":2,"cytometry":"conventional",
            "panel":{"name":"P1","markers":[{"marker":"CD3"}],"qc":policy},
            "samples":[{"id":"A01","specimen":"S01","file":raw}]}),
        )
        .unwrap();
        write_json(&layout,&json!({"schema_version":1,"protocol_id":"release-contract","panels":{"P1":gate_panel("P1")}})).unwrap();
        let planned = root.join("planned");
        let plan = planned.join("plan.json");
        outcome(
            &[
                "specimen",
                "plan",
                "--manifest",
                text(&manifest),
                "--layout",
                text(&layout),
                "--specimen",
                "S01",
                "--output",
                text(&planned),
            ],
            0,
        );
        let mask = root.join("mask.parquet");
        raw_mask(&raw, &mask);
        let qc = root.join("qc.json");
        cytiq(
            &[
                "qc",
                "--manifest",
                text(&manifest),
                "--sample",
                "A01",
                "--mask",
                text(&mask),
                "--output",
                text(&qc),
            ],
            0,
        );
        assert_eq!(serialization::read(&qc).unwrap()["status"], "warn");
        let counts = root.join("counts.json");
        let membership = root.join("membership.parquet");
        cytiq(
            &[
                "gate",
                "--input",
                text(&raw),
                "--gating-ml",
                text(&fixture("specimen/gates.xml")),
                "--mask",
                text(&mask),
                "--membership",
                text(&membership),
                "--output",
                text(&counts),
                "--format",
                "json",
            ],
            0,
        );
        let observed = root.join("observed");
        let run = outcome(
            &observe_args(
                &plan,
                "A01",
                "gates",
                &membership,
                &cytiq_provenance::record_path_for_file(&counts),
                &mask,
                &qc,
                &cytiq_provenance::record_path_for_file(&qc),
                &observed,
            ),
            3,
        );
        assert_eq!(run["reason"], "checks_failed");
        assert_eq!(run["completed"], true);
        verify_package(&observed).unwrap();
        let checks = serialization::read(&observed.join("release-qc.json")).unwrap();
        assert!(
            checks["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["status"] == "fail")
        );
        let result = root.join("result");
        outcome(
            &[
                "specimen",
                "run",
                "--plan",
                text(&plan),
                "--observations",
                text(&observed),
                "--output",
                text(&result),
            ],
            3,
        );
        let summary = serialization::read(&result.join("result.json")).unwrap();
        assert_eq!(summary["status"], "fail");
        assert!(
            summary["observations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["status"] == "qc_rejected" && o["value_u64"].is_null())
        );
        assert!(observed.join("observations.parquet").exists());
    }
}

fn cytof_bundle(directory: &Path) -> (PathBuf, PathBuf) {
    let original = fixture("classify/cytof/lightgbm");
    let output = directory.join("cytof-model");
    fs::create_dir(&output).unwrap();
    let mut panel = serialization::read(&original.join("panel.yaml")).unwrap();
    let mut vocabulary = serialization::read(&original.join("classes.yaml")).unwrap();
    let evidence = json!([{"source":"synthetic-contract","reference":"software-only"}]);
    let mut cells = vec![
        json!({"id":"leukocytes","name":"Qualified events","parent":null,"evidence":evidence}),
    ];
    for class in vocabulary["classes"].as_array().unwrap() {
        cells.push(json!({"id":class["target"],"name":class["target"],"parent":"leukocytes","evidence":evidence}));
    }
    let markers = panel["features"].as_array().unwrap().iter().map(|f| json!({"id":f["marker_id"],"measurement_kind":if f["fluorescence"] == true {"protein"} else {"derived"}})).collect::<Vec<_>>();
    let tree = KnowledgeArtifact::from_value(json!({"schema_version":2,"artifact_type":"curated_tree","version":"0.1.0","generated_at":"2026-11-06T00:00:00Z","context":panel["context"],"cells":cells,"registry":{"schema_version":2,"artifact_type":"marker_registry","version":"0.1.0","markers":markers},"tasks":[]})).unwrap();
    let required_features = panel["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].clone())
        .collect::<Vec<_>>();
    panel["resolution_rules"] = Value::Array(tree.value()["cells"].as_array().unwrap().iter().map(|c| json!({"population":c["id"],"required_features":required_features,"evidence":evidence})).collect());
    let panel = KnowledgeArtifact::from_value(panel).unwrap();
    let entries = tree.value()["cells"].as_array().unwrap().iter().map(|c| json!({"population":c["id"],"status":"resolvable","reason":"accepted panel-specific rule covered"})).collect::<Vec<_>>();
    let projection = KnowledgeArtifact::from_value(json!({"schema_version":2,"artifact_type":"panel_projection","version":"0.1.0","tree":tree.value(),"panel":panel.value(),"tree_sha256":tree.sha256(),"panel_sha256":panel.sha256(),"populations":entries})).unwrap();
    vocabulary["tree_sha256"] = json!(tree.sha256());
    vocabulary["projection_sha256"] = json!(projection.sha256());
    vocabulary["panel_sha256"] = json!(panel.sha256());
    vocabulary["registry_sha256"] = json!(serialization::digest(&tree.value()["registry"]));
    let vocabulary = KnowledgeArtifact::from_value(vocabulary).unwrap();
    let mut mapping = serialization::read(&original.join("label-mapping.yaml")).unwrap();
    mapping["vocabulary_sha256"] = json!(vocabulary.sha256());
    let mapping = KnowledgeArtifact::from_value(mapping).unwrap();
    let mut manifest = serialization::read(&original.join("manifest.yaml")).unwrap();
    manifest["profile"] = panel.value().clone();
    manifest["panel_sha256"] = json!(panel.sha256());
    manifest["vocabulary_sha256"] = json!(vocabulary.sha256());
    manifest["label_mapping_sha256"] = json!(mapping.sha256());
    for (name, value) in [
        ("panel", panel.value()),
        ("classes", vocabulary.value()),
        ("label-mapping", mapping.value()),
        ("manifest", &manifest),
    ] {
        write_json(&output.join(format!("{name}.json")), value).unwrap();
    }
    fs::copy(original.join("model.onnx"), output.join("model.onnx")).unwrap();
    let projection_path = directory.join("cytof.projection.json");
    write_json(&projection_path, projection.value()).unwrap();
    (output, projection_path)
}

#[test]
#[allow(clippy::too_many_lines)] // Exercise both existing inference recipes through the CLI.
fn conventional_and_cytof_classifiers_reach_specimen_results_with_their_target_kinds() {
    for (cytof, qc_failure, use_policy) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let (model, projection, raw, panel) = if cytof {
            let (model, projection) = cytof_bundle(directory);
            (
                model,
                projection,
                fixture("classify/cytof/input.fcs"),
                "synthetic_cytof",
            )
        } else {
            (
                fixture("classify/v2/binary"),
                fixture("specimen/facscanto2-p1.projection.json"),
                fixture("classify/raw.fcs"),
                "P1",
            )
        };
        let manifest = directory.join("run.json");
        let layout = directory.join("layout.json");
        write_json(&manifest,&json!({"schema_version":2,"cytometry":if cytof {"cytof"} else {"conventional"},"panels":[{"name":panel,"markers":[{"marker":"CD3"}],"qc":{"min_events":if qc_failure {u64::MAX} else {1}}}],"samples":[{"id":"A01","panel":panel,"specimen":"S01","file":raw}]})).unwrap();
        if cytof {
            let preflight = directory.join("preflight.json");
            cytiq(
                &[
                    "preflight",
                    "--manifest",
                    text(&manifest),
                    "--output",
                    text(&preflight),
                ],
                0,
            );
            let checks = serialization::read(&preflight).unwrap();
            assert!(
                checks["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["check"] != "spillover" && c["check"] != "control_coverage")
            );
        }
        let vocabulary = model.join("classes.json");
        let policy =
            use_policy.then(|| decision_support::policy(directory, &model, Some(1.0), None));
        write_json(&layout,&json!({"schema_version":1,"protocol_id":"synthetic-ml","panels":{panel:{"projection":{"path":projection,"sha256":digest(&projection)},"sources":{"ml":{
            "format":"classify_predictions","definition":ArtifactRef::of(&model.join("model.onnx")).unwrap(),"bundle_manifest":ArtifactRef::of(&model.join("manifest.json")).unwrap(),"abstain_margin":if cytof || use_policy {0.0} else {0.4},"vocabulary":{"path":vocabulary,"sha256":digest(&vocabulary)},"decision_policy":policy.as_ref().map(|path|ArtifactRef::of(path).unwrap())
        }}}}})).unwrap();
        let planned = directory.join("planned");
        let plan = planned.join("plan.json");
        outcome(
            &[
                "specimen",
                "plan",
                "--manifest",
                text(&manifest),
                "--layout",
                text(&layout),
                "--specimen",
                "S01",
                "--output",
                text(&planned),
            ],
            0,
        );
        let mask = directory.join("mask.parquet");
        raw_mask(&raw, &mask);
        let qc = directory.join("qc.json");
        cytiq(
            &[
                "qc",
                "--manifest",
                text(&manifest),
                "--sample",
                "A01",
                "--mask",
                text(&mask),
                "--output",
                text(&qc),
            ],
            if qc_failure { 3 } else { 0 },
        );
        let classified = directory.join("classified");
        let mut args = vec![
            "classify",
            "--input",
            text(&raw),
            "--model",
            text(&model),
            "--mask",
            text(&mask),
            "--output",
            text(&classified),
            "--threads",
            "1",
            "--batch-size",
            "17",
        ];
        if !cytof && !use_policy {
            args.extend(["--abstain-margin", "0.4"]);
        }
        if let Some(policy) = &policy {
            args.extend(["--decision-policy", text(policy)]);
        }
        cytiq(&args, 0);
        let package = directory.join("observed");
        outcome(
            &observe_args(
                &plan,
                "A01",
                "ml",
                &classified.join("predictions.parquet"),
                &classified.join("provenance.json"),
                &mask,
                &qc,
                &cytiq_provenance::record_path_for_file(&qc),
                &package,
            ),
            if qc_failure {
                3
            } else if !cytof {
                4
            } else {
                0
            },
        );
        let descriptor = serialization::read(&package.join("event-annotations.json")).unwrap();
        if use_policy {
            assert_eq!(descriptor["schema_version"], 2);
            assert!(descriptor["abstained_events"].as_u64().unwrap() > 0);
            assert_eq!(descriptor["unknown_events"], 0);
        } else if !cytof {
            assert!(descriptor["unknown_events"].as_u64().unwrap() > 0);
        }
        let result = directory.join("result");
        outcome(
            &[
                "specimen",
                "run",
                "--plan",
                text(&plan),
                "--observations",
                text(&package),
                "--output",
                text(&result),
            ],
            if qc_failure {
                3
            } else if !cytof {
                4
            } else {
                0
            },
        );
        let summary = serialization::read(&result.join("result.json")).unwrap();
        if qc_failure {
            assert_eq!(summary["status"], "fail");
            assert!(
                summary["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|o| o["status"] == "qc_rejected"
                        && o["value_u64"].is_null()
                        && o["value_f64"].is_null())
            );
        }
        if !cytof && !qc_failure {
            assert_eq!(summary["complete"], false);
            assert!(
                summary["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|o| o["status"] == "not_measured"
                        && o["value_u64"].is_null()
                        && o["value_f64"].is_null())
            );
        }
        assert!(
            summary["observations"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["target"]["kind"] == if cytof { "population" } else { "phenotype" })
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn spectral_unmixing_membership_reaches_a_specimen_without_fcm_classification() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let input = fixture("autospectral");
    let unmixed = directory.join("unmixed");
    cytiq(
        &[
            "autospectral",
            "--controls",
            text(&input.join("controls.csv")),
            "--samples",
            text(&input.join("samples.csv")),
            "--input-dir",
            text(&input),
            "--output",
            text(&unmixed),
            "--quantile-gate",
            "--threads",
            "1",
        ],
        0,
    );
    let raw = input.join("sample.fcs");
    let manifest = directory.join("run.json");
    let layout = directory.join("layout.json");
    write_json(&manifest,&json!({"schema_version":2,"cytometry":"spectral","panel":{"name":"P1","markers":[{"marker":"FITC","channel":"FITC"}],"qc":{"min_kept_events":1}},"samples":[{"id":"A01","specimen":"S01","file":raw}],"controls":[{"file":input.join("unstained.fcs"),"dye":"Autofluorescence"}]})).unwrap();
    let projection_path = directory.join("spectral.projection.json");
    let mut projection = serialization::read(&fixture("specimen/P1.projection.json")).unwrap();
    projection["panel"]["features"][0]["channel"] = json!("FITC");
    projection["panel_sha256"] = json!(serialization::digest(&projection["panel"]));
    let projection = KnowledgeArtifact::from_value(projection).unwrap();
    write_json(&projection_path, projection.value()).unwrap();
    let mut panel = gate_panel("P1");
    panel["projection"] = json!({"path":projection_path,"sha256":projection.sha256()});
    write_json(
        &layout,
        &json!({"schema_version":1,"protocol_id":"spectral-contract","panels":{"P1":panel}}),
    )
    .unwrap();
    let planned = directory.join("planned");
    let plan = planned.join("plan.json");
    outcome(
        &[
            "specimen",
            "plan",
            "--manifest",
            text(&manifest),
            "--layout",
            text(&layout),
            "--specimen",
            "S01",
            "--output",
            text(&planned),
        ],
        0,
    );
    let mask = unmixed.join("unmixed/sample.unmixed.masks.parquet");
    let qc = directory.join("qc.json");
    cytiq(
        &[
            "qc",
            "--manifest",
            text(&manifest),
            "--sample",
            "A01",
            "--mask",
            text(&mask),
            "--output",
            text(&qc),
        ],
        0,
    );
    let counts = directory.join("counts.json");
    let membership = directory.join("membership.parquet");
    cytiq(
        &[
            "gate",
            "--input",
            text(&unmixed.join("unmixed/sample.unmixed.fcs")),
            "--gating-ml",
            text(&fixture("specimen/gates.xml")),
            "--mask",
            text(&mask),
            "--membership",
            text(&membership),
            "--output",
            text(&counts),
            "--format",
            "json",
        ],
        0,
    );
    let observed = directory.join("observed");
    outcome(
        &observe_args(
            &plan,
            "A01",
            "gates",
            &membership,
            &cytiq_provenance::record_path_for_file(&counts),
            &mask,
            &qc,
            &cytiq_provenance::record_path_for_file(&qc),
            &observed,
        ),
        0,
    );
    let result = directory.join("result");
    outcome(
        &[
            "specimen",
            "run",
            "--plan",
            text(&plan),
            "--observations",
            text(&observed),
            "--output",
            text(&result),
        ],
        0,
    );
    verify_package(&result).unwrap();
}

#[test]
fn reference_ratio_requires_validated_anchor_limits() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path();
    let manifest = directory.join("run.json");
    let raw = fixture("pipeline/sample.fcs");
    write_json(&manifest,&json!({"schema_version":2,"cytometry":"conventional","panels":[
        {"name":"P1","markers":[{"marker":"CD3"}],"qc":{"min_events":1}},
        {"name":"P2","markers":[{"marker":"CD3"}],"qc":{"min_events":1}}],
        "samples":[{"id":"A01","file":raw,"panel":"P1","specimen":"S01"},{"id":"A02","file":raw,"panel":"P2","specimen":"S01"}]})).unwrap();
    let validate = |name: &str, strategy: Value| {
        let layout = directory.join(name);
        write_json(&layout,&json!({"schema_version":1,"protocol_id":"ratio","panels":{"P1":gate_panel("P1"),"P2":gate_panel("P2")},
            "reconciliation":[{"target":target("t_cells"),"strategy":strategy}]})).unwrap();
        outcome(
            &[
                "specimen",
                "validate",
                "--manifest",
                text(&manifest),
                "--layout",
                text(&layout),
                "--specimen",
                "S01",
            ],
            if name == "valid.json" { 0 } else { 2 },
        )
    };
    let strategy = |limits: Option<Value>| {
        let mut strategy = json!({"kind":"reference_ratio","source":{"panel":"P2","source":"gates"},
            "reference":{"panel":"P1","source":"gates"},"anchor":target("cells")});
        if let Some(limits) = limits {
            strategy["anchor_limits"] = limits;
        }
        strategy
    };
    let missing = validate("missing.json", strategy(None));
    assert_eq!(missing["reason"], "invalid_configuration");
    assert!(
        missing["message"]
            .as_str()
            .unwrap()
            .contains("anchor_limits"),
        "{missing}"
    );
    let zero = validate("zero.json", strategy(Some(json!({"min_events":0}))));
    assert_eq!(zero["reason"], "invalid_configuration");
    validate(
        "valid.json",
        strategy(Some(
            json!({"min_events":50,"max_scale":4.0,"max_frequency_ratio":1.5}),
        )),
    );
}
