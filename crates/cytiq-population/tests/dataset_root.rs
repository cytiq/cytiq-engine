//! Objects are read from inside the dataset root, never through a symbolic
//! link below it.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::{Built, Storage, build, build_storing};
use cytiq_population::dataset::{self, SourceObject};
use cytiq_population::events;
use cytiq_population::identity::ExecutionRef;
use cytiq_population::issue::{Issue, IssueCode, Issues, Severity};

fn errors(built: &Built, root: &Path) -> Vec<Issue> {
    let mut issues = Issues::new();
    dataset::verify_objects(&built.manifest, root, &mut issues);
    issues
        .into_items()
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect()
}

/// The only errors are refusals of a symbolic link, at `pointer`.
fn expect_link(built: &Built, pointer: &str) {
    let errors = errors(built, built.root.path());
    assert!(!errors.is_empty(), "the link was followed");
    for error in &errors {
        assert_eq!(error.code, IssueCode::InvalidReference, "{errors:?}");
        assert!(error.message.contains("symbolic link"), "{errors:?}");
    }
    assert!(
        errors.iter().any(|error| error.pointer == pointer),
        "{errors:?}"
    );
}

fn location(built: &Built, artifact: usize) -> PathBuf {
    built
        .root
        .path()
        .join(&built.manifest.artifacts[artifact].location)
}

/// Replace `path` by a symbolic link to itself moved to `target`.
fn relocate(path: &Path, target: &Path) {
    fs::rename(path, target).unwrap();
    symlink(target, path).unwrap();
}

#[test]
fn an_object_that_is_a_link_is_refused_even_with_the_recorded_bytes() {
    let outside = tempfile::tempdir().unwrap();
    for artifact in [0, 1, 4] {
        let built = build();
        relocate(
            &location(&built, artifact),
            &outside.path().join(format!("{artifact}.parquet")),
        );
        expect_link(&built, &format!("/artifacts/{artifact}/location"));
    }
}

#[test]
fn a_link_that_stays_inside_the_root_is_refused_too() {
    let built = build();
    let path = location(&built, 0);
    relocate(&path, &path.with_file_name("moved.parquet"));
    expect_link(&built, "/artifacts/0/location");
}

#[test]
fn a_directory_on_the_way_that_is_a_link_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    let built = build();
    relocate(&built.root.path().join("masks"), &outside.path().join("m"));
    expect_link(&built, "/artifacts/4/location");
}

#[test]
fn a_dangling_link_is_refused_as_a_link() {
    let built = build();
    let path = location(&built, 3);
    fs::remove_file(&path).unwrap();
    symlink("/nonexistent/table.parquet", &path).unwrap();
    expect_link(&built, "/artifacts/3/location");
}

#[test]
fn a_part_that_is_a_link_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    let built = build_storing(Storage::Parts);
    let part = location(&built, 0).with_file_name(events::part_name(1));
    relocate(&part, &outside.path().join("part.parquet"));
    expect_link(&built, "/artifacts/0/parts/1/location");
}

#[test]
fn a_source_and_a_provenance_record_are_objects_too() {
    let outside = tempfile::tempdir().unwrap();
    let mut built = build();
    let target = outside.path().join("raw.fcs");
    fs::write(&target, b"anything").unwrap();
    symlink(&target, built.root.path().join("raw.fcs")).unwrap();
    built.manifest.acquisitions[0].source = Some(SourceObject {
        location: "raw.fcs".into(),
        content: dataset::content_of(&target).unwrap(),
    });
    expect_link(&built, "/acquisitions/0/source/location");

    let mut built = build();
    let target = outside.path().join("record.json");
    fs::write(&target, b"{}").unwrap();
    symlink(&target, built.root.path().join("record.json")).unwrap();
    built.manifest.artifacts[0].execution = Some(ExecutionRef {
        provenance: Some("record.json".into()),
        fingerprint: None,
    });
    expect_link(&built, "/artifacts/0/execution");
}

#[test]
fn the_root_itself_may_be_a_link() {
    let outside = tempfile::tempdir().unwrap();
    for storage in [Storage::Table, Storage::Parts] {
        let built = build_storing(storage);
        let link = outside.path().join(format!("root-{storage:?}"));
        symlink(built.root.path(), &link).unwrap();
        assert_eq!(errors(&built, &link), Vec::new());
    }
}

#[test]
fn an_object_that_is_not_a_file_is_missing_and_never_waited_for() {
    let built = build();
    let path = location(&built, 0);
    fs::remove_file(&path).unwrap();
    // Opening a FIFO for reading waits for a writer unless it is refused.
    let status = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let found = errors(&built, built.root.path());
    assert_eq!(found[0].code, IssueCode::MissingObject, "{found:?}");

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let found = errors(&built, built.root.path());
    assert_eq!(found[0].code, IssueCode::MissingObject, "{found:?}");
}
