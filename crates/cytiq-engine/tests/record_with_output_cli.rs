//! An output and its provenance record appear together or not at all: the
//! record of every command is broken in turn, and the process is killed
//! between two renames.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use cytiq_provenance::RunRecord;
use cytiq_provenance::reuse::verify_outputs;
use cytiq_publish::OutputLock;

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
    record: Record,
    args: Vec<String>,
}

impl Case {
    fn new(name: &'static str, output: &'static str, record: Record, args: &[&str]) -> Self {
        Self {
            name,
            output,
            record,
            args: strings(args),
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
        Self {
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
    cases.extend(reports(work, &inputs));
    cases
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
    assert!(
        !output.status.success(),
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
        succeed(&args);
        let first: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert!(verify_outputs(&first), "{}", case.name);

        // A previous output and its record: both stay.
        expect_nothing(&case, directory.path(), &overwrite, Some(&full));
        let published = format!("published|{}|full", case.output);
        expect_nothing(&case, directory.path(), &overwrite, Some(&published));
        if case.record != Record::WholeDirectory {
            // A directory where the record goes.
            fs::remove_file(&record).unwrap();
            fs::create_dir(&record).unwrap();
            expect_nothing(&case, directory.path(), &overwrite, None);
            fs::remove_dir(&record).unwrap();
        }
        succeed(&overwrite);
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
        succeed(&args);
        let overwrite = [args, strings(&["--overwrite"])].concat();
        let mode = |mode| {
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(mode)).unwrap();
        };
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
                succeed(&args);
                before = Some(fs::read(&record).unwrap());
                args.push("--overwrite".into());
            }
            let failpoint = format!("published|{}|pause:{}", case.output, text(&marker));
            kill_at(&args, &failpoint, &marker);
            assert_eq!(
                fs::read(&record).ok(),
                before,
                "{}: not its record",
                case.name
            );

            // Completed: the outputs with the record they were staged with.
            succeed(&[strings(&["--reuse", "skip"]), args].concat());
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
        succeed(&args);
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
        succeed(&overwrite);
        expect_complete(&case, directory.path(), &output);
    }
}
