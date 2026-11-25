//! Open the objects of a dataset without leaving its root.
//!
//! An object is opened once, one path component at a time, and never through
//! a symbolic link below the root: no link, however it is nested, can make a
//! dataset describe a file outside it. Whoever asked reads the returned handle
//! and never the path again, so what is hashed is what is read. The root
//! itself may be a link: it is resolved once.

use std::fs::File;
use std::io::{self, Seek as _};
use std::path::Path;

use crate::identity::{ContentRef, ObjectKey};
use crate::issue::{IssueCode, Issues};

/// Why an object could not be opened.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// Nothing of that name, or not a regular file.
    Missing,
    /// The key is, or passes through, a symbolic link.
    Link,
    Io(io::Error),
}

impl OpenError {
    /// Report why `location` cannot be read, at `at`.
    pub fn report(&self, location: &str, at: impl Into<String>, issues: &mut Issues) {
        let (code, message) = match self {
            Self::Missing => (IssueCode::MissingObject, format!("`{location}` is missing")),
            Self::Link => (
                IssueCode::InvalidReference,
                format!(
                    "`{location}` is or passes through a symbolic link; an object is a regular \
                     file inside the dataset root"
                ),
            ),
            Self::Io(error) => (
                IssueCode::MissingObject,
                format!("`{location}` cannot be read: {error}"),
            ),
        };
        issues.error(code, at, message);
    }
}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Self::Missing,
            _ => Self::Io(error),
        }
    }
}

/// The directory a dataset's object keys resolve against.
#[derive(Debug)]
pub(crate) struct ObjectRoot(platform::Directory);

impl ObjectRoot {
    /// Resolve and open `root`; an empty path is the current directory.
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = if root.as_os_str().is_empty() {
            Path::new(".")
        } else {
            root
        };
        platform::Directory::open(&std::fs::canonicalize(root)?).map(Self)
    }

    /// Open the regular file `key` names under the root.
    pub fn object(&self, key: &ObjectKey) -> Result<File, OpenError> {
        let file = self.0.object(key.as_str())?;
        if file.metadata()?.is_file() {
            Ok(file)
        } else {
            Err(OpenError::Missing)
        }
    }

    /// Open `location`, hash it and return the handle, rewound, when it has
    /// the recorded size and digest. Reports why it does not under `at`.
    /// `None` without a report for a key the document rules already refuse.
    pub fn verified(
        &self,
        location: &str,
        content: &ContentRef,
        at: &str,
        issues: &mut Issues,
    ) -> Option<File> {
        let key = ObjectKey::parse(location).ok()?;
        let file = match self.object(&key) {
            Ok(file) => file,
            Err(error) => {
                error.report(location, format!("{at}/location"), issues);
                return None;
            }
        };
        let expected = (content.bytes, content.sha256.as_str());
        verified(
            file,
            location,
            expected,
            at,
            &format!("{at}/content"),
            issues,
        )
    }
}

/// `file` rewound, when it has the expected size and digest; a read error is
/// reported at `at`'s location and a difference at `content_at`.
pub(crate) fn verified(
    mut file: File,
    location: &str,
    expected: (u64, &str),
    at: &str,
    content_at: &str,
    issues: &mut Issues,
) -> Option<File> {
    let hashed =
        cytiq_provenance::hash_reader(&mut file).and_then(|hashed| file.rewind().map(|()| hashed));
    match hashed {
        Ok((bytes, sha256)) if (bytes, sha256.as_str()) == expected => Some(file),
        Ok((bytes, _)) => {
            issues.error(
                IssueCode::CorruptArtifactHash,
                content_at,
                format!(
                    "`{location}` has {bytes} bytes and a different digest than recorded ({} \
                     bytes)",
                    expected.0
                ),
            );
            None
        }
        Err(error) => {
            OpenError::Io(error).report(location, format!("{at}/location"), issues);
            None
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{AtFlags, FileType, Mode, OFlags};

    use super::OpenError;

    #[derive(Debug)]
    pub(super) struct Directory(OwnedFd);

    /// Directories are opened to resolve names in, never through a link.
    const DIRECTORY: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    /// Without `NONBLOCK`, opening a FIFO would wait for its writer; a regular
    /// file ignores the flag.
    const OBJECT: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);

    impl Directory {
        pub fn open(root: &Path) -> io::Result<Self> {
            Ok(Self(rustix::fs::open(root, DIRECTORY, Mode::empty())?))
        }

        pub fn object(&self, key: &str) -> Result<File, OpenError> {
            let mut directory = None;
            let mut segments = key.split('/').peekable();
            while let Some(segment) = segments.next() {
                let parent = directory.as_ref().map_or(self.0.as_fd(), OwnedFd::as_fd);
                let flags = if segments.peek().is_some() {
                    DIRECTORY
                } else {
                    OBJECT
                };
                match rustix::fs::openat(parent, segment, flags, Mode::empty()) {
                    Ok(opened) if segments.peek().is_none() => return Ok(File::from(opened)),
                    Ok(opened) => directory = Some(opened),
                    Err(error) => {
                        // Only names the failure: nothing is opened after it.
                        let link = rustix::fs::statat(parent, segment, AtFlags::SYMLINK_NOFOLLOW)
                            .is_ok_and(|stat| {
                                FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
                            });
                        return Err(if link {
                            OpenError::Link
                        } else {
                            io::Error::from(error).into()
                        });
                    }
                }
            }
            Err(OpenError::Missing)
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::fs::File;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::OpenError;

    #[derive(Debug)]
    pub(super) struct Directory(PathBuf);

    impl Directory {
        pub fn open(root: &Path) -> io::Result<Self> {
            Ok(Self(root.to_path_buf()))
        }

        pub fn object(&self, key: &str) -> Result<File, OpenError> {
            let mut path = self.0.clone();
            for segment in key.split('/') {
                path.push(segment);
                if std::fs::symlink_metadata(&path)?.is_symlink() {
                    return Err(OpenError::Link);
                }
            }
            Ok(File::open(path)?)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::io::Read as _;
    use std::os::unix::fs::symlink;

    use super::*;

    fn content(bytes: &[u8]) -> ContentRef {
        let (bytes, sha256) = cytiq_provenance::hash_reader(&mut &*bytes).unwrap();
        ContentRef {
            sha256,
            bytes,
            values_sha256: None,
        }
    }

    #[test]
    fn what_was_hashed_is_what_is_read_whatever_happens_to_the_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("table");
        fs::write(&path, b"verified").unwrap();
        let root = ObjectRoot::open(temp.path()).unwrap();
        let mut issues = Issues::new();
        let mut file = root
            .verified("table", &content(b"verified"), "", &mut issues)
            .unwrap();
        // The name now leads to another file, then to a link.
        fs::write(temp.path().join("other"), b"replaced").unwrap();
        fs::rename(temp.path().join("other"), &path).unwrap();
        let mut read = String::new();
        file.read_to_string(&mut read).unwrap();
        assert_eq!(read, "verified");
        assert!(!issues.has_errors());
        fs::remove_file(&path).unwrap();
        symlink("/etc/hosts", &path).unwrap();
        assert!(matches!(
            root.object(&ObjectKey::parse("table").unwrap()),
            Err(OpenError::Link)
        ));
    }

    #[test]
    fn a_root_moved_after_it_was_opened_stays_the_same_root() {
        let temp = tempfile::tempdir().unwrap();
        let (first, second) = (temp.path().join("a"), temp.path().join("b"));
        fs::create_dir(&first).unwrap();
        fs::write(first.join("table"), b"inside").unwrap();
        let root = ObjectRoot::open(&first).unwrap();
        fs::rename(&first, &second).unwrap();
        fs::create_dir(&first).unwrap();
        fs::write(first.join("table"), b"impostor").unwrap();
        let mut read = String::new();
        root.object(&ObjectKey::parse("table").unwrap())
            .unwrap()
            .read_to_string(&mut read)
            .unwrap();
        assert_eq!(read, "inside");
    }

    #[test]
    fn a_missing_object_a_difference_and_a_link_have_their_own_codes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("table"), b"bytes").unwrap();
        fs::create_dir(temp.path().join("dir")).unwrap();
        symlink(temp.path().join("dir"), temp.path().join("link")).unwrap();
        let root = ObjectRoot::open(temp.path()).unwrap();
        for (location, code) in [
            ("absent", IssueCode::MissingObject),
            ("table/absent", IssueCode::MissingObject),
            ("dir", IssueCode::MissingObject),
            ("link/table", IssueCode::InvalidReference),
            ("link", IssueCode::InvalidReference),
            ("table", IssueCode::CorruptArtifactHash),
        ] {
            let mut issues = Issues::new();
            assert!(
                root.verified(location, &content(b"other"), "", &mut issues)
                    .is_none()
            );
            assert_eq!(issues.items()[0].code, code, "{location}");
        }
    }
}
