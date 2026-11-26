//! `cytiq repair`: name the deviations of a file, and change nothing.

#[path = "../../cytiq-repair/tests/common/mod.rs"]
mod files;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use files::Spec;
use serde_json::{Value, json};

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

/// The plan of a file holding `bytes`, and its path.
fn plan(root: &Path, bytes: &[u8]) -> Value {
    let input = root.join("input.fcs");
    fs::write(&input, bytes).unwrap();
    let output = cytiq(&["repair", "--input", text(&input), "--plan"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&input).unwrap(), bytes, "the input is untouched");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn codes(plan: &Value) -> Vec<&str> {
    plan["repairs"]
        .as_array()
        .map(|repairs| {
            repairs
                .iter()
                .map(|repair| repair["code"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_plan_names_the_repairs_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let spec = Spec {
        padding: b"   ".to_vec(),
        raw_text: b"$OP/Person//$EXP/Name/".to_vec(),
        data_end_past: 1,
        ..Spec::default()
    };
    let bytes = Spec {
        raw_text: b"$INST//$EXP/Person Name/".to_vec(),
        ..spec
    }
    .build();
    let plan = plan(root.path(), &bytes);
    assert_eq!(plan["contract"], "cytiq.repair.plan.v1");
    assert_eq!(plan["status"], "repairable");
    let mut found = codes(&plan);
    found.sort_unstable();
    assert_eq!(
        found,
        [
            "data_end_exclusive",
            "text_empty_values",
            "text_trailing_padding"
        ]
    );
    let digest = cytiq_provenance::hash_file(&root.path().join("input.fcs"))
        .unwrap()
        .1;
    assert_eq!(plan["acquisition_id"], digest);
    assert_eq!(plan["data"]["events"], 3);
    assert_eq!(plan["data"]["bytes"], 24);
    assert!(plan["reader"].as_str().unwrap().contains("delimiter"));
    let document = plan.to_string();
    assert!(!document.contains("Person"), "no keyword value is printed");
    assert!(!document.contains(text(root.path())), "no path is printed");
    let names: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
    assert_eq!(names.len(), 1, "no record and no output");
}

#[test]
fn conforming_and_refused_files_say_so() {
    let root = tempfile::tempdir().unwrap();
    let conforming = plan(root.path(), &Spec::default().build());
    assert_eq!(conforming["status"], "conforming");
    assert!(codes(&conforming).is_empty() && conforming.get("refusal").is_none());

    let refused = plan(
        root.path(),
        &Spec {
            header: [None, Some((400, 423)), None],
            ..Spec::default()
        }
        .build(),
    );
    assert_eq!(refused["status"], "refused");
    assert_eq!(refused["refusal"]["code"], "ambiguous_data_offsets");
    assert_eq!(refused.get("repairs"), None);

    let absent = cytiq(&[
        "repair",
        "--input",
        text(&root.path().join("absent.fcs")),
        "--plan",
    ]);
    assert_eq!(absent.status.code(), Some(4));
    assert_eq!(cytiq(&["repair", "--plan"]).status.code(), Some(2));
    let input = root.path().join("input.fcs");
    assert_eq!(
        cytiq(&["repair", "--input", text(&input)]).status.code(),
        Some(2)
    );
}

/// No other command reads around a deviation, whatever the plan says.
#[test]
fn other_commands_still_refuse_the_file() {
    let root = tempfile::tempdir().unwrap();
    let bytes = Spec {
        padding: b" ".to_vec(),
        ..Spec::default()
    }
    .build();
    assert_eq!(plan(root.path(), &bytes)["status"], json!("repairable"));
    let input = root.path().join("input.fcs");
    let output = root.path().join("events.parquet");
    for args in [
        vec!["meta", "--input", text(&input)],
        vec![
            "export",
            "events",
            "--input",
            text(&input),
            "--output",
            text(&output),
            "--layer",
            "raw",
        ],
    ] {
        assert_eq!(cytiq(&args).status.code(), Some(2));
    }
    assert!(!output.exists());
}
