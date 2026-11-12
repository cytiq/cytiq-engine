# cytiq-publish

All-or-nothing publication of command outputs. A command stages every output
next to its target, writes and syncs it, and publishes the set with one
`commit`. Until then nothing appears at a target path: an error, a panic or a
dropped publication leaves the previous outputs in place and no temporary
files behind. A commit that fails undoes what it had published, so a failed
run publishes none of its outputs.

## API

| Item | Purpose |
| --- | --- |
| `Existing::{Refuse, Replace}` | what to do with a target that exists; `Existing::overwrite(flag)` maps `--overwrite` |
| `Publication::new(existing)` | an empty set of staged outputs |
| `check(target)` | fail early, before any work, when a target is refused |
| `stage_file(target, \|writer\| ...)` | stage a file written through a buffered writer |
| `stage_path(target, \|path\| ...)` | stage a file written by a writer that takes a path |
| `stage_dir(target)` | stage a directory; returns the empty directory to fill |
| `commit()` | publish every staged output in staging order, then sync their directories; undo everything if that fails |
| `write_file(target, existing, \|writer\| ...)` | one file: stage and commit |
| `check_output_dir(directory, existing)` | before any work: refuse a non-empty directory output unless `Existing::Replace` |

Errors are `PublishError::{Exists, Locked, StagedTwice, Io, RollbackFailed}`;
`PublishError::path()` is the path an error is about. `RollbackFailed` keeps
the error that failed the commit as its `source` and lists a
`RollbackFailure { target, previous, source }` for every step it could not
undo, with `previous` naming where a replaced output was kept. The staging
closures return the caller's error type, which needs `From<PublishError>`.

## Guarantees

- A temporary output is `.<target name>.<random>.tmp` in the target's
  directory, so publishing is a rename on one file system.
- Files are flushed and `fsync`ed before they are published; files inside a
  staged directory are synced at commit; after the renames, the directories
  holding the targets are synced, and so are the directories holding the
  parent directories created while staging.
- Files get the permissions `File::create` gives them and directories those of
  `fs::create_dir` (0o666 and 0o777 minus the umask), not `tempfile`'s
  owner-only 0o600 / 0o700.
- `Refuse` is checked when an output is staged, before its writer runs, and
  again at commit before anything is renamed. Files are then published with a
  no-clobber rename.
- `Replace` leaves the previous output in place until commit. Before the first
  rename, every file to replace is hard-linked to a backup
  `.<target name>.<random>.old.tmp` beside it (copied and synced where hard
  links fail), and the new file is renamed over it: the target never goes
  missing. A directory is replaced by moving the previous one into a
  `.<target name>.<random>.old.tmp` directory and renaming the staged one in.
- If a rename or a sync fails, the commit undoes what it published, newest
  first: new outputs are removed, replaced ones renamed back, and the
  directories synced again. It then returns the original error, or
  `RollbackFailed` when a step could not be undone; a backup that could not be
  put back is kept and named in the error. A panic during a commit unwinds
  into the same undo.
- Backups are removed only after the directories are synced.
- Parent directories created while staging are removed again if nothing is
  published.

## Crashes

A crash (or a panic with `panic = "abort"`, as in the release profile) is not
undone. Because every output is published by one rename:

- each target holds either its previous or its new output, never a mix of the
  two; a replaced directory is briefly absent between being moved aside and
  the new one being renamed in, and a crash then leaves it under
  `.<target name>.<random>.old.tmp/previous`;
- the set can be mixed: outputs renamed before the crash are new, the rest are
  previous ones or missing;
- staged outputs `.<target name>.<random>.tmp` and backups
  `.<target name>.<random>.old.tmp` can remain beside the targets. Every such
  name ends in `.tmp`, so `cytiq-provenance` never records one as an output,
  and they are safe to delete.

A set is therefore complete when its provenance record exists and every output
it lists has the recorded size and SHA-256. Stage the record last, so that it
is the last output a crash can leave new.

## Usage

```rust
use std::io::Write;
use cytiq_publish::{Existing, Publication, PublishError};

fn run(output: &std::path::Path, overwrite: bool) -> Result<(), PublishError> {
    let mut publication = Publication::new(Existing::overwrite(overwrite));
    publication.check(output.join("result.csv"))?;
    // ... compute ...
    publication.stage_file(output.join("result.csv"), |w| {
        w.write_all(b"a,b\n").map_err(|e| PublishError::io("result.csv", e))
    })?;
    publication.commit()
}
```

## Locking and recovery

`OutputLock::acquire(target)` takes a nonblocking kernel lock on a sibling
`.<name>.cytiq-lock`, outside any directory publication swaps. Keep the guard alive
from recovery/assessment through the final provenance write. Another owner receives
`PublishError::Locked`. The guard unlinks its lock file while holding the lock;
on Unix it checks the inode before accepting or unlinking it. A crash releases the
kernel lock, and the next invocation can claim the leftover file.

`recovery::leftovers(target)` recognizes only that target's exact six-character
`.tmp`/`.old.tmp` names. `recovery::recover(&lock)` restores a missing target from a
unique backup (directories use `previous`), cleans abandoned staging, and syncs the
parent. Multiple possible predecessors are preserved and reported as a conflict.
Mixed file sets still require a matching provenance record and verified hashes.
Locks are cooperative: older binaries do not take them, and filesystem support
varies on NFS, SMB and FUSE. CLI recompute warns if locking is explicitly unsupported;
reuse requires a working lock. Package publication requires locking for every policy.
