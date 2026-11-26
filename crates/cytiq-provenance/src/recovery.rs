//! Finish a publication a crash interrupted between two renames.
//!
//! A command stages its outputs and, last, its record; every staged file is
//! synced before the first rename. A crash between two renames leaves some
//! outputs new and the others, and the record, as they were. The staged record
//! then says what the whole set was: when every output it lists is in place or
//! still staged with the recorded bytes, the remaining renames are done and the
//! set is complete. Nothing is invented and nothing complete is discarded.
//! Otherwise the destination is left to [`cytiq_publish::recovery::recover`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cytiq_publish::{OutputLock, PublishError, recovery};

use crate::reuse::Destination;
use crate::{FileRecord, RunRecord, SCHEMA_VERSION, hash_file};

/// Recover `destination`, whose lock is held: complete an interrupted
/// publication, restore an output moved aside, then discard abandoned staging
/// of the destination and of its record.
///
/// # Errors
/// Returns I/O errors, and a conflict when an output has several predecessors.
pub fn recover(lock: &OutputLock, destination: &Destination) -> Result<(), PublishError> {
    let record_path = destination.record_path();
    let outputs = complete(&record_path)?;
    recovery::recover(lock)?;
    let directories = directories_below(&outputs, destination);
    discard_abandoned(
        outputs
            .iter()
            .chain(&directories)
            .chain(std::iter::once(&record_path)),
    )
}

/// The directories between `outputs` and a directory destination: a command
/// can publish a whole subdirectory, whose staging is beside it.
fn directories_below(outputs: &[PathBuf], destination: &Destination) -> Vec<PathBuf> {
    let Destination::Dir(root) = destination else {
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = outputs
        .iter()
        .flat_map(|output| output.ancestors().skip(1))
        .filter(|directory| directory.starts_with(root) && *directory != root.as_path())
        .map(Path::to_path_buf)
        .collect();
    directories.sort();
    directories.dedup();
    directories
}

/// Discard the staging and the backups a killed run left beside `paths`, but
/// not beside one that another process has locked. A command with several
/// outputs locks only the first, and knows the others once it has staged them.
///
/// # Errors
/// Returns I/O errors, and a conflict when an output has several predecessors.
pub fn discard_abandoned<'a>(
    paths: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<(), PublishError> {
    for path in paths {
        // Locking writes beside the path, so only when there is work to do.
        if recovery::leftovers(path)?.is_empty() {
            continue;
        }
        // Another process may be publishing a file this one also wrote; its
        // staging is not ours to discard.
        match OutputLock::acquire(path) {
            Ok(lock) => recovery::recover(&lock)?,
            Err(PublishError::Locked { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Do the renames a crash left undone; the outputs of the completed set, or
/// nothing.
fn complete(record_path: &Path) -> Result<Vec<PathBuf>, PublishError> {
    for staged in staged_files(record_path)? {
        let Some(record) = record_of(&staged) else {
            continue;
        };
        let Some(renames) = undone(&record)? else {
            continue;
        };
        for (from, to) in renames
            .iter()
            .map(|(from, to)| (from.as_path(), to.as_path()))
            .chain(std::iter::once((staged.as_path(), record_path)))
        {
            fs::rename(from, to).map_err(|error| PublishError::io(to, error))?;
            sync_parent(to)?;
        }
        return Ok(record
            .outputs
            .iter()
            .map(|output| PathBuf::from(&output.path))
            .collect());
    }
    Ok(Vec::new())
}

/// The renames that complete `record`'s set: `None` unless an output is already
/// in place and every other one is in place or staged.
fn undone(record: &RunRecord) -> Result<Option<Vec<(PathBuf, PathBuf)>>, PublishError> {
    let mut renames = Vec::new();
    let mut published = false;
    for output in &record.outputs {
        let target = PathBuf::from(&output.path);
        if holds(&target, output) {
            published = true;
            continue;
        }
        let staged = staged_files(&target)?
            .into_iter()
            .find(|file| holds(file, output));
        let Some(staged) = staged else {
            return Ok(None);
        };
        renames.push((staged, target));
    }
    Ok(published.then_some(renames))
}

/// Staged files of `target`, without the backups of what it replaced.
fn staged_files(target: &Path) -> Result<Vec<PathBuf>, PublishError> {
    Ok(recovery::leftovers(target)?
        .into_iter()
        .filter(|path| path.is_file() && !path.to_string_lossy().ends_with(".old.tmp"))
        .collect())
}

fn holds(path: &Path, output: &FileRecord) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
        && hash_file(path)
            .is_ok_and(|(bytes, sha256)| bytes == output.bytes && sha256 == output.sha256)
}

/// A whole record this version writes, or `None`.
fn record_of(path: &Path) -> Option<RunRecord> {
    let bytes = crate::input::document_bytes(path).ok()?;
    let record: RunRecord = serde_json::from_slice(&bytes).ok()?;
    let whole = record.schema_version == SCHEMA_VERSION
        && !record.outputs.is_empty()
        && record.execution_fingerprint.as_deref() == Some(record.compute_fingerprint().as_str());
    whole.then_some(record)
}

fn sync_parent(path: &Path) -> Result<(), PublishError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let synced = if cfg!(unix) {
        fs::File::open(parent).and_then(|directory| directory.sync_all())
    } else {
        io::Result::Ok(())
    };
    synced.map_err(|error| PublishError::io(parent, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_below_a_directory_destination_only() {
        let outputs = [
            PathBuf::from("out/tube_1/cutoffs.csv"),
            PathBuf::from("out/tube_1/deep/ssi.csv"),
            PathBuf::from("out/summary.json"),
        ];
        assert_eq!(
            directories_below(&outputs, &Destination::Dir(PathBuf::from("out"))),
            [
                PathBuf::from("out/tube_1"),
                PathBuf::from("out/tube_1/deep")
            ]
        );
        assert_eq!(
            directories_below(&outputs, &Destination::File(PathBuf::from("out"))),
            Vec::<PathBuf>::new()
        );
    }
}
