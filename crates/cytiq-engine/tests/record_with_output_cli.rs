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

/// Arguments of a command for an output at the given path.
type Arguments = Box<dyn Fn(&Path) -> Vec<String>>;

/// One command: how to run it, and where its output and record go.
struct Case {
    name: &'static str,
    args: Arguments,
    /// File name of the output in its own empty directory.
    output: &'static str,
    /// The record is inside the output directory, which is replaced whole.
    whole_directory: bool,
}

impl Case {
    fn record(&self, output: &Path) -> PathBuf {
        if self.whole_directory {
            cytiq_provenance::record_path_for_dir(output)
        } else {
            cytiq_provenance::record_path_for_file(output)
        }
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The commands, on inputs prepared in `work`.
fn cases(work: &Path) -> Vec<Case> {
    let fcs = text(&fixture("flowcyt/Case12_P.fcs"));
    let manifest = text(&fixture("dataset/reference/manifest.json"));
    let reference = fixture("dataset/reference");
    let compensated = work.join("case12.comp.fcs");
    succeed(&strings(&[
        "compensate",
        "--input",
        &fcs,
        "--output",
        &text(&compensated),
    ]));
    let mask = text(&compensated.with_extension("masks.parquet"));
    let producer = text(&cytiq_provenance::record_path_for_file(&compensated));
    let events = text(&reference.join("events/M/S12-M.raw.parquet"));
    let table = fcs.clone();
    let parts = fcs;
    vec![
        Case {
            name: "export events",
            args: Box::new(move |output| {
                let output = text(output);
                strings(&[
                    "export", "events", "--input", &table, "--output", &output, "--layer", "raw",
                ])
            }),
            output: "events.parquet",
            whole_directory: false,
        },
        Case {
            name: "export events, parts",
            args: Box::new(move |output| {
                let output = text(output);
                strings(&[
                    "export",
                    "events",
                    "--input",
                    &parts,
                    "--output",
                    &output,
                    "--layer",
                    "raw",
                    "--max-file-bytes",
                    "128B",
                ])
            }),
            output: "events",
            whole_directory: true,
        },
        Case {
            name: "export labels",
            args: Box::new(move |output| {
                strings(&[
                    "export",
                    "labels",
                    "--kind",
                    "qc",
                    "--input",
                    &mask,
                    "--producer-provenance",
                    &producer,
                    "--events",
                    &events,
                    "--output",
                    &text(output),
                ])
            }),
            output: "labels.parquet",
            whole_directory: false,
        },
        Case {
            name: "dataset export",
            args: Box::new(move |output| {
                strings(&[
                    "dataset",
                    "export",
                    "--manifest",
                    &manifest,
                    "--output",
                    &text(output),
                ])
            }),
            output: "product",
            whole_directory: true,
        },
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
        let args = (case.args)(&output);
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
        let last = format!("published|{}|full", case.output);
        expect_nothing(&case, directory.path(), &overwrite, Some(&last));
        if !case.whole_directory {
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
        let args = (case.args)(&output);
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
            start.elapsed() < Duration::from_secs(60) && child.try_wait().unwrap().is_none(),
            "the run never reached `{failpoint}`"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // SIGKILL on Unix: nothing of the process runs after it.
    child.kill().unwrap();
    child.wait().unwrap();
    fs::remove_file(marker).unwrap();
}

/// Killed between the rename of an output and that of its record.
#[test]
fn a_kill_between_two_renames_is_completed_by_the_next_run() {
    let work = tempfile::tempdir().unwrap();
    let marker = work.path().join("marker");
    for case in cases(work.path())
        .into_iter()
        .filter(|case| !case.whole_directory)
    {
        for previous in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join(case.output);
            let record = case.record(&output);
            let mut args = (case.args)(&output);
            if previous {
                // An older output with its own record, both replaced.
                fs::write(&output, "older").unwrap();
                fs::write(&record, "{}").unwrap();
                args.push("--overwrite".into());
            }
            let failpoint = format!("published|{}|pause:{}", case.output, text(&marker));
            kill_at(&args, &failpoint, &marker);
            let torn = fs::read(&output).unwrap();
            assert_ne!(torn, b"older", "{}: the output was renamed", case.name);
            assert_eq!(record.exists(), previous, "{}: not its record", case.name);

            // Completed: the same output, with the record it was staged with.
            let reused = [strings(&["--reuse", "skip"]), args.clone()].concat();
            succeed(&reused);
            assert_eq!(fs::read(&output).unwrap(), torn, "{}", case.name);
            let found: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
            assert!(verify_outputs(&found), "{}", case.name);
            let names: Vec<PathBuf> = contents(directory.path()).into_keys().collect();
            let mut expected: Vec<PathBuf> = found
                .outputs
                .iter()
                .map(|output| PathBuf::from(&output.path))
                .chain([record.clone()])
                .collect();
            expected.sort();
            assert_eq!(names, expected, "{}", case.name);
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
        let args = (case.args)(&output);
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
        let found: RunRecord = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        assert!(verify_outputs(&found), "{}", case.name);
        assert!(
            !contents(directory.path())
                .keys()
                .any(|path| path.extension().is_some_and(|e| e == "tmp")),
            "{}: abandoned staging remains",
            case.name
        );
    }
}
