//! Run provenance: which command produced which files from which inputs.
//!
//! Each cytiq command that writes files also writes a `provenance.json`
//! record next to its output: the tool, version and build, the platform, the
//! exact command-line arguments and the effective configuration, fixed
//! algorithm constants and the upstream versions the algorithms port, start
//! and finish times, and the size, SHA-256 and role of every input and output
//! file. Inputs are hashed before the command runs, so a command that replaces
//! its own input (`transform --overwrite` in place) still records what it
//! read.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_publish::{Existing, PublishError};
use log::debug;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod fingerprint;
pub mod input;
pub mod outcome;
pub mod reuse;

/// Version 3 adds execution fingerprints, completion reasons and optional parallelism.
/// Version 2 records remain readable, but cannot be reused.
pub const SCHEMA_VERSION: u32 = 3;
pub const MIN_READ_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("failed to hash `{path}` for provenance: {source}")]
    Hash {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "input `{path}` changed while the command ran; its provenance record would not describe what was read"
    )]
    InputChanged { path: PathBuf },
    #[error("failed to write provenance record `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ProvenanceError {
    /// Where the error falls in the exit contract: hashing and writing follow
    /// their I/O kind; an input changing under the command is a runtime error.
    #[must_use]
    pub fn outcome_reason(&self) -> outcome::OutcomeReason {
        match self {
            Self::Hash { source, .. } | Self::Write { source, .. } => {
                outcome::OutcomeReason::of_io(source)
            }
            Self::InputChanged { .. } => outcome::OutcomeReason::RuntimeError,
        }
    }
}

/// Size and SHA-256 of one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    /// What the command read the file as: `input`, `mask`, `gating_ml`,
    /// `matrix`, `manifest`, `control`, `reference`, ... Outputs have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub bytes: u64,
    pub sha256: String,
}

/// How the binary was built.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Commit the binary was built from, or `unknown`.
    pub git_commit: String,
    pub rustc: String,
    /// Target triple.
    pub target: String,
    /// Cargo profile: `release` or `debug`.
    pub profile: String,
}

impl BuildInfo {
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            git_commit: "unknown".to_owned(),
            rustc: "unknown".to_owned(),
            target: "unknown".to_owned(),
            profile: "unknown".to_owned(),
        }
    }
}

/// Where the command ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    /// Logical CPUs available; the default thread count of parallel stages.
    pub cpus: usize,
}

impl Platform {
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
        }
    }
}

impl FileRecord {
    /// Hash `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read.
    pub fn of(path: &Path) -> Result<Self, ProvenanceError> {
        let (bytes, sha256) = hash_file(path).map_err(|source| ProvenanceError::Hash {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            path: path.display().to_string(),
            role: None,
            bytes,
            sha256,
        })
    }
}

/// The `provenance.json` document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    /// `cytiq <command>`.
    pub tool: String,
    pub version: String,
    pub build: BuildInfo,
    pub platform: Platform,
    /// Command-line arguments after the program name, exactly as given.
    pub arguments: Vec<String>,
    /// Every option of the command with its default applied.
    pub config: serde_json::Value,
    /// Fixed algorithm constants that shape the result but are no options.
    pub constants: serde_json::Value,
    /// Upstream implementations the command's algorithms port: name to
    /// version.
    pub algorithms: BTreeMap<String, String>,
    /// Milliseconds since the Unix epoch.
    pub started_at_unix_ms: u128,
    pub finished_at_unix_ms: u128,
    pub inputs: Vec<FileRecord>,
    pub outputs: Vec<FileRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<outcome::OutcomeReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<usize>,
}

impl RunRecord {
    #[must_use]
    pub fn compute_fingerprint(&self) -> String {
        fingerprint::compute(self)
    }
}

/// Collects a run's record: start it and snapshot the inputs before the
/// command runs, finish it with the files the command wrote.
#[derive(Clone, Debug)]
pub struct RunRecorder {
    tool: String,
    version: String,
    build: BuildInfo,
    arguments: Vec<String>,
    config: serde_json::Value,
    constants: serde_json::Value,
    algorithms: BTreeMap<String, String>,
    started_at_unix_ms: u128,
    inputs: Vec<InputSnapshot>,
    parallelism: Option<usize>,
}

/// An input as it was before the command ran.
#[derive(Clone, Debug)]
struct InputSnapshot {
    path: PathBuf,
    record: FileRecord,
}

impl RunRecorder {
    /// Build a v3 record for staged files without publishing anything.
    /// Each pair contains the final path and the file to hash before commit.
    ///
    /// # Errors
    /// Rejects changed inputs, missing staged files and hashing failures.
    pub fn prepare_staged(
        self,
        outputs: &[(PathBuf, PathBuf)],
        reason: outcome::OutcomeReason,
    ) -> Result<RunRecord, ProvenanceError> {
        self.verify_inputs()?;
        let outputs = outputs
            .iter()
            .map(|(target, staged)| {
                let mut record = FileRecord::of(staged)?;
                record.path = target.display().to_string();
                Ok(record)
            })
            .collect::<Result<Vec<_>, ProvenanceError>>()?;
        let mut record = RunRecord {
            schema_version: SCHEMA_VERSION,
            tool: self.tool,
            version: self.version,
            build: self.build,
            platform: Platform::current(),
            arguments: self.arguments,
            config: self.config,
            constants: self.constants,
            algorithms: self.algorithms,
            started_at_unix_ms: self.started_at_unix_ms,
            finished_at_unix_ms: unix_ms(),
            inputs: self.inputs.into_iter().map(|input| input.record).collect(),
            outputs,
            execution_fingerprint: None,
            reason: Some(reason),
            parallelism: self.parallelism,
        };
        record.execution_fingerprint = Some(record.compute_fingerprint());
        Ok(record)
    }

    #[must_use]
    pub fn start(
        tool: impl Into<String>,
        version: impl Into<String>,
        arguments: Vec<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            version: version.into(),
            build: BuildInfo::unknown(),
            arguments,
            config: serde_json::Value::Null,
            constants: serde_json::Value::Null,
            algorithms: BTreeMap::new(),
            started_at_unix_ms: unix_ms(),
            inputs: Vec::new(),
            parallelism: None,
        }
    }

    #[must_use]
    pub fn with_build(mut self, build: BuildInfo) -> Self {
        self.build = build;
        self
    }

    pub fn set_parallelism(&mut self, threads: usize) {
        self.parallelism = Some(threads);
    }

    #[must_use]
    pub fn execution_fingerprint(&self) -> String {
        self.snapshot_record().compute_fingerprint()
    }

    /// Current semantic inputs, suitable for read-only reuse assessment.
    #[must_use]
    pub fn snapshot_record(&self) -> RunRecord {
        RunRecord {
            schema_version: SCHEMA_VERSION,
            tool: self.tool.clone(),
            version: self.version.clone(),
            build: self.build.clone(),
            platform: Platform::current(),
            arguments: self.arguments.clone(),
            config: self.config.clone(),
            constants: self.constants.clone(),
            algorithms: self.algorithms.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            finished_at_unix_ms: unix_ms(),
            inputs: self.inputs.iter().map(|i| i.record.clone()).collect(),
            outputs: Vec::new(),
            execution_fingerprint: None,
            reason: None,
            parallelism: self.parallelism,
        }
    }

    /// Record the command's effective configuration.
    pub fn set_config(&mut self, config: &impl Serialize) {
        self.config = to_value(config);
    }

    /// Record fixed algorithm constants.
    pub fn set_constants(&mut self, constants: &impl Serialize) {
        self.constants = to_value(constants);
    }

    /// Record the upstream implementations the algorithms port.
    pub fn set_algorithms(&mut self, algorithms: &[(&str, &str)]) {
        self.algorithms = algorithms
            .iter()
            .map(|(name, version)| ((*name).to_owned(), (*version).to_owned()))
            .collect();
    }

    /// Hash `inputs` now, before the command reads them, recording `role`
    /// for each. Paths that are not files (missing optional inputs,
    /// directories) are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when an input cannot be hashed.
    pub fn snapshot_inputs(
        &mut self,
        role: &str,
        inputs: &[PathBuf],
    ) -> Result<(), ProvenanceError> {
        for path in inputs.iter().filter(|path| path.is_file()) {
            let mut record = FileRecord::of(path)?;
            record.role = Some(role.to_owned());
            self.inputs.push(InputSnapshot {
                path: path.clone(),
                record,
            });
        }
        Ok(())
    }

    /// Verify that every snapshotted input still has its original bytes.
    /// # Errors
    /// Returns hashing failures or `InputChanged`, including changes with preserved mtimes.
    pub fn verify_inputs(&self) -> Result<(), ProvenanceError> {
        for input in &self.inputs {
            Self::verify_input(input)?;
        }
        Ok(())
    }

    fn verify_input(input: &InputSnapshot) -> Result<(), ProvenanceError> {
        let current = hash_file(&input.path).map_err(|source| ProvenanceError::Hash {
            path: input.path.clone(),
            source,
        })?;
        if current != (input.record.bytes, input.record.sha256.clone()) {
            return Err(ProvenanceError::InputChanged {
                path: input.path.clone(),
            });
        }
        Ok(())
    }

    /// [`Self::snapshot_inputs`] for files that each carry their role.
    ///
    /// # Errors
    ///
    /// Returns an error when an input cannot be hashed.
    pub fn snapshot_roles(&mut self, inputs: &[(&str, PathBuf)]) -> Result<(), ProvenanceError> {
        for (role, path) in inputs {
            self.snapshot_inputs(role, std::slice::from_ref(path))?;
        }
        Ok(())
    }

    /// Hash `outputs` and write the record, with the inputs as snapshotted,
    /// to `record_path` (through a `.tmp` sibling, so a failed run never
    /// leaves half a record). Paths that are not files (missing optional
    /// outputs, directories) are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when an input that is not also an output changed
    /// since its snapshot, a file cannot be hashed, or the record cannot be
    /// written.
    pub fn finish(
        self,
        outputs: &[PathBuf],
        record_path: &Path,
        reason: outcome::OutcomeReason,
    ) -> Result<RunRecord, ProvenanceError> {
        let outputs = outputs
            .iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        let written = outputs
            .iter()
            .map(|path| canonical(path))
            .collect::<Vec<_>>();
        for input in &self.inputs {
            if !written.contains(&canonical(&input.path)) {
                Self::verify_input(input)?;
            }
        }
        let mut record = RunRecord {
            schema_version: SCHEMA_VERSION,
            tool: self.tool,
            version: self.version,
            build: self.build,
            platform: Platform::current(),
            arguments: self.arguments,
            config: self.config,
            constants: self.constants,
            algorithms: self.algorithms,
            started_at_unix_ms: self.started_at_unix_ms,
            finished_at_unix_ms: unix_ms(),
            inputs: self.inputs.into_iter().map(|input| input.record).collect(),
            outputs: outputs
                .into_iter()
                .map(|path| FileRecord::of(path))
                .collect::<Result<_, _>>()?,
            execution_fingerprint: None,
            reason: Some(reason),
            parallelism: self.parallelism,
        };
        record.execution_fingerprint = Some(record.compute_fingerprint());
        write_record(record_path, &record)?;
        Ok(record)
    }
}

/// `sample.comp.fcs` -> `sample.comp.fcs.provenance.json`.
#[must_use]
pub fn record_path_for_file(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".provenance.json");
    PathBuf::from(name)
}

/// `clean/` -> `clean/provenance.json`.
#[must_use]
pub fn record_path_for_dir(output_dir: &Path) -> PathBuf {
    output_dir.join("provenance.json")
}

/// Every file under `dir`, recursively and sorted, except provenance records
/// and temporary files and directories. A missing directory yields nothing.
#[must_use]
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !is_temporary(&path) {
                    pending.push(path);
                }
            } else if !is_record_or_temporary(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The files under a directory output before a run. With `--overwrite` a
/// directory can hold an earlier run's files; [`DirSnapshot::published`]
/// lists only what this run wrote, so a record never claims leftovers.
///
/// Publication always renames a new file into place, so a published file has
/// a new identity (inode on Unix, else size and modification time) even when
/// its bytes did not change.
#[derive(Clone, Debug, Default)]
pub struct DirSnapshot {
    files: BTreeMap<PathBuf, FileStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt as _;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}

impl DirSnapshot {
    /// Record the files [`files_under`] `dir` lists now.
    #[must_use]
    pub fn take(dir: &Path) -> Self {
        Self {
            files: files_under(dir)
                .into_iter()
                .filter_map(|path| FileStamp::of(&path).map(|stamp| (path, stamp)))
                .collect(),
        }
    }

    /// [`files_under`] `dir` that are new or replaced since [`Self::take`].
    #[must_use]
    pub fn published(&self, dir: &Path) -> Vec<PathBuf> {
        files_under(dir)
            .into_iter()
            .filter(|path| {
                self.files
                    .get(path)
                    .is_none_or(|before| FileStamp::of(path).as_ref() != Some(before))
            })
            .collect()
    }
}

fn is_record_or_temporary(path: &Path) -> bool {
    let record = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "provenance.json" || name.ends_with(".provenance.json"));
    is_temporary(path)
        || record
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".cytiq-lock"))
}

/// Staged outputs (`cytiq-publish`) are `.tmp` files or directories until
/// they are renamed into place; backups of replaced outputs, which a crash
/// can leave behind, end in `.old.tmp`.
fn is_temporary(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
}

/// Size and lowercase hex SHA-256 of a file, streamed.
///
/// # Errors
///
/// Returns an error when the file cannot be read.
pub fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    let mut bytes = 0;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    let sha256 = hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    Ok((bytes, sha256))
}

fn write_record(path: &Path, record: &RunRecord) -> Result<(), ProvenanceError> {
    debug!("writing provenance record to {}", path.display());
    let write_error = |source| ProvenanceError::Write {
        path: path.to_path_buf(),
        source,
    };
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|error| write_error(error.into()))?;
    bytes.push(b'\n');
    cytiq_publish::write_file(path, Existing::Replace, |writer| {
        writer
            .write_all(&bytes)
            .map_err(|source| PublishError::io(path, source))
    })
    .map_err(|error| match error {
        PublishError::Io { source, .. } => write_error(source),
        other => write_error(io::Error::other(other)),
    })
}

/// Plain option structs always serialize; anything else is recorded as its
/// error rather than failing the run.
fn to_value(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|error| serde_json::Value::String(error.to_string()))
}

/// `path` resolved, so `./a` and `a` name the same file.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("cytiq-provenance-{}-{name}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn staged_records_hash_staged_bytes_but_name_final_outputs() {
        let directory = temp_dir("staged-record");
        let input = directory.join("input");
        fs::write(&input, b"input").unwrap();
        let staged = directory.join("staged.tmp");
        fs::write(&staged, b"abc").unwrap();
        let target = directory.join("not-yet-published.json");
        let mut recorder = RunRecorder::start("test", "1", vec![]);
        recorder
            .snapshot_inputs("input", std::slice::from_ref(&input))
            .unwrap();
        let record = recorder
            .prepare_staged(
                &[(target.clone(), staged)],
                crate::outcome::OutcomeReason::Success,
            )
            .unwrap();
        assert_eq!(record.schema_version, SCHEMA_VERSION);
        assert_eq!(record.outputs[0].path, target.display().to_string());
        assert_eq!(record.outputs[0].sha256, ABC_SHA256);
        assert!(!target.exists());
    }

    #[test]
    fn snapshots_list_only_files_a_run_published() {
        let directory = temp_dir("snapshot");
        fs::write(directory.join("left.csv"), "earlier run").unwrap();
        fs::write(directory.join("replaced.csv"), "same").unwrap();
        let snapshot = DirSnapshot::take(&directory);
        // Publication renames a new file into place, even for equal bytes.
        fs::write(directory.join("replaced.csv.tmp"), "same").unwrap();
        fs::rename(
            directory.join("replaced.csv.tmp"),
            directory.join("replaced.csv"),
        )
        .unwrap();
        fs::write(directory.join("new.csv"), "this run").unwrap();
        assert_eq!(
            snapshot.published(&directory),
            [directory.join("new.csv"), directory.join("replaced.csv")]
        );
        assert!(
            DirSnapshot::take(&directory.join("absent"))
                .files
                .is_empty()
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn files_under_skips_records_and_staged_outputs() {
        let directory = temp_dir("files-under");
        fs::create_dir_all(directory.join("nested")).unwrap();
        fs::create_dir_all(directory.join(".model.a1b2c3.tmp")).unwrap();
        fs::create_dir_all(directory.join(".model.d4e5f6.old.tmp/previous")).unwrap();
        for name in [
            "data.csv",
            "nested/part.parquet",
            "provenance.json",
            "out.fcs.provenance.json",
            ".data.csv.x9y8z7.tmp",
            ".model.a1b2c3.tmp/model.json",
            ".data.csv.g7h8i9.old.tmp",
            ".model.d4e5f6.old.tmp/previous/a.json",
        ] {
            fs::write(directory.join(name), name).unwrap();
        }
        assert_eq!(
            files_under(&directory),
            [
                directory.join("data.csv"),
                directory.join("nested/part.parquet")
            ]
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn hashes_empty_small_and_multibuffer_files_with_sizes() {
        let directory = temp_dir("hash");
        let large = (0_u8..=250).cycle().take(20_003).collect::<Vec<_>>();
        for (name, data, expected) in [
            (
                "empty",
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            ("small", b"abc".as_slice(), ABC_SHA256),
            (
                "large",
                large.as_slice(),
                "ee00de5b1eb73f0d3b7b22862029ded78a3f153910ce1b6f2e4862c04773adf8",
            ),
        ] {
            let path = directory.join(name);
            fs::write(&path, data).unwrap();
            let (bytes, sha256) = hash_file(&path).unwrap();
            assert_eq!(bytes, data.len() as u64, "{name}");
            assert_eq!(sha256, expected, "{name}");
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn records_inputs_and_outputs_with_hashes() {
        let directory = temp_dir("record");
        let input = directory.join("in.txt");
        let output = directory.join("out.txt");
        fs::write(&input, b"abc").unwrap();
        let record_path = record_path_for_file(&output);

        let mut recorder = RunRecorder::start("cytiq test", "0.0.0", vec!["--input".to_owned()]);
        recorder
            .snapshot_inputs("input", &[input, directory.join("missing.txt")])
            .unwrap();
        fs::write(&output, b"").unwrap();
        let record = recorder
            .finish(
                &[output],
                &record_path,
                crate::outcome::OutcomeReason::Success,
            )
            .unwrap();

        assert_eq!(record.inputs.len(), 1, "missing paths are skipped");
        assert_eq!(record.inputs[0].sha256, ABC_SHA256);
        assert_eq!(record.outputs[0].bytes, 0);
        assert!(record.finished_at_unix_ms >= record.started_at_unix_ms);
        let written: RunRecord =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(written, record);
        assert!(record_path.ends_with("out.txt.provenance.json"));
    }

    #[test]
    fn hashes_inputs_before_they_change() {
        let directory = temp_dir("in-place");
        let file = directory.join("sample.fcs");
        fs::write(&file, b"abc").unwrap();

        let mut recorder = RunRecorder::start("cytiq test", "0.0.0", Vec::new());
        recorder
            .snapshot_inputs("input", std::slice::from_ref(&file))
            .unwrap();
        // The command replaces its input in place.
        fs::write(&file, b"abcdef").unwrap();
        let record = recorder
            .finish(
                std::slice::from_ref(&file),
                &record_path_for_file(&file),
                crate::outcome::OutcomeReason::Success,
            )
            .unwrap();

        assert_eq!(record.inputs[0].sha256, ABC_SHA256);
        assert_eq!(record.inputs[0].bytes, 3);
        assert_eq!(record.outputs[0].bytes, 6);
    }

    #[test]
    fn fails_when_an_input_changes_during_the_run() {
        let directory = temp_dir("changed");
        let input = directory.join("in.txt");
        let output = directory.join("out.txt");
        fs::write(&input, b"abc").unwrap();

        let mut recorder = RunRecorder::start("cytiq test", "0.0.0", Vec::new());
        recorder
            .snapshot_inputs("input", std::slice::from_ref(&input))
            .unwrap();
        fs::write(&input, b"abcd").unwrap();
        fs::write(&output, b"").unwrap();
        let record_path = record_path_for_file(&output);

        assert!(matches!(
            recorder.finish(
                &[output],
                &record_path,
                crate::outcome::OutcomeReason::Success
            ),
            Err(ProvenanceError::InputChanged { .. })
        ));
        assert!(!record_path.exists());
    }

    #[test]
    fn records_build_config_and_roles() {
        let directory = temp_dir("v2");
        let input = directory.join("in.txt");
        let output = directory.join("out.txt");
        fs::write(&input, b"abc").unwrap();
        let build = BuildInfo {
            git_commit: "0123abc".to_owned(),
            rustc: "rustc 1.99.1".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            profile: "release".to_owned(),
        };
        let mut recorder =
            RunRecorder::start("cytiq test", "0.0.0", Vec::new()).with_build(build.clone());
        recorder.set_config(&serde_json::json!({ "threads": 4 }));
        recorder.set_constants(&serde_json::json!({ "mad": 6.0 }));
        recorder.set_algorithms(&[("PeacoQC", "1.20.0")]);
        recorder
            .snapshot_inputs("mask", std::slice::from_ref(&input))
            .unwrap();
        fs::write(&output, b"").unwrap();
        let record = recorder
            .finish(
                std::slice::from_ref(&output),
                &record_path_for_file(&output),
                crate::outcome::OutcomeReason::Success,
            )
            .unwrap();

        assert_eq!(record.schema_version, SCHEMA_VERSION);
        assert_eq!(record.build, build);
        assert_eq!(record.platform.os, std::env::consts::OS);
        assert!(record.platform.cpus >= 1);
        assert_eq!(record.config["threads"], 4);
        assert_eq!(record.constants["mad"], 6.0);
        assert_eq!(record.algorithms["PeacoQC"], "1.20.0");
        assert_eq!(record.inputs[0].role.as_deref(), Some("mask"));
        assert_eq!(record.outputs[0].role, None);
        let json = fs::read_to_string(record_path_for_file(&output)).unwrap();
        assert!(!json.contains("\"role\": null"), "outputs carry no role");
    }
}
