//! Typed validation issues: every rule reports a stable code, a severity and
//! the JSON pointer of what it concerns, and validation collects all of them
//! instead of stopping at the first.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// What went wrong; serialized in `snake_case` and stable across versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// Unknown manifest, contract, events or mask version.
    UnsupportedSchema,
    /// The document does not parse or has the wrong shape.
    MalformedDocument,
    /// A field the profile or a signal step needs is absent.
    MissingMetadata,
    /// Panels, channels, detectors, markers or reporters disagree.
    PanelMismatch,
    /// Signal processing steps that cannot be combined or compared.
    IncompatibleSignalState,
    /// One (acquisition, event) identity appears twice.
    DuplicateEventIdentity,
    /// One acquisition is listed twice or bound to two samples.
    DuplicateAcquisition,
    /// A sample, specimen, panel or instrument id repeats.
    DuplicateIdentifier,
    /// A reference that does not resolve, or resolves to another acquisition.
    InvalidReference,
    /// Channels out of physical order, duplicated or reserved.
    InconsistentChannelOrder,
    /// An object the manifest lists is absent.
    MissingObject,
    /// An object's bytes, size or decoded values differ from the manifest.
    CorruptArtifactHash,
    /// Something identifying is exported without an explicit contract.
    PrivacyViolation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub code: IssueCode,
    pub severity: Severity,
    /// RFC 6901 JSON pointer into the manifest (`""` for the whole document).
    pub pointer: String,
    pub message: String,
}

/// Collected issues, at most [`Issues::MAX`] kept; counts include the rest.
#[derive(Clone, Debug, Default)]
pub struct Issues {
    items: Vec<Issue>,
    errors: usize,
    warnings: usize,
}

impl Issues {
    /// Issues kept in a report; later ones are only counted.
    pub const MAX: usize = 1000;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn error(
        &mut self,
        code: IssueCode,
        pointer: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Issue {
            code,
            severity: Severity::Error,
            pointer: pointer.into(),
            message: message.into(),
        });
    }

    pub fn warning(
        &mut self,
        code: IssueCode,
        pointer: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(Issue {
            code,
            severity: Severity::Warning,
            pointer: pointer.into(),
            message: message.into(),
        });
    }

    pub fn push(&mut self, issue: Issue) {
        match issue.severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
        }
        if self.items.len() < Self::MAX {
            self.items.push(issue);
        }
    }

    #[must_use]
    pub fn errors(&self) -> usize {
        self.errors
    }

    #[must_use]
    pub fn warnings(&self) -> usize {
        self.warnings
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// More issues were found than kept.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.errors + self.warnings > self.items.len()
    }

    #[must_use]
    pub fn items(&self) -> &[Issue] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<Issue> {
        self.items
    }

    /// Codes of the kept issues of `severity`.
    #[must_use]
    pub fn codes(&self, severity: Severity) -> BTreeSet<IssueCode> {
        self.items
            .iter()
            .filter(|issue| issue.severity == severity)
            .map(|issue| issue.code)
            .collect()
    }
}

/// RFC 6901 pointer of `segments` (`~` and `/` escaped).
#[must_use]
pub fn pointer<I, S>(segments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    segments
        .into_iter()
        .fold(String::new(), |mut text, segment| {
            text.push('/');
            text.push_str(&segment.as_ref().replace('~', "~0").replace('/', "~1"));
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_escape_reserved_characters() {
        assert_eq!(pointer::<[&str; 0], &str>([]), "");
        assert_eq!(
            pointer(["artifacts", "3", "location"]),
            "/artifacts/3/location"
        );
        assert_eq!(pointer(["keywords", "a/b~c"]), "/keywords/a~1b~0c");
    }

    #[test]
    fn issues_are_capped_but_counted() {
        let mut issues = Issues::new();
        for index in 0..=Issues::MAX {
            issues.error(
                IssueCode::MissingObject,
                pointer(["a", &index.to_string()]),
                "x",
            );
        }
        issues.warning(IssueCode::MissingMetadata, "", "y");
        assert_eq!(issues.items().len(), Issues::MAX);
        assert_eq!(issues.errors(), Issues::MAX + 1);
        assert_eq!(issues.warnings(), 1);
        assert!(issues.truncated() && issues.has_errors());
        assert_eq!(
            issues.codes(Severity::Error),
            BTreeSet::from([IssueCode::MissingObject])
        );
        assert!(
            issues.codes(Severity::Warning).is_empty(),
            "dropped beyond the cap"
        );
    }

    #[test]
    fn codes_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&IssueCode::CorruptArtifactHash).unwrap(),
            "\"corrupt_artifact_hash\""
        );
    }
}
