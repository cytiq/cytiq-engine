//! An output and its provenance record appear together or not at all: the
//! record of every command is broken in turn, and the process is killed
//! between two renames.
//!
//! Not covered: `meta` without `--output` and `transform --dry-run` write no
//! file; `population` and `specimen` publish their package by one rename.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use cytiq_provenance::RunRecord;
use cytiq_provenance::reuse::verify_outputs;
use cytiq_publish::OutputLock;

#[path = "../../cytiq-cytonorm/tests/support/mod.rs"]
mod cytonorm;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures")
        .join(name)
}

fn text(path: &Path) -> String {
    path.to_str().expect("path should be UTF-8").to_owned()
}

fn cytiq(args: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cytiq"));
    command.args(args).env_remove("CYTIQ_FAILPOINT");
    command
}

fn run(args: &[String], failpoint: Option<&str>) -> Output {
    let mut command = cytiq(args);
    if let Some(failpoint) = failpoint {
        command.env("CYTIQ_FAILPOINT", failpoint);
    }
    command.output().expect("command should run")
}

fn succeed(args: &[String]) {
    let output = run(args, None);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run a case's command, which must end with the case's exit code.
fn complete(case: &Case, args: &[String]) {
    let output = run(args, None);
    assert_eq!(
        output.status.code(),
        Some(case.exit),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every file and empty directory under `root`, with its bytes.
fn contents(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.insert(path.clone(), Vec::new());
                pending.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                found.insert(path, bytes);
            }
        }
    }
    found
}

/// Where a command's record goes, and how its outputs are published.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Record {
    /// Beside the output file.
    Beside,
    /// In an output directory whose files are published one by one.
    InDirectory,
    /// In an output directory published by one rename.
    WholeDirectory,
}

/// Where the output goes in the arguments of a command.
const OUT: &str = "{out}";

/// One command: how to run it, and where its outputs and record go.
struct Case {
    name: &'static str,
    /// File name of the output in its own empty directory.
    output: &'static str,
    /// The first target published: the output, or a file below it.
    first: &'static str,
    record: Record,
    args: Vec<String>,
    /// The exit code of a run that publishes: 3 when checks fail.
    exit: i32,
}

impl Case {
    fn new(name: &'static str, output: &'static str, record: Record, args: &[&str]) -> Self {
        Self {
            name,
            output,
            first: output,
            record,
            args: strings(args),
            exit: 0,
        }
    }

    /// A command that publishes `first` first, below its output directory.
    fn in_directory(
        name: &'static str,
        output: &'static str,
        first: &'static str,
        args: &[&str],
    ) -> Self {
        Self {
            first,
            ..Self::new(name, output, Record::InDirectory, args)
        }
    }

    fn args(&self, output: &Path) -> Vec<String> {
        self.args
            .iter()
            .map(|arg| arg.replace(OUT, &text(output)))
            .collect()
    }

    fn record(&self, output: &Path) -> PathBuf {
        if self.record == Record::Beside {
            cytiq_provenance::record_path_for_file(output)
        } else {
            cytiq_provenance::record_path_for_dir(output)
        }
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// `cytiq` in `work`, which must succeed.
fn prepare(args: &[&str]) {
    succeed(&strings(args));
}

/// Inputs several commands read, made once in `work`.
struct Inputs {
    raw: String,
    compensated: String,
    /// A stable acquisition `PeacoQC` keeps.
    peacoqc: String,
    /// Two separated populations, with CD3, CD4 and CD8.
    clusters: String,
    /// A `FlowSOM` model trained on `clusters`.
    flowsom_model: String,
    /// `cytonorm` samples, their manifest and a trained model.
    cytonorm: PathBuf,
    transformed: String,
    mask: String,
    compensate_record: String,
}

impl Inputs {
    fn new(work: &Path) -> Self {
        let raw = text(&fixture("flowcyt/Case12_P.fcs"));
        let compensated = work.join("case12.comp.fcs");
        let transformed = text(&work.join("case12.trans.fcs"));
        prepare(&[
            "compensate",
            "--input",
            &raw,
            "--output",
            &text(&compensated),
        ]);
        prepare(&[
            "transform",
            "--input",
            &text(&compensated),
            "--output",
            &transformed,
        ]);
        let cytonorm = work.join("cytonorm");
        fs::create_dir(&cytonorm).unwrap();
        cytonorm::design(&cytonorm, "controls");
        prepare(&[
            "cytonorm",
            "train",
            "--manifest",
            &text(&cytonorm.join("batches.json")),
            "--output",
            &text(&cytonorm.join("model")),
            "--threads",
            "1",
        ]);
        let peacoqc = work.join("stable.fcs");
        let events = 2_000_u16;
        cytiq_fcs::write_fcs3_1_f32(
            &peacoqc,
            &["Time", "FL1-A", "FL2-A"].map(cytiq_fcs::FcsWriteParameter::new),
            &[
                (0..events).map(f32::from).collect(),
                (0..events).map(|event| f32::from(event % 97)).collect(),
                (0..events).map(|event| f32::from(event * 7 % 89)).collect(),
            ],
        )
        .unwrap();
        let clusters = work.join("clusters.fcs");
        write_clusters(&clusters);
        let flowsom_model = text(&work.join("flowsom.json"));
        prepare(&[
            "flowsom",
            "train",
            "--input",
            &text(&clusters),
            "--output",
            &flowsom_model,
            "--channels",
            "CD3,CD4,CD8",
            "--threads",
            "1",
        ]);
        Self {
            compensated: text(&compensated),
            peacoqc: text(&peacoqc),
            clusters: text(&clusters),
            flowsom_model,
            cytonorm,
            mask: text(&compensated.with_extension("masks.parquet")),
            compensate_record: text(&cytiq_provenance::record_path_for_file(&compensated)),
            raw,
            transformed,
        }
    }
}

/// The commands, on inputs prepared in `work`.
fn cases(work: &Path) -> Vec<Case> {
    let inputs = Inputs::new(work);
    let mut cases = exports(&inputs);
    cases.push(repair(work, &inputs));
    cases.extend(reports(work, &inputs));
    cases.extend(fcs_stages(&inputs));
    cases.extend(matrices());
    cases.extend(models(&inputs));
    cases.extend(cohort_and_clean(work));
    cases
}

fn cohort_and_clean(work: &Path) -> Vec<Case> {
    let cohort = work.join("cohort");
    fs::create_dir(&cohort).unwrap();
    for (index, offset) in [0.0, 4.0, 8.0, 280.0].into_iter().enumerate() {
        write_cohort_sample(&cohort.join(format!("sample_{index:02}.fcs")), offset);
    }
    let clean = |input: &str| text(&fixture("clean").join(input));
    let clean_args = |input: &str, extra: &[&str]| -> Vec<String> {
        [
            &strings(&[
                "clean",
                "--input",
                &clean(input),
                "--output",
                OUT,
                "--threads",
                "2",
            ])[..],
            &strings(extra)[..],
        ]
        .concat()
    };
    let checks_failed = Case {
        name: "clean, --require unmet",
        args: clean_args("no_viability.fcs", &["--require", "singlets,viability"]),
        exit: 3,
        ..Case::in_directory("clean", "clean", "masks.parquet", &[])
    };
    vec![
        Case::in_directory(
            "cytoscan",
            "cytoscan",
            "cytoscan_summary.csv",
            &[
                "cytoscan",
                "--input",
                &text(&cohort),
                "--output",
                OUT,
                "--channels",
                "CD3,CD4,CD8",
                "--flag-method",
                "outlier",
                "--threads",
                "1",
            ],
        ),
        Case {
            args: clean_args("sample.fcs", &[]),
            ..Case::in_directory("clean", "clean", "masks.parquet", &[])
        },
        checks_failed,
    ]
}

/// 80 events whose markers are shifted by `offset` (`cytoscan_cli.rs`).
fn write_cohort_sample(path: &Path, offset: f32) {
    let mut columns = vec![Vec::new(); 6];
    for event_index in 0_u16..80 {
        let event = f32::from(event_index);
        columns[0].push(event);
        columns[1].push(20.0 + event * 0.2);
        columns[2].push(30.0 + event * 0.25);
        columns[3].push(100.0 + offset + event * 1.2);
        columns[4].push(160.0 + offset * 0.5 + event * 0.8);
        columns[5].push(90.0 + offset * 0.25 + event * 0.6);
    }
    let parameters = [
        cytiq_fcs::FcsWriteParameter::new("Time"),
        cytiq_fcs::FcsWriteParameter::new("FSC-A"),
        cytiq_fcs::FcsWriteParameter::new("SSC-A"),
        cytiq_fcs::FcsWriteParameter::with_label("FL1-A", "CD3"),
        cytiq_fcs::FcsWriteParameter::with_label("FL2-A", "CD4"),
        cytiq_fcs::FcsWriteParameter::with_label("FL3-A", "CD8"),
    ];
    let options = cytiq_fcs::FcsWriteOptions::new().with_text_keyword(
        "SPILL",
        "3,FL1-A,FL2-A,FL3-A,1,0.02,0.01,0.01,1,0.02,0.0,0.02,1",
    );
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options).unwrap();
}

/// 60 events in two populations (`flowsom_cli.rs`).
fn write_clusters(path: &Path) {
    let mut columns = vec![Vec::new(); 6];
    for raw_index in 0_u16..60 {
        let offset = if raw_index < 30 { 0.0 } else { 350.0 };
        let index = f32::from(raw_index);
        columns[0].push(index);
        columns[1].push(50.0 + index * 0.5);
        columns[2].push(70.0 + index * 0.25);
        columns[3].push(100.0 + offset + index * 1.5);
        columns[4].push(180.0 + offset * 0.5 + index);
        columns[5].push(80.0 + offset * 0.25 + index * 0.75);
    }
    let parameters = [
        cytiq_fcs::FcsWriteParameter::new("Time"),
        cytiq_fcs::FcsWriteParameter::new("FSC-A"),
        cytiq_fcs::FcsWriteParameter::new("SSC-A"),
        cytiq_fcs::FcsWriteParameter::with_label("FL1-A", "CD3"),
        cytiq_fcs::FcsWriteParameter::with_label("FL2-A", "CD4"),
        cytiq_fcs::FcsWriteParameter::with_label("FL3-A", "CD8"),
    ];
    cytiq_fcs::write_fcs3_1_f32(path, &parameters, &columns).unwrap();
}

fn matrices() -> Vec<Case> {
    let file = |name, output, args: &[&str]| Case::new(name, output, Record::Beside, args);
    let controls = |set: &str| {
        let directory = fixture(set);
        (text(&directory.join("controls.csv")), text(&directory))
    };
    let (spill_controls, spill_dir) = controls("autospill");
    let (spread_controls, spread_dir) = controls("autospread");
    let detectors = text(&fixture("autospread/detectors.csv"));
    vec![
        file(
            "autospill",
            "matrix.csv",
            &[
                "autospill",
                "--controls",
                &spill_controls,
                "--input-dir",
                &spill_dir,
                "--output",
                OUT,
            ],
        ),
        file(
            "autospread",
            "spread.csv",
            &[
                "autospread",
                "--controls",
                &spread_controls,
                "--input-dir",
                &spread_dir,
                "--detectors",
                &detectors,
                "--output",
                OUT,
            ],
        ),
    ]
}

fn models(inputs: &Inputs) -> Vec<Case> {
    let file = |name, output, args: &[&str]| Case::new(name, output, Record::Beside, args);
    let channels = ["--channels", "CD3,CD4,CD8", "--threads", "1"];
    vec![
        file(
            "flowsom",
            "clusters.flowsom.csv",
            &[
                &["flowsom", "--input", &inputs.clusters, "--output", OUT],
                &channels[..],
            ]
            .concat(),
        ),
        file(
            "flowsom train",
            "flowsom.json",
            &[
                &[
                    "flowsom",
                    "train",
                    "--input",
                    &inputs.clusters,
                    "--output",
                    OUT,
                ],
                &channels[..],
            ]
            .concat(),
        ),
        file(
            "flowsom apply",
            "clusters.apply.csv",
            &[
                "flowsom",
                "apply",
                "--input",
                &inputs.clusters,
                "--model",
                &inputs.flowsom_model,
                "--output",
                OUT,
            ],
        ),
        Case::in_directory(
            "classify",
            "predictions",
            "predictions.parquet",
            &[
                "classify",
                "--input",
                &text(&fixture("classify/raw.fcs")),
                "--model",
                &text(&fixture("classify/lightgbm")),
                "--output",
                OUT,
                "--threads",
                "1",
            ],
        ),
        Case::in_directory(
            "spillqc",
            "spillqc",
            "summary.json",
            &[
                "spillqc",
                "--input",
                &text(&fixture("parity/spillqc/tube_1.fcs")),
                "--output",
                OUT,
            ],
        ),
    ]
}

fn fcs_stages(inputs: &Inputs) -> Vec<Case> {
    let file = |name, output, args: &[&str]| Case::new(name, output, Record::Beside, args);
    let spectral = fixture("autospectral");
    let cytonorm = |name: &str| text(&inputs.cytonorm.join(name));
    vec![
        file(
            "compensate",
            "case12.comp.fcs",
            &["compensate", "--input", &inputs.raw, "--output", OUT],
        ),
        file(
            "transform",
            "case12.trans.fcs",
            &["transform", "--input", &inputs.compensated, "--output", OUT],
        ),
        file(
            "peacoqc",
            "case12.peacoqc.fcs",
            &[
                "peacoqc",
                "--input",
                &inputs.peacoqc,
                "--output",
                OUT,
                "--channels",
                "FL1-A,FL2-A",
            ],
        ),
        Case::in_directory(
            "autospectral",
            "unmixed",
            "fluorophore_spectra.csv",
            &[
                "autospectral",
                "--controls",
                &text(&spectral.join("controls.csv")),
                "--samples",
                &text(&spectral.join("samples.csv")),
                "--input-dir",
                &text(&spectral),
                "--output",
                OUT,
                "--threads",
                "1",
                "--quantile-gate",
            ],
        ),
        Case::new(
            "cytonorm train",
            "model",
            Record::WholeDirectory,
            &[
                "cytonorm",
                "train",
                "--manifest",
                &cytonorm("batches.json"),
                "--output",
                OUT,
                "--threads",
                "1",
            ],
        ),
        file(
            "cytonorm apply",
            "vb.norm.fcs",
            &[
                "cytonorm",
                "apply",
                "--input",
                &cytonorm("vb.fcs"),
                "--model",
                &cytonorm("model"),
                "--batch",
                "b",
                "--output",
                OUT,
            ],
        ),
    ]
}

fn exports(inputs: &Inputs) -> Vec<Case> {
    let manifest = text(&fixture("dataset/reference/manifest.json"));
    let events = text(&fixture("dataset/reference/events/M/S12-M.raw.parquet"));
    let export = [
        "export",
        "events",
        "--input",
        &inputs.raw,
        "--output",
        OUT,
        "--layer",
        "raw",
    ];
    vec![
        Case::new("export events", "events.parquet", Record::Beside, &export),
        Case::new(
            "export events, parts",
            "events",
            Record::WholeDirectory,
            &[&export[..], &["--max-file-bytes", "128B"]].concat(),
        ),
        Case::new(
            "export labels",
            "labels.parquet",
            Record::Beside,
            &[
                "export",
                "labels",
                "--kind",
                "qc",
                "--input",
                &inputs.mask,
                "--producer-provenance",
                &inputs.compensate_record,
                "--events",
                &events,
                "--output",
                OUT,
            ],
        ),
        Case::new(
            "dataset export",
            "product",
            Record::WholeDirectory,
            &[
                "dataset",
                "export",
                "--manifest",
                &manifest,
                "--output",
                OUT,
            ],
        ),
    ]
}

const RUN: &str = "\
schema_version: 3
cytometry: conventional
panels:
  - name: M
    signal_state: raw
    markers:
      - { marker: CD14, channel: FL1 INT }
samples:
  - { id: S12, file: case12.fcs, panel: M }
";

/// `cytiq repair` of Case12 with an ANALYSIS that ends at -1 in HEADER.
fn repair(work: &Path, inputs: &Inputs) -> Case {
    let mut bytes = fs::read(&inputs.raw).unwrap();
    bytes[42..58].copy_from_slice(b"       0      -1");
    let deviating = work.join("deviating.fcs");
    fs::write(&deviating, bytes).unwrap();
    Case::new(
        "repair",
        "repaired.fcs",
        Record::Beside,
        &[
            "repair",
            "--input",
            &text(&deviating),
            "--output",
            OUT,
            "--allow",
            "analysis_offsets_negative",
        ],
    )
}

fn reports(work: &Path, inputs: &Inputs) -> Vec<Case> {
    fs::copy(&inputs.raw, work.join("case12.fcs")).unwrap();
    let run = work.join("run.yaml");
    fs::write(&run, RUN).unwrap();
    let run = text(&run);
    let manifest = text(&fixture("dataset/reference/manifest.json"));
    let gates = text(&fixture("dataset/case12-gates.xml"));
    let file = |name, output, args: &[&str]| Case::new(name, output, Record::Beside, args);
    vec![
        file(
            "stats",
            "stats.json",
            &["stats", "--input", &inputs.raw, "--output", OUT],
        ),
        file(
            "meta",
            "meta.json",
            &["meta", "--input", &inputs.raw, "--output", OUT],
        ),
        file(
            "preflight",
            "preflight.json",
            &["preflight", "--manifest", &run, "--output", OUT],
        ),
        file(
            "qc",
            "qc.json",
            &[
                "qc",
                "--manifest",
                &run,
                "--sample",
                "S12",
                "--mask",
                &inputs.mask,
                "--output",
                OUT,
            ],
        ),
        file(
            "dataset validate",
            "report.json",
            &[
                "dataset",
                "validate",
                "--manifest",
                &manifest,
                "--output",
                OUT,
            ],
        ),
        file(
            "mask combine",
            "combined.parquet",
            &["mask", "combine", "--mask", &inputs.mask, "--output", OUT],
        ),
        file(
            "gate",
            "gates.csv",
            &[
                "gate",
                "--input",
                &inputs.transformed,
                "--gating-ml",
                &gates,
                "--output",
                OUT,
                "--membership",
                "{out}.membership.parquet",
            ],
        ),
        file(
            "plot",
            "plot.svg",
            &[
                "plot",
                "--input",
                &inputs.raw,
                "--x",
                "FS INT",
                "--y",
                "SS INT",
                "--output",
                OUT,
            ],
        ),
    ]
}

/// Run a failing command and require that nothing changed in its directory.
fn expect_nothing(case: &Case, directory: &Path, args: &[String], failpoint: Option<&str>) {
    let before = contents(directory);
    let output = run(args, failpoint);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(case.exit),
        "{}: {failpoint:?} did not fail the run",
        case.name
    );
    let after = contents(directory);
    let names = |map: &BTreeMap<PathBuf, Vec<u8>>| map.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        names(&after),
        names(&before),
        "{} {failpoint:?}: {stderr}",
        case.name
    );
    assert!(after == before, "{} {failpoint:?}: {stderr}", case.name);
}

#[test]
fn a_record_that_cannot_be_written_takes_the_output_with_it() {
    let work = tempfile::tempdir().unwrap();
    for case in cases(work.path()) {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(case.output);
        let record = case.record(&output);
        let args = case.args(&output);
        let overwrite = [args.clone(), strings(&["--overwrite"])].concat();
        let suffix = record.file_name().unwrap().to_str().unwrap();
        let full = format!("stage|{suffix}|full");

        // Nothing there before: nothing after.
        expect_nothing(&case, directory.path(), &args, Some(&full));
        complete(&case, &args);
        let first: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert!(verify_outputs(&first), "{}", case.name);

        // A previous output and its record: both stay.
        expect_nothing(&case, directory.path(), &overwrite, Some(&full));
        let published = format!("published|{}|full", case.first);
        expect_nothing(&case, directory.path(), &overwrite, Some(&published));
        if case.record != Record::WholeDirectory {
            // A directory where the record goes.
            fs::remove_file(&record).unwrap();
            fs::create_dir(&record).unwrap();
            expect_nothing(&case, directory.path(), &overwrite, None);
            fs::remove_dir(&record).unwrap();
        }
        complete(&case, &overwrite);
        let again: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert!(verify_outputs(&again), "{}", case.name);
    }
}

#[cfg(unix)]
#[test]
fn a_directory_that_cannot_be_written_keeps_what_it_holds() {
    use std::os::unix::fs::PermissionsExt as _;
    let work = tempfile::tempdir().unwrap();
    for case in cases(work.path()) {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(case.output);
        let args = case.args(&output);
        complete(&case, &args);
        let overwrite = [args, strings(&["--overwrite"])].concat();
        // Where the record is staged: the output directory, or its parent.
        let locked = if case.record == Record::InDirectory {
            output
        } else {
            directory.path().to_path_buf()
        };
        let mode = |mode| fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();
        mode(0o555);
        expect_nothing(&case, directory.path(), &overwrite, None);
        mode(0o755);
    }
}

/// Run until the process waits at `failpoint`, then kill it.
fn kill_at(args: &[String], failpoint: &str, marker: &Path) {
    let mut child = cytiq(args)
        .env("CYTIQ_FAILPOINT", failpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(120) && child.try_wait().unwrap().is_none(),
            "{args:?} never reached `{failpoint}`"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // SIGKILL on Unix: nothing of the process runs after it.
    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_file(marker).unwrap();
}

/// The recorded outputs and the record, with nothing else and nothing missing.
fn expect_complete(case: &Case, directory: &Path, output: &Path) {
    let record = case.record(output);
    let found: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert!(verify_outputs(&found), "{}", case.name);
    let files: Vec<PathBuf> = contents(directory)
        .into_keys()
        .filter(|path| path.is_file())
        .collect();
    let mut expected: Vec<PathBuf> = found
        .outputs
        .iter()
        .map(|output| PathBuf::from(&output.path))
        .chain([record])
        .collect();
    expected.sort();
    assert_eq!(files, expected, "{}", case.name);
}

/// Killed between the rename of an output and that of its record.
#[test]
fn a_kill_between_two_renames_is_completed_by_the_next_run() {
    let work = tempfile::tempdir().unwrap();
    let marker = work.path().join("marker");
    for case in cases(work.path())
        .into_iter()
        .filter(|case| case.record != Record::WholeDirectory)
    {
        for previous in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join(case.output);
            let record = case.record(&output);
            let mut args = case.args(&output);
            let mut before = None;
            if previous {
                complete(&case, &args);
                before = Some(fs::read(&record).unwrap());
                args.push("--overwrite".into());
            }
            let failpoint = format!("published|{}|pause:{}", case.first, text(&marker));
            kill_at(&args, &failpoint, &marker);
            assert_eq!(
                fs::read(&record).ok(),
                before,
                "{}: not its record",
                case.name
            );

            // Completed: the outputs with the record they were staged with.
            complete(&case, &[strings(&["--reuse", "skip"]), args].concat());
            expect_complete(&case, directory.path(), &output);
            assert_ne!(fs::read(&record).ok(), before, "{}: its record", case.name);
            assert!(OutputLock::acquire(&output).is_ok(), "the lock is free");
        }
    }
}

/// Killed after everything is staged and before the first rename.
#[test]
fn a_kill_before_the_first_rename_leaves_the_previous_output() {
    let work = tempfile::tempdir().unwrap();
    let marker = work.path().join("marker");
    for case in cases(work.path()) {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(case.output);
        let record = case.record(&output);
        let args = case.args(&output);
        complete(&case, &args);
        let before = contents(directory.path());
        let overwrite = [args, strings(&["--overwrite"])].concat();
        let suffix = record.file_name().unwrap().to_str().unwrap();
        let failpoint = format!("stage|{suffix}|pause:{}", text(&marker));
        kill_at(&overwrite, &failpoint, &marker);
        for (path, bytes) in &before {
            if path.is_file() {
                assert!(&fs::read(path).unwrap() == bytes, "{}", case.name);
            }
        }
        complete(&case, &overwrite);
        expect_complete(&case, directory.path(), &output);
    }
}
