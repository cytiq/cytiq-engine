//! Failure injection without hooks in the code under test: errors come from
//! the caller's writer or from targets that cannot be replaced.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cytiq_publish::{Existing, Publication, PublishError, check_output_dir, write_file};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Publish(#[from] PublishError),
    #[error("writer failed")]
    Writer,
}

fn write_text(publication: &mut Publication, target: &Path, text: &str) -> Result<(), TestError> {
    publication.stage_file(target, |writer| {
        writer
            .write_all(text.as_bytes())
            .map_err(|source| PublishError::io(target, source).into())
    })
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn publishes_every_staged_file_on_commit() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &root.path().join("data.csv"), "a,b\n").unwrap();
    write_text(&mut publication, &root.path().join("provenance.json"), "{}").unwrap();
    assert!(
        listing(root.path())
            .iter()
            .all(|name| name.starts_with('.') && name.contains(".tmp"))
    );
    publication.commit().unwrap();
    assert_eq!(listing(root.path()), ["data.csv", "provenance.json"]);
    assert_eq!(
        fs::read_to_string(root.path().join("data.csv")).unwrap(),
        "a,b\n"
    );
}

#[test]
fn a_failing_writer_publishes_nothing_and_leaves_no_temporaries() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &root.path().join("first.csv"), "1").unwrap();
    let error = publication
        .stage_file(root.path().join("second.csv"), |writer| {
            writer.write_all(b"partial").unwrap();
            Err::<(), _>(TestError::Writer)
        })
        .unwrap_err();
    assert!(matches!(error, TestError::Writer));
    drop(publication);
    assert!(
        listing(root.path()).is_empty(),
        "{:?}",
        listing(root.path())
    );
}

#[test]
fn refused_targets_fail_before_the_writer_runs() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("out.csv");
    fs::write(&target, "previous").unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    assert!(matches!(
        publication.check(&target),
        Err(PublishError::Exists { .. })
    ));
    let mut called = false;
    let error = publication
        .stage_file(&target, |_| {
            called = true;
            Ok::<(), TestError>(())
        })
        .unwrap_err();
    assert!(matches!(
        error,
        TestError::Publish(PublishError::Exists { .. })
    ));
    assert!(!called);
    assert_eq!(fs::read_to_string(&target).unwrap(), "previous");
}

#[test]
fn a_target_that_appears_after_staging_fails_the_commit_before_publishing() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &root.path().join("a.csv"), "a").unwrap();
    write_text(&mut publication, &root.path().join("b.csv"), "b").unwrap();
    fs::write(root.path().join("b.csv"), "someone else").unwrap();
    assert!(matches!(
        publication.commit(),
        Err(PublishError::Exists { .. })
    ));
    assert_eq!(listing(root.path()), ["b.csv"]);
    assert_eq!(
        fs::read_to_string(root.path().join("b.csv")).unwrap(),
        "someone else"
    );
}

#[test]
fn replace_keeps_the_previous_output_until_commit() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("out.csv");
    fs::write(&target, "previous").unwrap();
    let error = write_file(&target, Existing::Replace, |writer| {
        writer.write_all(b"half").unwrap();
        Err::<(), _>(TestError::Writer)
    })
    .unwrap_err();
    assert!(matches!(error, TestError::Writer));
    assert_eq!(fs::read_to_string(&target).unwrap(), "previous");
    assert_eq!(listing(root.path()), ["out.csv"]);

    write_file(&target, Existing::Replace, |writer| {
        writer
            .write_all(b"new")
            .map_err(|source| TestError::from(PublishError::io(&target, source)))
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn a_directory_in_the_way_fails_the_commit_before_anything_is_published() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("keep.txt"), "keep").unwrap();
    let mut publication = Publication::new(Existing::Replace);
    write_text(&mut publication, &root.path().join("first.csv"), "1").unwrap();
    write_text(
        &mut publication,
        &blocked,
        "a file cannot replace a directory",
    )
    .unwrap();
    assert!(matches!(publication.commit(), Err(PublishError::Io { .. })));
    assert_eq!(listing(root.path()), ["blocked"]);
    assert_eq!(
        fs::read_to_string(blocked.join("keep.txt")).unwrap(),
        "keep"
    );
}

/// Stage `locked/second.csv` after whatever `publication` holds, then commit
/// with `locked` read-only: that rename fails after the earlier ones succeed.
/// The staged temporary stays in `locked`, which also prevents its removal.
#[cfg(unix)]
fn commit_failing_at_a_locked_directory(
    mut publication: Publication,
    root: &Path,
) -> Result<(), PublishError> {
    use std::os::unix::fs::PermissionsExt;

    let locked = root.join("locked");
    fs::create_dir(&locked).unwrap();
    write_text(&mut publication, &locked.join("second.csv"), "2").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let result = publication.commit();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!locked.join("second.csv").exists());
    result
}

#[cfg(unix)]
#[test]
fn a_failing_rename_unpublishes_outputs_published_before_it() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Replace);
    write_text(&mut publication, &root.path().join("first.csv"), "1").unwrap();
    let result = commit_failing_at_a_locked_directory(publication, root.path());
    assert!(
        matches!(&result, Err(PublishError::Io { path, .. }) if path.ends_with("locked/second.csv")),
        "{result:?}"
    );
    assert_eq!(listing(root.path()), ["locked"]);
}

#[cfg(unix)]
#[test]
fn a_failing_rename_restores_files_replaced_before_it() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("first.csv");
    fs::write(&target, "previous").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let mut publication = Publication::new(Existing::Replace);
    write_text(&mut publication, &target, "new").unwrap();
    let result = commit_failing_at_a_locked_directory(publication, root.path());
    assert!(matches!(result, Err(PublishError::Io { .. })), "{result:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "previous");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(listing(root.path()), ["first.csv", "locked"]);
}

#[cfg(unix)]
#[test]
fn a_failing_rename_restores_directories_replaced_before_it() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("model");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("old.json"), "old").unwrap();
    let mut publication = Publication::new(Existing::Replace);
    let staged = publication.stage_dir(&target).unwrap();
    fs::write(staged.join("new.json"), "new").unwrap();
    let result = commit_failing_at_a_locked_directory(publication, root.path());
    assert!(matches!(result, Err(PublishError::Io { .. })), "{result:?}");
    assert_eq!(listing(&target), ["old.json"]);
    assert_eq!(fs::read_to_string(target.join("old.json")).unwrap(), "old");
    assert_eq!(listing(root.path()), ["locked", "model"]);
}

#[cfg(unix)]
#[test]
fn a_failing_rename_removes_directories_published_before_it() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    let staged = publication.stage_dir(root.path().join("model")).unwrap();
    fs::write(staged.join("new.json"), "new").unwrap();
    let result = commit_failing_at_a_locked_directory(publication, root.path());
    assert!(matches!(result, Err(PublishError::Io { .. })), "{result:?}");
    assert_eq!(listing(root.path()), ["locked"]);
}

#[cfg(unix)]
#[test]
fn a_failing_rename_removes_parent_directories_it_created() {
    let root = tempfile::tempdir().unwrap();
    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &root.path().join("a/b/first.csv"), "1").unwrap();
    let result = commit_failing_at_a_locked_directory(publication, root.path());
    assert!(matches!(result, Err(PublishError::Io { .. })), "{result:?}");
    assert_eq!(listing(root.path()), ["locked"]);
}

#[test]
fn replacing_outputs_leaves_no_backups_behind() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("out.csv");
    let directory = root.path().join("model");
    fs::write(&file, "previous").unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("old.json"), "old").unwrap();

    let mut publication = Publication::new(Existing::Replace);
    write_text(&mut publication, &file, "new").unwrap();
    let staged = publication.stage_dir(&directory).unwrap();
    fs::write(staged.join("new.json"), "new").unwrap();
    publication.commit().unwrap();

    assert_eq!(listing(root.path()), ["model", "out.csv"]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    assert_eq!(listing(&directory), ["new.json"]);
}

#[test]
fn directories_are_published_whole_and_replaced_with_a_backup() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("model");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("old.json"), "old").unwrap();

    let refused = Publication::new(Existing::Refuse).stage_dir(&target);
    assert!(matches!(refused, Err(PublishError::Exists { .. })));

    let mut publication = Publication::new(Existing::Replace);
    let staged = publication.stage_dir(&target).unwrap();
    fs::create_dir(staged.join("nested")).unwrap();
    fs::write(staged.join("nested/new.json"), "new").unwrap();
    assert_eq!(fs::read_to_string(target.join("old.json")).unwrap(), "old");
    publication.commit().unwrap();
    assert_eq!(listing(&target), ["nested"]);
    assert_eq!(
        fs::read_to_string(target.join("nested/new.json")).unwrap(),
        "new"
    );
    assert_eq!(listing(root.path()), ["model"]);
}

#[test]
fn created_parent_directories_are_removed_when_nothing_is_published() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("a/b/out.csv");
    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &target, "x").unwrap();
    assert!(root.path().join("a/b").is_dir());
    drop(publication);
    assert_eq!(listing(root.path()), Vec::<String>::new());

    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &target, "x").unwrap();
    publication.commit().unwrap();
    assert_eq!(fs::read_to_string(&target).unwrap(), "x");
}

#[test]
fn a_target_cannot_be_staged_twice() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("out.csv");
    let mut publication = Publication::new(Existing::Replace);
    write_text(&mut publication, &target, "1").unwrap();
    let error = write_text(&mut publication, &target, "2").unwrap_err();
    assert!(matches!(
        error,
        TestError::Publish(PublishError::StagedTwice { .. })
    ));
}

#[test]
fn stage_path_supports_writers_that_take_a_path() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("out.bin");
    let mut publication = Publication::new(Existing::Refuse);
    publication
        .stage_path(&target, |path: &Path| {
            fs::write(path, b"written by path").map_err(|source| PublishError::io(path, source))
        })
        .unwrap();
    publication.commit().unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"written by path");
}

#[cfg(unix)]
#[test]
fn modes_match_file_create_and_create_dir() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    fs::File::create(root.path().join("plain.txt")).unwrap();
    fs::create_dir(root.path().join("plain")).unwrap();

    let mut publication = Publication::new(Existing::Refuse);
    write_text(&mut publication, &root.path().join("published.txt"), "x").unwrap();
    publication
        .stage_dir(root.path().join("published"))
        .unwrap();
    publication.commit().unwrap();

    assert_eq!(
        mode(root.path().join("published.txt")),
        mode(root.path().join("plain.txt"))
    );
    assert_eq!(
        mode(root.path().join("published")),
        mode(root.path().join("plain"))
    );
}

#[test]
fn directory_outputs_refuse_another_runs_files_unless_replacing() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let empty = root.path().join("empty");
    let occupied = root.path().join("occupied");
    let file = root.path().join("file");
    fs::create_dir(&empty).unwrap();
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("metrics.json"), "{}").unwrap();
    fs::write(&file, "").unwrap();
    for existing in [Existing::Refuse, Existing::Replace] {
        check_output_dir(&missing, existing).unwrap();
        check_output_dir(&empty, existing).unwrap();
        assert!(matches!(
            check_output_dir(&file, existing),
            Err(PublishError::Exists { .. })
        ));
    }
    assert!(matches!(
        check_output_dir(&occupied, Existing::Refuse),
        Err(PublishError::Exists { path }) if path == occupied
    ));
    check_output_dir(&occupied, Existing::Replace).unwrap();
}
