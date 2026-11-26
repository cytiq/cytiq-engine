//! `cytiq repair`: name the deviations of a file, and repair what is allowed.

mod common;

#[path = "../../cytiq-repair/tests/common/mod.rs"]
mod files;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use cytiq_fcs::{FcsReadLimits, FcsReader};
use cytiq_population::dataset::{self, AcquisitionRole};
use cytiq_population::events::{self, EventsSummary, LayerState};
use cytiq_population::issue::Issues;
use cytiq_population::privacy::PrivacyPolicy;
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

/// A file with three deviations, and the arguments that allow them.
const ALLOWED: [&str; 6] = [
    "--allow",
    "text_trailing_padding",
    "--allow",
    "text_empty_values",
    "--allow",
    "data_end_exclusive",
];

fn deviating() -> Spec {
    Spec {
        keywords: vec![
            ("$CYT", "Model//X"),
            ("VENDOR", "kept"),
            ("$SPILLOVER", "2,FSC-A,FL1-A,1,0.1,0.05,1"),
        ],
        raw_text: b"$INST//$EXP/Person Name/".to_vec(),
        padding: b"  \0".to_vec(),
        data_end_past: 1,
        ..Spec::default()
    }
}

fn repair(input: &Path, output: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["repair", "--input", text(input), "--output", text(output)];
    args.extend_from_slice(extra);
    cytiq(&args)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn digest(path: &Path) -> String {
    cytiq_provenance::hash_file(path).unwrap().1
}

fn verified(path: &Path) -> EventsSummary {
    let mut issues = Issues::new();
    events::verify_file(path, "", &mut issues).unwrap_or_else(|| panic!("{:?}", issues.items()))
}

#[test]
fn nothing_is_repaired_that_was_not_allowed() {
    let root = tempfile::tempdir().unwrap();
    let (input, output) = (root.path().join("input.fcs"), root.path().join("out.fcs"));
    fs::write(&input, deviating().build()).unwrap();
    for allow in [&ALLOWED[..0], &ALLOWED[..2], &ALLOWED[2..]] {
        let refused = repair(&input, &output, allow);
        assert_eq!(refused.status.code(), Some(2), "{allow:?}");
        assert!(stderr(&refused).contains("--allow"), "{}", stderr(&refused));
    }
    // A code that does not exist is a usage error.
    assert_eq!(
        repair(&input, &output, &["--allow", "everything"])
            .status
            .code(),
        Some(2)
    );
    let names: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
    assert_eq!(names.len(), 1, "nothing was written");
}

#[test]
fn a_repair_writes_the_file_its_report_and_its_record() {
    let root = tempfile::tempdir().unwrap();
    let (input, output) = (root.path().join("input.fcs"), root.path().join("out.fcs"));
    let bytes = deviating().build();
    fs::write(&input, &bytes).unwrap();
    let repaired = repair(&input, &output, &ALLOWED);
    assert_eq!(repaired.status.code(), Some(0), "{}", stderr(&repaired));
    assert_eq!(fs::read(&input).unwrap(), bytes, "the input is untouched");
    let report_path = root.path().join("out.fcs.repair.json");
    common::assert_provenance(
        &cytiq_provenance::record_path_for_file(&output),
        "cytiq repair",
        &[&input],
        &[&output, &report_path],
    );

    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["contract"], "cytiq.repair.report.v1");
    assert_eq!(report["acquisition_id"], digest(&input));
    assert_eq!(report["output_sha256"], digest(&output));
    let data = files::data();
    assert_eq!(
        report["data"]["sha256"],
        common::sha256_hex(&{
            let path = root.path().join("data");
            fs::write(&path, &data).unwrap();
            path
        })
    );
    let keywords: Vec<&Value> = report["dropped"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry.get("keywords"))
        .collect();
    assert_eq!(keywords, [&json!(["$INST", "$EXP"])]);
    let document = report.to_string();
    assert!(!document.contains("Person") && !document.contains(text(root.path())));

    let dataset = FcsReader::open(&output).unwrap().read().unwrap();
    let keywords = &dataset.text;
    assert_eq!(
        keywords.get("CYTIQ_ACQUISITION"),
        Some(digest(&input).as_str())
    );
    assert_eq!(keywords.get("VENDOR"), Some("kept"));
    assert_eq!(keywords.get("$INST"), None);
    let record: Value = serde_json::from_str(keywords.get("CYTIQ_REPAIR").unwrap()).unwrap();
    assert_eq!(
        record["codes"],
        json!([
            "text_trailing_padding",
            "text_empty_values",
            "data_end_exclusive"
        ])
    );
    let written = fs::read(&output).unwrap();
    assert!(written.windows(data.len()).any(|window| window == data));

    // Idempotent: what was written needs nothing.
    let again = repair(&output, &root.path().join("again.fcs"), &ALLOWED);
    assert_eq!(again.status.code(), Some(2));
    assert!(stderr(&again).contains("conforms"), "{}", stderr(&again));
    // Deterministic, and never in place or over another file.
    assert_eq!(repair(&input, &output, &ALLOWED).status.code(), Some(4));
    let overwrite = [&ALLOWED[..], &["--overwrite"]].concat();
    assert_eq!(repair(&input, &output, &overwrite).status.code(), Some(0));
    assert_eq!(fs::read(&output).unwrap(), written);
    assert_eq!(repair(&input, &input, &overwrite).status.code(), Some(2));
    assert_eq!(fs::read(&input).unwrap(), bytes);
    let reused = cytiq(
        &[
            &["--reuse", "skip", "repair", "--input", text(&input)][..],
            &["--output", text(&output)],
            &ALLOWED,
        ]
        .concat(),
    );
    assert_eq!(reused.status.code(), Some(0), "{}", stderr(&reused));
}

#[test]
fn refused_and_conforming_files_are_not_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let (input, output) = (root.path().join("input.fcs"), root.path().join("out.fcs"));
    let all = [
        "text_trailing_padding",
        "text_overlaps_data",
        "undeclared_bytes",
    ];
    let allow: Vec<&str> = all.iter().flat_map(|code| ["--allow", *code]).collect();
    for (spec, message) in [
        (Spec::default(), "conforms"),
        (
            Spec {
                header: [None, Some((400, 423)), None],
                ..Spec::default()
            },
            "ambiguous_data_offsets",
        ),
    ] {
        fs::write(&input, spec.build()).unwrap();
        let refused = repair(&input, &output, &allow);
        assert_eq!(refused.status.code(), Some(2));
        assert!(stderr(&refused).contains(message), "{}", stderr(&refused));
        assert!(!output.exists());
    }
    let absent = root.path().join("absent.fcs");
    assert_eq!(repair(&absent, &output, &allow).status.code(), Some(4));
    let text_output = root.path().join("out.txt");
    assert_eq!(repair(&input, &text_output, &allow).status.code(), Some(2));
}

/// The pipeline takes a repaired file for the raw acquisition it is.
#[test]
fn a_repaired_file_is_raw_to_every_stage() {
    let root = tempfile::tempdir().unwrap();
    let (input, output) = (root.path().join("input.fcs"), root.path().join("out.fcs"));
    fs::write(&input, deviating().build()).unwrap();
    assert_eq!(repair(&input, &output, &ALLOWED).status.code(), Some(0));
    let acquisition = digest(&input);
    let table = root.path().join("raw.parquet");
    let exported = cytiq(&[
        "export",
        "events",
        "--input",
        text(&output),
        "--output",
        text(&table),
        "--layer",
        "raw",
    ]);
    assert_eq!(exported.status.code(), Some(0), "{}", stderr(&exported));
    let summary = verified(&table);
    assert_eq!(summary.descriptor.state, LayerState::Raw);
    assert_eq!(summary.descriptor.acquisition_id, acquisition);
    assert_eq!(summary.source_sha256, digest(&output));
    assert_eq!(summary.raw_events, Some(3));

    // The values are those of the same file written without deviations.
    let conforming = root.path().join("conforming.fcs");
    fs::write(&conforming, Spec::default().build()).unwrap();
    let reference = root.path().join("reference.parquet");
    cytiq(&[
        "export",
        "events",
        "--input",
        text(&conforming),
        "--output",
        text(&reference),
        "--layer",
        "raw",
    ]);
    let expected = verified(&reference);
    assert_eq!(summary.channels.len(), expected.channels.len());
    let values = |path: &Path| {
        let file = fs::File::open(path).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        reader
            .map(|batch| batch.unwrap().columns()[3..].to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(values(&table), values(&reference));

    // Derived files keep the acquisition, and are no acquisition themselves.
    let compensated = root.path().join("out.comp.fcs");
    let done = cytiq(&[
        "compensate",
        "--input",
        text(&output),
        "--output",
        text(&compensated),
    ]);
    assert_eq!(done.status.code(), Some(0), "{}", stderr(&done));
    let mask =
        cytiq_mask::EventMask::read_parquet(&compensated.with_extension("masks.parquet")).unwrap();
    assert_eq!(mask.acquisition, acquisition);
    let entry = |path: &Path| {
        let metadata = FcsReader::open(path)
            .unwrap()
            .stream(std::num::NonZeroUsize::MIN, FcsReadLimits::default())
            .unwrap()
            .metadata;
        dataset::acquisition_entry(
            path,
            &metadata,
            "T",
            AcquisitionRole::Sample,
            &PrivacyPolicy::deidentified(),
        )
    };
    let described = entry(&output).unwrap();
    assert_eq!(described.id, acquisition);
    assert_eq!(described.raw_events, 3);
    assert!(!described.keywords.contains_key("CYTIQ_REPAIR"));
    assert!(entry(&compensated).is_err(), "a derived file is refused");
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
