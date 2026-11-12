use std::ffi::OsString;
use std::fs::{self, File, Permissions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use tempfile::{Builder, NamedTempFile, TempDir, TempPath};

use crate::{PublishError, RollbackFailure};

/// Suffix of a staged output, and of a published directory being discarded.
const STAGED: &str = ".tmp";
/// Suffix of the backup of a replaced output. It ends in `.tmp` too, so a
/// backup left by a crash is never taken for an output.
const BACKUP: &str = ".old.tmp";
/// Name of a directory moved aside, inside the temporary directory holding it.
const MOVED: &str = "previous";

/// What a publication does with a target that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    /// Fail: when the output is staged, before it is written, and again at
    /// commit.
    Refuse,
    /// Replace it at commit; until then the previous output stays in place.
    Replace,
}

impl Existing {
    /// `Replace` when the user passed `--overwrite`, `Refuse` otherwise.
    #[must_use]
    pub fn overwrite(overwrite: bool) -> Self {
        if overwrite {
            Self::Replace
        } else {
            Self::Refuse
        }
    }
}

/// The writer [`Publication::stage_file`] hands out. The publication flushes
/// and syncs it; the closure only writes.
pub type Staging<'a> = BufWriter<&'a mut File>;

enum Staged {
    File(NamedTempFile),
    Directory(TempDir),
}

struct Entry {
    target: PathBuf,
    staged: Staged,
}

/// An output published by a commit that has not finished, with what it
/// replaced.
enum Published {
    File {
        target: PathBuf,
        /// The replaced file, hard-linked or copied beside the target.
        previous: Option<TempPath>,
    },
    Directory {
        target: PathBuf,
        /// The replaced directory, moved aside as `<backup>/previous`.
        previous: Option<TempDir>,
    },
}

/// A set of outputs staged next to their targets and published together.
///
/// Temporary names are `.<target name>.<random>.tmp` in the target's
/// directory, so publishing is a rename on one file system. Files get the
/// permissions `File::create` would give them, directories those of
/// `fs::create_dir`. Dropping the publication without a successful
/// [`commit`](Self::commit) removes everything it staged and every parent
/// directory it created that is still empty; a commit that fails or panics
/// first undoes what it published.
pub struct Publication {
    existing: Existing,
    entries: Vec<Entry>,
    /// Parent directories created while staging, outermost first.
    created: Vec<PathBuf>,
    /// Outputs published by a commit that has not finished, oldest first.
    journal: Vec<Published>,
    committed: bool,
}

impl Publication {
    #[must_use]
    pub fn new(existing: Existing) -> Self {
        Self {
            existing,
            entries: Vec::new(),
            created: Vec::new(),
            journal: Vec::new(),
            committed: false,
        }
    }

    /// Fails with [`PublishError::Exists`] when `target` exists and existing
    /// outputs are refused. Commands call it for every output before doing any
    /// work.
    ///
    /// # Errors
    /// [`PublishError::Exists`] as described.
    pub fn check(&self, target: impl AsRef<Path>) -> Result<(), PublishError> {
        let target = target.as_ref();
        if self.existing == Existing::Refuse && exists(target) {
            return Err(PublishError::Exists {
                path: target.to_path_buf(),
            });
        }
        Ok(())
    }

    /// Stage the file `target`: `write` fills a temporary file next to it.
    ///
    /// # Errors
    /// [`PublishError`] when the target is refused, staged twice or the
    /// temporary file cannot be created, flushed or synced; whatever `write`
    /// returns. With [`Existing::Refuse`] and an existing target, `write` is
    /// not called.
    pub fn stage_file<E, F>(&mut self, target: impl AsRef<Path>, write: F) -> Result<(), E>
    where
        E: From<PublishError>,
        F: FnOnce(&mut Staging<'_>) -> Result<(), E>,
    {
        let target = self.prepare(target.as_ref())?;
        let mut file = temporary_file(&target)?;
        let mut writer = BufWriter::new(file.as_file_mut());
        write(&mut writer)?;
        writer
            .flush()
            .map_err(|source| PublishError::io(&target, source))?;
        drop(writer);
        file.as_file()
            .sync_all()
            .map_err(|source| PublishError::io(&target, source))?;
        self.entries.push(Entry {
            target,
            staged: Staged::File(file),
        });
        Ok(())
    }

    /// Stage the file `target` through a writer that takes a path: `write`
    /// receives the path of an empty temporary file next to `target` and may
    /// truncate or replace it.
    ///
    /// # Errors
    /// As [`stage_file`](Self::stage_file).
    pub fn stage_path<E, F>(&mut self, target: impl AsRef<Path>, write: F) -> Result<(), E>
    where
        E: From<PublishError>,
        F: FnOnce(&Path) -> Result<(), E>,
    {
        let target = self.prepare(target.as_ref())?;
        let file = temporary_file(&target)?;
        write(file.path())?;
        sync_file(file.path()).map_err(|source| PublishError::io(&target, source))?;
        self.entries.push(Entry {
            target,
            staged: Staged::File(file),
        });
        Ok(())
    }

    /// Stage the directory `target` and return the empty directory to fill
    /// in its place. Its files are synced at commit.
    ///
    /// # Errors
    /// [`PublishError`] when the target is refused, staged twice or the
    /// directory cannot be created.
    pub fn stage_dir(&mut self, target: impl AsRef<Path>) -> Result<PathBuf, PublishError> {
        let target = self.prepare(target.as_ref())?;
        let prefix = hidden_prefix(&target);
        let mut builder = Builder::new();
        builder.prefix(&prefix).rand_bytes(6).suffix(STAGED);
        request_mode(&mut builder, 0o777);
        let directory = builder
            .tempdir_in(parent(&target))
            .map_err(|source| PublishError::io(&target, source))?;
        let path = directory.path().to_path_buf();
        self.entries.push(Entry {
            target,
            staged: Staged::Directory(directory),
        });
        Ok(path)
    }

    /// Publish every staged output, in staging order, then sync the
    /// directories that hold them and those that hold the directories created
    /// for them.
    ///
    /// Every target is checked first, so a target that appeared since staging
    /// (with [`Existing::Refuse`]) or a directory where a file goes fails the
    /// commit before anything is published. Each file to replace is then
    /// hard-linked (or copied) to a backup beside it. If a rename or a sync
    /// fails, what was published is undone, newest first: new outputs are
    /// removed and replaced ones restored. Backups are removed only after the
    /// directories are synced.
    ///
    /// # Errors
    /// [`PublishError::Exists`] for a refused target; [`PublishError::Io`]
    /// when a backup, rename or sync fails and everything published was
    /// undone; [`PublishError::RollbackFailed`] when it could not be.
    pub fn commit(mut self) -> Result<(), PublishError> {
        for entry in &self.entries {
            self.check(&entry.target)?;
            match &entry.staged {
                Staged::Directory(directory) => sync_tree(directory.path())
                    .map_err(|source| PublishError::io(&entry.target, source))?,
                Staged::File(_) if is_directory(&entry.target) => {
                    return Err(PublishError::io(
                        &entry.target,
                        io::Error::new(
                            io::ErrorKind::IsADirectory,
                            "a file cannot replace a directory",
                        ),
                    ));
                }
                Staged::File(_) => {}
            }
        }
        let directories = self.directories();
        let mut backups = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let replaced = self.existing == Existing::Replace
                && matches!(entry.staged, Staged::File(_))
                && exists(&entry.target);
            backups.push(if replaced {
                Some(back_up_file(&entry.target)?)
            } else {
                None
            });
        }
        for (entry, backup) in std::mem::take(&mut self.entries).into_iter().zip(backups) {
            match publish(entry, backup, self.existing) {
                Ok(published) => self.journal.push(published),
                Err(error) => return Err(self.roll_back(error)),
            }
        }
        for directory in &directories {
            if let Err(source) = sync_directory(directory) {
                return Err(self.roll_back(PublishError::io(directory, source)));
            }
        }
        self.committed = true;
        // The renames are durable: drop the backups of what they replaced.
        self.journal.clear();
        Ok(())
    }

    fn prepare(&mut self, target: &Path) -> Result<PathBuf, PublishError> {
        if target.file_name().is_none() {
            return Err(PublishError::io(
                target,
                io::Error::new(io::ErrorKind::InvalidInput, "an output needs a file name"),
            ));
        }
        if self.entries.iter().any(|entry| entry.target == target) {
            return Err(PublishError::StagedTwice {
                path: target.to_path_buf(),
            });
        }
        self.check(target)?;
        let directory = parent(target);
        let missing = directory
            .ancestors()
            .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !exists(ancestor))
            .collect::<Vec<_>>();
        fs::create_dir_all(directory).map_err(|source| PublishError::io(directory, source))?;
        self.created
            .extend(missing.into_iter().rev().map(Path::to_path_buf));
        Ok(target.to_path_buf())
    }

    /// The directories a commit changes: those holding the targets and those
    /// holding the directories created while staging.
    fn directories(&self) -> Vec<PathBuf> {
        let paths = self
            .entries
            .iter()
            .map(|entry| entry.target.as_path())
            .chain(self.created.iter().map(PathBuf::as_path));
        let mut directories: Vec<PathBuf> = Vec::new();
        for path in paths {
            let directory = parent(path);
            if !directories.iter().any(|known| known == directory) {
                directories.push(directory.to_path_buf());
            }
        }
        directories
    }

    /// Undo the journal. Returns `error` when that succeeded and
    /// [`PublishError::RollbackFailed`] naming what was not undone otherwise.
    fn roll_back(&mut self, error: PublishError) -> PublishError {
        let mut failures = self.undo();
        if failures.is_empty() {
            return error;
        }
        match error {
            PublishError::RollbackFailed {
                source,
                failures: mut earlier,
            } => {
                earlier.append(&mut failures);
                PublishError::RollbackFailed {
                    source,
                    failures: earlier,
                }
            }
            error => PublishError::RollbackFailed {
                source: Box::new(error),
                failures,
            },
        }
    }

    /// Undo every published output, newest first, then sync the directories
    /// that hold them. Returns the steps that failed.
    fn undo(&mut self) -> Vec<RollbackFailure> {
        let mut failures = Vec::new();
        let mut directories: Vec<PathBuf> = Vec::new();
        while let Some(published) = self.journal.pop() {
            let directory = parent(published.target()).to_path_buf();
            if let Err(failure) = published.undo() {
                failures.push(failure);
            }
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        for directory in directories {
            if let Err(source) = sync_directory(&directory) {
                failures.push(RollbackFailure {
                    target: directory,
                    previous: None,
                    source,
                });
            }
        }
        failures
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        // Only a commit that panicked leaves outputs in the journal.
        let _ = self.undo();
        self.entries.clear();
        if !self.committed {
            for directory in self.created.iter().rev() {
                // Fails, as intended, when the directory is no longer empty.
                let _ = fs::remove_dir(directory);
            }
        }
    }
}

/// Check a directory output before any work: a run must not publish into a
/// directory that holds another run's files unless `existing` is
/// [`Existing::Replace`] (`--overwrite`). A missing or empty directory is
/// accepted; a file in its place is refused either way.
///
/// # Errors
/// [`PublishError::Exists`] for a non-empty directory with
/// [`Existing::Refuse`] or a non-directory at `directory`;
/// [`PublishError::Io`] when it cannot be inspected.
pub fn check_output_dir(
    directory: impl AsRef<Path>,
    existing: Existing,
) -> Result<(), PublishError> {
    let directory = directory.as_ref();
    let refused = || PublishError::Exists {
        path: directory.to_path_buf(),
    };
    match fs::metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PublishError::io(directory, error)),
        Ok(metadata) if !metadata.is_dir() => Err(refused()),
        Ok(_) if existing == Existing::Replace => Ok(()),
        Ok(_) => {
            let mut entries =
                fs::read_dir(directory).map_err(|error| PublishError::io(directory, error))?;
            if entries.next().is_some() {
                Err(refused())
            } else {
                Ok(())
            }
        }
    }
}

/// Write one file all-or-nothing: [`Publication::stage_file`] then
/// [`Publication::commit`].
///
/// # Errors
/// As those two.
pub fn write_file<E, F>(target: impl AsRef<Path>, existing: Existing, write: F) -> Result<(), E>
where
    E: From<PublishError>,
    F: FnOnce(&mut Staging<'_>) -> Result<(), E>,
{
    let mut publication = Publication::new(existing);
    publication.stage_file(target, write)?;
    publication.commit().map_err(E::from)
}

/// Rename a staged output onto its target. `backup` holds the file it
/// replaces, if any.
fn publish(
    entry: Entry,
    backup: Option<TempPath>,
    existing: Existing,
) -> Result<Published, PublishError> {
    let Entry { target, staged } = entry;
    match staged {
        Staged::File(file) => {
            let persisted = match existing {
                Existing::Refuse => file.persist_noclobber(&target),
                Existing::Replace => file.persist(&target),
            };
            match persisted {
                Ok(_) => Ok(Published::File {
                    target,
                    previous: backup,
                }),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    Err(PublishError::Exists { path: target })
                }
                Err(error) => Err(PublishError::io(&target, error.error)),
            }
        }
        Staged::Directory(directory) => {
            let previous = publish_directory(directory, &target, existing)?;
            Ok(Published::Directory { target, previous })
        }
    }
}

/// Rename a staged directory onto `target`. An existing target is moved
/// aside first and returned, so that a failed commit can restore it; if the
/// rename fails, it is restored at once.
fn publish_directory(
    directory: TempDir,
    target: &Path,
    existing: Existing,
) -> Result<Option<TempDir>, PublishError> {
    let io_error = |source| PublishError::io(target, source);
    if !exists(target) {
        fs::rename(directory.path(), target).map_err(io_error)?;
        let _ = directory.keep();
        return Ok(None);
    }
    if existing == Existing::Refuse {
        return Err(PublishError::Exists {
            path: target.to_path_buf(),
        });
    }
    let backup = move_aside(target, BACKUP).map_err(io_error)?;
    if let Err(source) = fs::rename(directory.path(), target) {
        return Err(match restore_directory(backup, target) {
            Ok(()) => io_error(source),
            Err(failure) => PublishError::RollbackFailed {
                source: Box::new(io_error(source)),
                failures: vec![failure],
            },
        });
    }
    let _ = directory.keep();
    Ok(Some(backup))
}

impl Published {
    fn target(&self) -> &Path {
        match self {
            Self::File { target, .. } | Self::Directory { target, .. } => target,
        }
    }

    /// Put back what was at the target before the commit. A backup that
    /// cannot be put back is kept and named in the failure.
    fn undo(self) -> Result<(), RollbackFailure> {
        match self {
            Self::File {
                target,
                previous: None,
            } => fs::remove_file(&target).map_err(|source| RollbackFailure {
                target,
                previous: None,
                source,
            }),
            Self::File {
                target,
                previous: Some(backup),
            } => backup.persist(&target).map_err(|error| RollbackFailure {
                target,
                previous: Some(keep_file(error.path)),
                source: error.error,
            }),
            Self::Directory { target, previous } => {
                let published = match move_aside(&target, STAGED) {
                    Ok(published) => published,
                    Err(source) => {
                        return Err(RollbackFailure {
                            previous: previous.map(keep_directory),
                            target,
                            source,
                        });
                    }
                };
                let Some(backup) = previous else {
                    // Dropping `published` removes the new directory.
                    return Ok(());
                };
                restore_directory(backup, &target).inspect_err(|_| {
                    // Leave the new output rather than nothing at the target.
                    let _ = fs::rename(published.path().join(MOVED), &target);
                })
            }
        }
    }
}

/// Hard-link the file `target` to `.<name>.<random>.old.tmp` beside it, or
/// copy it there when it cannot be linked. The target stays in place.
fn back_up_file(target: &Path) -> Result<TempPath, PublishError> {
    let linked = Builder::new()
        .prefix(&hidden_prefix(target))
        .rand_bytes(6)
        .suffix(BACKUP)
        .make_in(parent(target), |link| fs::hard_link(target, link));
    match linked {
        Ok(link) => Ok(link.into_temp_path()),
        // A copy restores a regular file only.
        Err(error) if !fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_file()) => {
            Err(PublishError::io(target, error))
        }
        Err(_) => copy_file(target).map_err(|source| PublishError::io(target, source)),
    }
}

/// Copy the file `target`, with its permissions, to
/// `.<name>.<random>.old.tmp` beside it, and sync the copy: once restored, it
/// must survive a crash.
fn copy_file(target: &Path) -> io::Result<TempPath> {
    let mut source = File::open(target)?;
    let mut copy = Builder::new()
        .prefix(&hidden_prefix(target))
        .rand_bytes(6)
        .suffix(BACKUP)
        .tempfile_in(parent(target))?;
    io::copy(&mut source, copy.as_file_mut())?;
    copy.as_file()
        .set_permissions(source.metadata()?.permissions())?;
    copy.as_file().sync_all()?;
    Ok(copy.into_temp_path())
}

/// Move `target` to `<aside>/previous`, `<aside>` being a new directory
/// `.<name>.<random><suffix>` beside it, so that the target disappears in one
/// rename. Dropping the result removes what was moved.
fn move_aside(target: &Path, suffix: &str) -> io::Result<TempDir> {
    let aside = Builder::new()
        .prefix(&hidden_prefix(target))
        .rand_bytes(6)
        .suffix(suffix)
        .tempdir_in(parent(target))?;
    fs::rename(target, aside.path().join(MOVED))?;
    Ok(aside)
}

/// Rename the directory moved aside into `backup` back onto `target`, which
/// must be free. On failure the backup is kept and named in the failure.
fn restore_directory(backup: TempDir, target: &Path) -> Result<(), RollbackFailure> {
    fs::rename(backup.path().join(MOVED), target).map_err(|source| RollbackFailure {
        target: target.to_path_buf(),
        previous: Some(keep_directory(backup)),
        source,
    })
}

fn keep_file(mut backup: TempPath) -> PathBuf {
    backup.disable_cleanup(true);
    backup.to_path_buf()
}

fn keep_directory(backup: TempDir) -> PathBuf {
    backup.keep().join(MOVED)
}

fn temporary_file(target: &Path) -> Result<NamedTempFile, PublishError> {
    let prefix = hidden_prefix(target);
    let mut builder = Builder::new();
    builder.prefix(&prefix).rand_bytes(6).suffix(STAGED);
    request_mode(&mut builder, 0o666);
    builder
        .tempfile_in(parent(target))
        .map_err(|source| PublishError::io(target, source))
}

fn hidden_prefix(target: &Path) -> OsString {
    let mut prefix = OsString::from(".");
    prefix.push(target.file_name().unwrap_or_default());
    prefix.push(".");
    prefix
}

fn parent(target: &Path) -> &Path {
    target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// A path is taken when anything is there, including a dangling symlink.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Request the mode that `File::create` (0o666) or `fs::create_dir` (0o777)
/// use; the process umask applies to both.
#[cfg(unix)]
fn request_mode(builder: &mut Builder<'_, '_>, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    builder.permissions(Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn request_mode(_builder: &mut Builder<'_, '_>, _mode: u32) {}

fn sync_file(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Sync every file and directory under `root`, iteratively.
fn sync_tree(root: &Path) -> io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                sync_file(&entry.path())?;
            }
        }
        sync_directory(&directory)?;
    }
    Ok(())
}

/// Undo failures injected through file system permissions.
#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn backup_name(path: &Path, target: &str) -> bool {
        let name = path.file_name().unwrap().to_string_lossy();
        name.starts_with(&format!(".{target}.")) && name.ends_with(BACKUP)
    }

    /// `target` now holds `new`, as after a rename over it.
    fn replace(target: &Path, new: &str) {
        let staged = target.with_extension("staged");
        fs::write(&staged, new).unwrap();
        fs::rename(staged, target).unwrap();
    }

    #[test]
    fn a_failed_file_undo_keeps_its_backup() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("out.csv");
        fs::write(&target, "previous").unwrap();
        let backup = back_up_file(&target).unwrap();
        replace(&target, "new");

        // Renaming the backup over the target needs a writable directory.
        set_mode(root.path(), 0o555);
        let failure = Published::File {
            target: target.clone(),
            previous: Some(backup),
        }
        .undo()
        .unwrap_err();
        set_mode(root.path(), 0o755);

        assert_eq!(failure.target, target);
        assert_eq!(failure.source.kind(), io::ErrorKind::PermissionDenied);
        let kept = failure.previous.unwrap();
        assert!(backup_name(&kept, "out.csv"), "{}", kept.display());
        assert_eq!(fs::read_to_string(&kept).unwrap(), "previous");
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
    }

    #[test]
    fn a_failed_directory_undo_keeps_its_backup_and_the_new_output() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("model");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("old.json"), "old").unwrap();
        let backup = move_aside(&target, BACKUP).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("new.json"), "new").unwrap();

        // Moving the previous directory out of its backup needs a writable
        // backup directory.
        let container = backup.path().to_path_buf();
        set_mode(&container, 0o555);
        let failure = Published::Directory {
            target: target.clone(),
            previous: Some(backup),
        }
        .undo()
        .unwrap_err();
        set_mode(&container, 0o755);

        assert_eq!(failure.target, target);
        let kept = failure.previous.unwrap();
        assert_eq!(kept, container.join(MOVED));
        assert!(backup_name(&container, "model"), "{}", container.display());
        assert_eq!(fs::read_to_string(kept.join("old.json")).unwrap(), "old");
        assert_eq!(fs::read_to_string(target.join("new.json")).unwrap(), "new");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_directory_undo_that_cannot_move_the_new_output_keeps_the_backup() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("model");
        fs::create_dir(&target).unwrap();
        let backup = move_aside(&target, BACKUP).unwrap();
        let container = backup.path().to_path_buf();
        fs::create_dir(&target).unwrap();

        set_mode(root.path(), 0o555);
        let failure = Published::Directory {
            target: target.clone(),
            previous: Some(backup),
        }
        .undo()
        .unwrap_err();
        set_mode(root.path(), 0o755);

        assert_eq!(failure.previous, Some(container.join(MOVED)));
        assert!(container.join(MOVED).is_dir());
        assert!(target.is_dir());
    }

    #[test]
    fn a_copied_backup_keeps_content_and_mode() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("out.csv");
        fs::write(&target, "previous").unwrap();
        set_mode(&target, 0o640);
        let copy = copy_file(&target).unwrap();
        assert!(backup_name(&copy, "out.csv"), "{}", copy.display());
        assert_eq!(fs::read_to_string(&copy).unwrap(), "previous");
        assert_eq!(mode(&copy), 0o640);
        let path = copy.to_path_buf();
        drop(copy);
        assert!(!path.exists());
    }
}
