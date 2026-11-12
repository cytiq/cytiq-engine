//! Read-only assessment and policy decisions for recorded destinations.
use crate::{RunRecord, hash_file, record_path_for_dir, record_path_for_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    #[default]
    Recompute,
    Skip,
    Reject,
}

#[derive(Clone, Debug)]
pub enum Destination {
    File(PathBuf),
    Dir(PathBuf),
}
impl Destination {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::File(p) | Self::Dir(p) => p,
        }
    }
    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        match self {
            Self::File(p) => record_path_for_file(p),
            Self::Dir(p) => record_path_for_dir(p),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Prior {
    Absent,
    Match(Box<RunRecord>),
    Stale { why: String },
    Damaged,
    Unrecorded,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Reuse,
    Compute,
    Conflict,
}

#[must_use]
pub fn decide(policy: ReusePolicy, prior: &Prior) -> Decision {
    match (policy, prior) {
        (ReusePolicy::Recompute, _) | (_, Prior::Absent) => Decision::Compute,
        (_, Prior::Match(_)) => Decision::Reuse,
        (ReusePolicy::Skip, _) => Decision::Compute,
        (ReusePolicy::Reject, _) => Decision::Conflict,
    }
}

/// Verify every recorded output, including its size; an empty output set is incomplete.
#[must_use]
pub fn verify_outputs(record: &RunRecord) -> bool {
    !record.outputs.is_empty()
        && record.outputs.iter().all(|output| {
            hash_file(Path::new(&output.path))
                .is_ok_and(|(bytes, sha)| bytes == output.bytes && sha == output.sha256)
        })
}

/// Assess without modifying the destination. Call under its output lock.
#[must_use]
pub fn assess(destination: &Destination, expected: &RunRecord) -> Prior {
    let path = destination.record_path();
    if !destination.path().exists() && !path.exists() {
        return Prior::Absent;
    }
    // A record is our own small JSON; an oversized one is not trusted.
    let bytes = match crate::input::document_bytes(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Prior::Unrecorded,
        Err(_) => return Prior::Damaged,
    };
    let Ok(record) = serde_json::from_slice::<RunRecord>(&bytes) else {
        return Prior::Damaged;
    };
    let stale = |why: &str| Prior::Stale { why: why.into() };
    if record.schema_version != crate::SCHEMA_VERSION {
        return stale("provenance version");
    }
    if !crate::fingerprint::reusable_build(&record.build.git_commit) {
        return stale("unidentified build");
    }
    if record.execution_fingerprint.as_deref() != Some(record.compute_fingerprint().as_str()) {
        return Prior::Damaged;
    }
    if record.reason.is_none_or(|r| {
        !matches!(
            r,
            crate::outcome::OutcomeReason::Success
                | crate::outcome::OutcomeReason::ChecksFailed
                | crate::outcome::OutcomeReason::Incomplete
        )
    }) {
        return Prior::Damaged;
    }
    if record.tool != expected.tool {
        return stale("tool");
    }
    if record.version != expected.version {
        return stale("version");
    }
    if record.build != expected.build {
        return stale("build");
    }
    if crate::fingerprint::semantic_config(record.config.clone())
        != crate::fingerprint::semantic_config(expected.config.clone())
    {
        return stale("configuration");
    }
    if record.constants != expected.constants {
        return stale("constants");
    }
    if record.algorithms != expected.algorithms {
        return stale("algorithms");
    }
    if record.inputs != expected.inputs {
        return stale("inputs");
    }
    if record.parallelism != expected.parallelism {
        return stale("parallelism");
    }
    if record.execution_fingerprint.as_deref() != Some(expected.compute_fingerprint().as_str()) {
        return stale("fingerprint");
    }
    if !verify_outputs(&record) {
        return Prior::Damaged;
    }
    let recorded: std::collections::BTreeSet<_> = record
        .outputs
        .iter()
        .map(|f| canonical(Path::new(&f.path)))
        .collect();
    match destination {
        Destination::File(target) if !recorded.contains(&canonical(target)) => {
            return Prior::Damaged;
        }
        Destination::Dir(directory) => {
            let actual: std::collections::BTreeSet<_> = crate::files_under(directory)
                .iter()
                .filter(|p| p.file_name().is_none_or(|n| n != "outcome.json"))
                .map(|p| canonical(p))
                .collect();
            if actual != recorded {
                return Prior::Damaged;
            }
        }
        Destination::File(_) => {}
    }
    Prior::Match(Box::new(record))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
