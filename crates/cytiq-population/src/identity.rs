//! The three identities of a dataset artifact, kept apart so a dataset can
//! move without changing what it means:
//!
//! - **semantic**: what the artifact is, from contracts, roles, acquisition,
//!   parent identities and definition or content digests. It never includes a
//!   path, a timestamp or a build.
//! - **content**: the SHA-256 and size of the stored object, for integrity.
//!   It changes with the encoding (for example a Parquet writer upgrade).
//! - **execution**: the v3 provenance record of the run that produced the
//!   object and its fingerprint. It includes paths and is only referenced.
//!
//! Objects are located by [`ObjectKey`]s relative to the dataset root, never by
//! absolute paths or URIs, so no location, user name or credential is recorded.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::serialization;

/// Longest object key accepted, in bytes.
pub const OBJECT_KEY_BYTES_MAX: usize = 1024;

/// Domain tag of [`semantic_id`]; changing the computation changes the tag.
pub const SEMANTIC_IDENTITY: &str = "cytiq.semantic.v1";

/// A location relative to the dataset root: `/`-separated, without `.` or
/// `..` segments, schemes, drive letters, queries, fragments or user-info.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// # Errors
    /// Returns why `key` is not a relative object key.
    pub fn parse(key: &str) -> Result<Self, &'static str> {
        match object_key_problem(key) {
            Some(problem) => Err(problem),
            None => Ok(Self(key.to_owned())),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file this key names under `root`.
    #[must_use]
    pub fn resolve(&self, root: &Path) -> PathBuf {
        self.0
            .split('/')
            .fold(root.to_path_buf(), |path, segment| path.join(segment))
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ObjectKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::parse(&key).map_err(serde::de::Error::custom)
    }
}

/// Why `key` is not a relative object key, or `None` when it is one.
#[must_use]
pub fn object_key_problem(key: &str) -> Option<&'static str> {
    if key.is_empty() {
        return Some("object key is empty");
    }
    if key.len() > OBJECT_KEY_BYTES_MAX {
        return Some("object key is too long");
    }
    if key.chars().any(char::is_control) {
        return Some("object key contains control characters");
    }
    if key.starts_with('/') || key.contains('\\') {
        return Some("object key must be a relative `/`-separated path");
    }
    let first = key.split('/').next().unwrap_or_default();
    if first.contains(':') {
        return Some("object key must not carry a scheme or drive");
    }
    if key.contains(['?', '#', '@']) {
        return Some("object key must not carry a query, fragment or user-info");
    }
    if key
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Some("object key has an empty, `.` or `..` segment");
    }
    None
}

/// Bytes and digest of a stored object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentRef {
    pub sha256: String,
    pub bytes: u64,
    /// Encoding-independent digest of an events table's keys and values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_sha256: Option<String>,
}

/// The run that produced an object: its provenance record (an object of the
/// dataset when disclosed) and the record's execution fingerprint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// Path-free identity of an artifact: the digest of its contract, role,
/// acquisition, parents (as a set) and `basis`, which is a definition or
/// content digest, never a location.
#[must_use]
pub fn semantic_id(
    contract: &str,
    role: &str,
    acquisition: Option<&str>,
    parents: &[String],
    basis: &str,
) -> String {
    let mut parents: Vec<&str> = parents.iter().map(String::as_str).collect();
    parents.sort_unstable();
    parents.dedup();
    serialization::digest(&json!({
        "identity": SEMANTIC_IDENTITY,
        "contract": contract,
        "role": role,
        "acquisition": acquisition,
        "parents": parents,
        "basis": basis,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_keys_are_relative_and_carry_no_location() {
        for key in ["events/M/S01.raw.parquet", "a", "a.b/c-d_e f"] {
            assert!(ObjectKey::parse(key).is_ok(), "{key}");
        }
        for key in [
            "",
            "/abs/file.parquet",
            "C:/data/file.parquet",
            "s3://bucket/key",
            "https://user:secret@host/key",
            "a/../b",
            "./a",
            "a//b",
            "a/",
            "a\\b",
            "a?b",
            "a#b",
            "user@host/a",
            "a\nb",
        ] {
            assert!(ObjectKey::parse(key).is_err(), "{key:?}");
        }
        assert!(ObjectKey::parse(&"a".repeat(OBJECT_KEY_BYTES_MAX + 1)).is_err());
    }

    #[test]
    fn keys_resolve_under_the_root() {
        let key = ObjectKey::parse("events/M/x.parquet").unwrap();
        assert_eq!(
            key.resolve(Path::new("/root")),
            Path::new("/root")
                .join("events")
                .join("M")
                .join("x.parquet")
        );
    }

    #[test]
    fn keys_are_validated_when_deserialized() {
        assert!(serde_json::from_str::<ObjectKey>("\"a/b\"").is_ok());
        assert!(serde_json::from_str::<ObjectKey>("\"/a\"").is_err());
    }

    #[test]
    fn semantic_ids_ignore_parent_order_and_depend_on_every_component() {
        let parents = ["b".repeat(64), "a".repeat(64)];
        let base = semantic_id("c", "r", Some("x"), &parents, "basis");
        let reversed = [parents[1].clone(), parents[0].clone()];
        assert_eq!(base, semantic_id("c", "r", Some("x"), &reversed, "basis"));
        assert_ne!(base, semantic_id("c2", "r", Some("x"), &parents, "basis"));
        assert_ne!(base, semantic_id("c", "r2", Some("x"), &parents, "basis"));
        assert_ne!(base, semantic_id("c", "r", Some("y"), &parents, "basis"));
        assert_ne!(base, semantic_id("c", "r", None, &parents, "basis"));
        assert_ne!(
            base,
            semantic_id("c", "r", Some("x"), &parents[..1], "basis")
        );
        assert_ne!(base, semantic_id("c", "r", Some("x"), &parents, "other"));
    }
}
