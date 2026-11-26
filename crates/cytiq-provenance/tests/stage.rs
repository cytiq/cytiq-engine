//! The record of a run is staged with its outputs, and an interrupted
//! publication of the set is completed.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cytiq_provenance::outcome::OutcomeReason;
use cytiq_provenance::reuse::{Destination, verify_outputs};
use cytiq_provenance::{
    ProvenanceError, RunRecord, RunRecorder, record_path_for_dir, record_path_for_file, recovery,
};
use cytiq_publish::{Existing, OutputLock, Publication, PublishError};

fn recorder(input: &Path) -> RunRecorder {
    let mut recorder = RunRecorder::start("cytiq test", "1.0.0", vec![]);
    recorder
        .snapshot_inputs("input", &[input.to_path_buf()])
        .unwrap();
    recorder
}

fn stage(publication: &mut Publication, target: &Path, text: &str) {
    publication
        .stage_file(target, |writer| {
            writer
                .write_all(text.as_bytes())
                .map_err(|source| PublishError::io(target, source))
        })
        .unwrap();
}

fn read(path: &Path) -> RunRecord {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A root with an input, the outputs of a run and its record path.
struct Run {
    root: tempfile::TempDir,
    input: PathBuf,
    table: PathBuf,
    mask: PathBuf,
    record: PathBuf,
}

impl Run {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        fs::write(&input, "input").unwrap();
        let table = root.path().join("out.parquet");
        Self {
            mask: root.path().join("out.masks"),
            record: record_path_for_file(&table),
            root,
            input,
            table,
        }
    }

    fn outputs(&self) -> [PathBuf; 2] {
        [self.table.clone(), self.mask.clone()]
    }

    /// Stage both outputs with `text` and the record.
    fn staged(&self, existing: Existing, text: &str) -> Publication {
        let mut publication = Publication::new(existing);
        stage(&mut publication, &self.table, text);
        stage(&mut publication, &self.mask, &format!("{text} mask"));
        recorder(&self.input)
            .stage(
                &mut publication,
                &self.outputs(),
                &self.record,
                OutcomeReason::Success,
            )
            .unwrap();
        publication
    }

    fn recover(&self) {
        let lock = OutputLock::acquire(&self.table).unwrap();
        recovery::recover(&lock, &Destination::File(self.table.clone())).unwrap();
    }
}

#[test]
fn the_record_is_published_with_the_outputs_it_hashes() {
    let run = Run::new();
    fs::write(&run.record, "stale").unwrap();
    let publication = run.staged(Existing::Refuse, "new");
    assert!(!run.table.exists(), "nothing is published yet");
    assert_eq!(fs::read_to_string(&run.record).unwrap(), "stale");
    publication.commit().unwrap();
    let record = read(&run.record);
    assert!(verify_outputs(&record));
    let paths: Vec<&str> = record.outputs.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(
        paths,
        [run.table.to_str().unwrap(), run.mask.to_str().unwrap()]
    );
    assert_eq!(record.reason, Some(OutcomeReason::Success));
}

#[test]
fn a_directory_published_whole_holds_its_record() {
    let run = Run::new();
    let output = run.root.path().join("parts");
    let mut publication = Publication::new(Existing::Refuse);
    let staged = publication.stage_dir(&output).unwrap();
    fs::write(staged.join("part-0"), "values").unwrap();
    let record_path = record_path_for_dir(&output);
    recorder(&run.input)
        .stage(
            &mut publication,
            &[output.join("part-0")],
            &record_path,
            OutcomeReason::Success,
        )
        .unwrap();
    assert!(!output.exists());
    publication.commit().unwrap();
    assert_eq!(listing(&output), ["part-0", "provenance.json"]);
    assert!(verify_outputs(&read(&record_path)));
}

#[test]
fn a_changed_input_or_an_unstaged_output_publishes_nothing() {
    let run = Run::new();
    let changed = recorder(&run.input);
    fs::write(&run.input, "another").unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    stage(&mut publication, &run.table, "new");
    let error = changed
        .stage(
            &mut publication,
            std::slice::from_ref(&run.table),
            &run.record,
            OutcomeReason::Success,
        )
        .unwrap_err();
    assert!(matches!(error, ProvenanceError::InputChanged { .. }));
    let error = recorder(&run.input)
        .stage(
            &mut publication,
            &run.outputs(),
            &run.record,
            OutcomeReason::Success,
        )
        .unwrap_err();
    assert!(matches!(error, ProvenanceError::NotStaged { path } if path == run.mask));
    drop(publication);
    assert_eq!(listing(run.root.path()), ["input"]);
}

/// What a commit killed after `published` renames leaves behind: those targets
/// new, with a backup of what they replaced, and the rest still staged.
fn crash(run: &Run, text: &str, published: usize) {
    let publication = run.staged(Existing::Replace, text);
    let staged = publication.staged().unwrap();
    assert_eq!(staged.len(), 3);
    // Aside, because dropping the publication removes its own files.
    let aside = tempfile::tempdir().unwrap();
    for (_, file) in &staged {
        fs::copy(file, aside.path().join(file.file_name().unwrap())).unwrap();
    }
    drop(publication);
    for (index, (target, file)) in staged.iter().enumerate() {
        let name = file.file_name().unwrap();
        let kept = aside.path().join(name);
        if index >= published {
            fs::copy(kept, run.root.path().join(name)).unwrap();
            continue;
        }
        if target.exists() {
            let backup = name.to_str().unwrap().replace(".tmp", ".old.tmp");
            fs::rename(target, run.root.path().join(backup)).unwrap();
        }
        fs::copy(kept, target).unwrap();
    }
}

fn temporary(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e == "tmp")
}

const COMPLETE: [&str; 4] = [
    "input",
    "out.masks",
    "out.parquet",
    "out.parquet.provenance.json",
];

#[test]
fn a_crash_between_two_renames_is_completed() {
    for previous in [false, true] {
        for published in [1, 2] {
            let run = Run::new();
            if previous {
                run.staged(Existing::Refuse, "previous").commit().unwrap();
            }
            crash(&run, "new", published);
            assert!(listing(run.root.path()).len() > COMPLETE.len() - 1);
            run.recover();
            assert_eq!(listing(run.root.path()), COMPLETE, "{previous} {published}");
            assert_eq!(fs::read_to_string(&run.table).unwrap(), "new");
            assert_eq!(fs::read_to_string(&run.mask).unwrap(), "new mask");
            assert!(verify_outputs(&read(&run.record)));
        }
    }
}

#[test]
fn a_crash_before_the_first_rename_keeps_the_previous_set() {
    for previous in [false, true] {
        let run = Run::new();
        if previous {
            run.staged(Existing::Refuse, "previous").commit().unwrap();
        }
        crash(&run, "new", 0);
        run.recover();
        if previous {
            assert!(verify_outputs(&read(&run.record)));
            assert_eq!(fs::read_to_string(&run.table).unwrap(), "previous");
        } else {
            assert!(!run.table.exists() && !run.record.exists());
        }
        // Staging of a file another command may own is not discarded.
        let rest: Vec<String> = listing(run.root.path())
            .into_iter()
            .filter(|name| !name.starts_with(".out.masks."))
            .collect();
        assert!(!rest.iter().any(|name| temporary(name)), "{rest:?}");
    }
}

#[test]
fn a_set_that_cannot_be_completed_is_left_for_its_hashes_to_tell() {
    let run = Run::new();
    run.staged(Existing::Refuse, "previous").commit().unwrap();
    crash(&run, "new", 1);
    // The staged mask is damaged: the set is not what the record says.
    let staged = listing(run.root.path())
        .into_iter()
        .find(|name| name.starts_with(".out.masks."))
        .unwrap();
    fs::write(run.root.path().join(staged), "torn").unwrap();
    run.recover();
    assert_eq!(fs::read_to_string(&run.table).unwrap(), "new");
    assert_eq!(fs::read_to_string(&run.mask).unwrap(), "previous mask");
    assert!(!verify_outputs(&read(&run.record)), "the hashes tell");
}

#[test]
fn a_torn_or_foreign_staged_record_is_never_published() {
    let run = Run::new();
    run.staged(Existing::Refuse, "previous").commit().unwrap();
    let before = fs::read(&run.record).unwrap();
    for text in ["{", "{}", r#"{"schema_version":3}"#] {
        fs::write(
            run.root
                .path()
                .join(".out.parquet.provenance.json.abc123.tmp"),
            text,
        )
        .unwrap();
        run.recover();
        assert_eq!(fs::read(&run.record).unwrap(), before);
        assert_eq!(listing(run.root.path()), COMPLETE);
    }
}

#[test]
fn recovery_writes_nothing_where_nothing_was_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let absent = root.path().join("parts");
    let lock = OutputLock::acquire(&absent).unwrap();
    recovery::recover(&lock, &Destination::Dir(absent.clone())).unwrap();
    assert!(!absent.exists(), "an absent output stays absent");
}
