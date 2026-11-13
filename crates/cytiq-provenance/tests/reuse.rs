use cytiq_provenance::{
    BuildInfo, RunRecord, RunRecorder,
    outcome::OutcomeReason,
    reuse::{self, Decision, Destination, Prior, ReusePolicy},
};
use serde_json::json;

fn recorder() -> RunRecorder {
    let mut r =
        RunRecorder::start("cytiq stats", "1", vec!["stats".into()]).with_build(BuildInfo {
            git_commit: "a".repeat(40),
            rustc: "rustc-test".into(),
            target: "test".into(),
            profile: "debug".into(),
        });
    r.set_config(&json!({"bins": 64, "nested": {"threads": 2, "overwrite": false, "debug_dir": null, "dry_run": false}}));
    r.set_constants(&json!({"precision": 1}));
    r.set_algorithms(&[("algorithm", "1")]);
    r
}

#[test]
fn fingerprints_track_semantics_but_ignore_transport_and_bookkeeping() {
    let original = recorder().snapshot_record();
    let fingerprint = original.compute_fingerprint();
    assert_eq!(fingerprint.len(), 64);
    for field in [
        "timestamps",
        "arguments",
        "outputs",
        "platform",
        "schema",
        "options",
    ] {
        let mut r = original.clone();
        match field {
            "timestamps" => {
                r.started_at_unix_ms += 1;
                r.finished_at_unix_ms += 2;
            }
            "arguments" => r.arguments.push("--reuse".into()),
            "outputs" => r.outputs.push(cytiq_provenance::FileRecord {
                path: "new".into(),
                role: None,
                bytes: 0,
                sha256: String::new(),
            }),
            "platform" => r.platform.cpus += 1,
            "schema" => r.schema_version += 1,
            _ => {
                r.config["nested"] =
                    json!({"threads":32,"overwrite":true,"debug_dir":"debug","dry_run":true});
            }
        }
        assert_eq!(r.compute_fingerprint(), fingerprint, "{field}");
    }
    for field in [
        "tool",
        "version",
        "build",
        "config",
        "constants",
        "algorithms",
        "inputs",
        "parallelism",
    ] {
        let mut r = original.clone();
        match field {
            "tool" => r.tool.push('x'),
            "version" => r.version.push('x'),
            "build" => r.build.git_commit = "b".repeat(40),
            "config" => r.config["bins"] = json!(65),
            "constants" => r.constants["precision"] = json!(2),
            "algorithms" => {
                r.algorithms.insert("algorithm".into(), "2".into());
            }
            "inputs" => r.inputs.push(cytiq_provenance::FileRecord {
                path: "input".into(),
                role: Some("matrix".into()),
                bytes: 4,
                sha256: "c".repeat(64),
            }),
            _ => r.parallelism = Some(2),
        }
        assert_ne!(r.compute_fingerprint(), fingerprint, "{field}");
    }
}

#[test]
fn recorder_and_written_record_share_the_same_fingerprint() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("data.json");
    std::fs::write(&output, b"data").unwrap();
    let recorder = recorder();
    let expected = recorder.execution_fingerprint();
    let record = recorder
        .finish(
            std::slice::from_ref(&output),
            &cytiq_provenance::record_path_for_file(&output),
            OutcomeReason::ChecksFailed,
        )
        .unwrap();
    assert_eq!(
        record.execution_fingerprint.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(record.compute_fingerprint(), expected);
    assert_eq!(record.reason, Some(OutcomeReason::ChecksFailed));
}

#[test]
fn assessment_rejects_stale_unrecorded_and_tampered_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("data.json");
    let destination = Destination::File(output.clone());
    let expected = recorder().snapshot_record();
    assert!(matches!(
        reuse::assess(&destination, &expected),
        Prior::Absent
    ));
    std::fs::write(&output, b"data").unwrap();
    assert!(matches!(
        reuse::assess(&destination, &expected),
        Prior::Unrecorded
    ));
    let path = destination.record_path();
    let record = recorder()
        .finish(std::slice::from_ref(&output), &path, OutcomeReason::Success)
        .unwrap();
    assert!(matches!(
        reuse::assess(&destination, &expected),
        Prior::Match(_)
    ));
    let mut changed = expected.clone();
    changed.config["bins"] = json!(65);
    assert!(matches!(
        reuse::assess(&destination, &changed),
        Prior::Stale { .. }
    ));
    for bytes in [b"same".as_slice(), b"longer", b""] {
        std::fs::write(&output, bytes).unwrap();
        assert!(matches!(
            reuse::assess(&destination, &expected),
            Prior::Damaged
        ));
    }
    std::fs::remove_file(&output).unwrap();
    assert!(matches!(
        reuse::assess(&destination, &expected),
        Prior::Damaged
    ));
    std::fs::write(&output, b"data").unwrap();
    for (version, commit, damaged) in [
        (2, "a".repeat(40), false),
        (3, "unknown".into(), false),
        (3, format!("{}-dirty", "a".repeat(40)), false),
        (3, "b".repeat(40), true),
    ] {
        let mut r = record.clone();
        r.schema_version = version;
        r.build.git_commit = commit;
        std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
        let prior = reuse::assess(&destination, &expected);
        assert!(if damaged {
            matches!(prior, Prior::Damaged)
        } else {
            matches!(prior, Prior::Stale { .. })
        });
    }
    let mut old: serde_json::Value = serde_json::to_value(record).unwrap();
    old["schema_version"] = json!(2);
    for key in ["execution_fingerprint", "reason", "parallelism"] {
        old.as_object_mut().unwrap().remove(key);
    }
    let _: RunRecord = serde_json::from_value(old).unwrap();
}

#[test]
fn an_oversized_record_is_damaged_without_being_read() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("data.json");
    std::fs::write(&output, b"data").unwrap();
    let destination = Destination::File(output);
    // Sparse: a record larger than any document cytiq reads back.
    std::fs::File::create(destination.record_path())
        .unwrap()
        .set_len(cytiq_provenance::input::DOCUMENT_BYTES_MAX + 1)
        .unwrap();
    assert!(matches!(
        reuse::assess(&destination, &recorder().snapshot_record()),
        Prior::Damaged
    ));
}

#[test]
fn every_policy_has_a_decision_for_each_prior_state() {
    let mut matched = recorder().snapshot_record();
    matched.reason = Some(OutcomeReason::Success);
    let states = [
        Prior::Absent,
        Prior::Match(Box::new(matched)),
        Prior::Stale { why: "test".into() },
        Prior::Damaged,
        Prior::Unrecorded,
    ];
    for (policy, expected) in [
        (ReusePolicy::Recompute, [Decision::Compute; 5]),
        (
            ReusePolicy::Skip,
            [
                Decision::Compute,
                Decision::Reuse,
                Decision::Compute,
                Decision::Compute,
                Decision::Compute,
            ],
        ),
        (
            ReusePolicy::Reject,
            [
                Decision::Compute,
                Decision::Reuse,
                Decision::Conflict,
                Decision::Conflict,
                Decision::Conflict,
            ],
        ),
    ] {
        for (state, decision) in states.iter().zip(expected) {
            assert_eq!(reuse::decide(policy, state), decision);
        }
    }
}

#[test]
fn changed_input_bytes_are_rejected_even_when_size_and_mtime_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("output");
    std::fs::write(&input, b"old").unwrap();
    std::fs::write(&output, b"result").unwrap();
    let modified = std::fs::metadata(&input).unwrap().modified().unwrap();
    let mut recorder = recorder();
    recorder
        .snapshot_inputs("input", std::slice::from_ref(&input))
        .unwrap();
    std::fs::write(&input, b"new").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&input)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(recorder.verify_inputs().is_err());
    assert!(
        recorder
            .finish(
                &[output],
                &temp.path().join("provenance.json"),
                OutcomeReason::Success
            )
            .is_err()
    );
}
