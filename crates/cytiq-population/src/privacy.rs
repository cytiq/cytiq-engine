//! What a dataset may export from an FCS file.
//!
//! FCS TEXT carries file names, operators, dates, sample numbers, comments
//! and vendor keywords that can identify a patient, a person or a site. A
//! dataset exports only the acquisition-physics and channel keywords listed
//! here. Anything else needs a [`Disclosure`]: an explicit contract that names
//! its purpose and every additional keyword. Spillover matrices and `CYTIQ_*`
//! signal keywords are never exported verbatim (they can hold paths); the
//! events contract records them as digests and descriptors instead.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::issue::{IssueCode, Issues, pointer};

/// Dataset-level keywords exported by default: acquisition mode, layout,
/// counts and flow, nothing about who, where or when.
pub const ALLOWED_KEYWORDS: &[&str] = &[
    "$CYT",
    "$MODE",
    "$DATATYPE",
    "$BYTEORD",
    "$PAR",
    "$TOT",
    "$TIMESTEP",
    "$VOL",
    "$FLOWRATE",
    "$ABRT",
    "$LOST",
    "$ORIGINALITY",
    "$UNICODE",
    "CYTIQ_ACQUISITION",
];

/// `$Pn` keyword suffixes exported by default: channel name, stain, layout,
/// range, gain, voltage, excitation, power, emitted light, filter, detector
/// type, display, data type and the FCS 3.2 detector, tag, analyte,
/// feature, type and calibration.
pub const ALLOWED_PARAMETER_SUFFIXES: &[&str] = &[
    "N",
    "S",
    "B",
    "E",
    "R",
    "G",
    "V",
    "L",
    "O",
    "P",
    "F",
    "T",
    "D",
    "DATATYPE",
    "DET",
    "TAG",
    "ANALYTE",
    "FEATURE",
    "TYPE",
    "CALIBRATION",
];

/// Keywords whose content the events contract records structurally (as a
/// digest or a descriptor). They are never exported verbatim, not even under
/// a disclosure, because their values can hold file paths.
pub const STRUCTURAL_KEYWORDS: &[&str] = &["$SPILLOVER", "$SPILL", "SPILL", "$COMP"];

/// Prefix of cytiq's own processing keywords, exported only structurally.
pub const CYTIQ_PREFIX: &str = "CYTIQ_";

/// Whether `key` is exported without a disclosure. FCS keywords are
/// case-insensitive.
#[must_use]
pub fn is_allowed(key: &str) -> bool {
    ALLOWED_KEYWORDS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(key))
        || parameter_suffix(key).is_some_and(|suffix| {
            ALLOWED_PARAMETER_SUFFIXES
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(suffix))
        })
}

/// Whether `key` may never be exported verbatim.
#[must_use]
pub fn is_structural(key: &str) -> bool {
    STRUCTURAL_KEYWORDS
        .iter()
        .any(|structural| structural.eq_ignore_ascii_case(key))
        || (key
            .get(..CYTIQ_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(CYTIQ_PREFIX))
            && !key.eq_ignore_ascii_case("CYTIQ_ACQUISITION"))
}

/// The suffix of a `$P<n><suffix>` keyword.
fn parameter_suffix(key: &str) -> Option<&str> {
    let rest = key.get(..2).filter(|p| p.eq_ignore_ascii_case("$P"))?;
    let rest = &key[rest.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0 && rest.as_bytes()[0] != b'0').then(|| &rest[digits..])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyProfile {
    /// Only the allowlist; no disclosures.
    #[default]
    Deidentified,
    /// The allowlist plus what each disclosure names.
    Controlled,
}

/// An explicit agreement to export more than the allowlist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disclosure {
    /// Identifier of the agreement, for example a data use agreement number.
    pub contract: String,
    pub purpose: String,
    /// Additional FCS keywords.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Source FCS files may be listed as dataset objects.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_locations: bool,
    /// Provenance records (which contain paths and command lines) may be
    /// listed as dataset objects.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provenance_records: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyPolicy {
    pub profile: PrivacyProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disclosures: Vec<Disclosure>,
}

impl PrivacyPolicy {
    /// The allowlist only.
    #[must_use]
    pub fn deidentified() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn discloses_keyword(&self, key: &str) -> bool {
        self.disclosures.iter().any(|disclosure| {
            disclosure
                .keywords
                .iter()
                .any(|disclosed| disclosed.eq_ignore_ascii_case(key))
        })
    }

    #[must_use]
    pub fn discloses_source_locations(&self) -> bool {
        self.disclosures.iter().any(|d| d.source_locations)
    }

    #[must_use]
    pub fn discloses_provenance_records(&self) -> bool {
        self.disclosures.iter().any(|d| d.provenance_records)
    }

    /// Whether `key` may be exported under this policy.
    #[must_use]
    pub fn exports(&self, key: &str) -> bool {
        !is_structural(key) && (is_allowed(key) || self.discloses_keyword(key))
    }

    /// The exportable subset of `keywords`, keyed as written.
    pub fn export<'a>(
        &self,
        keywords: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> BTreeMap<String, String> {
        keywords
            .into_iter()
            .filter(|(key, _)| self.exports(key))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }

    /// Report a policy that is not an explicit contract: a deidentified
    /// profile with disclosures, a controlled one without, a disclosure
    /// without contract or purpose, or one naming a structural keyword.
    pub fn check(&self, at: &str, issues: &mut Issues) {
        match (self.profile, self.disclosures.is_empty()) {
            (PrivacyProfile::Deidentified, false) => issues.error(
                IssueCode::PrivacyViolation,
                format!("{at}/disclosures"),
                "a deidentified dataset has no disclosures; use `profile: controlled`",
            ),
            (PrivacyProfile::Controlled, true) => issues.error(
                IssueCode::PrivacyViolation,
                format!("{at}/profile"),
                "a controlled dataset names its disclosures",
            ),
            _ => {}
        }
        for (index, disclosure) in self.disclosures.iter().enumerate() {
            let here = format!("{at}{}", pointer(["disclosures", &index.to_string()]));
            if disclosure.contract.trim().is_empty() || disclosure.purpose.trim().is_empty() {
                issues.error(
                    IssueCode::PrivacyViolation,
                    here.clone(),
                    "a disclosure names its contract and purpose",
                );
            }
            for (k, key) in disclosure.keywords.iter().enumerate() {
                let at_key = format!("{here}{}", pointer(["keywords", &k.to_string()]));
                if key.trim().is_empty() || key.chars().any(char::is_control) {
                    issues.error(IssueCode::PrivacyViolation, at_key, "invalid FCS keyword");
                } else if is_structural(key) {
                    issues.error(
                        IssueCode::PrivacyViolation,
                        at_key,
                        format!("`{key}` is exported only as a digest or descriptor"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_and_acquisition_keywords_are_allowed() {
        for key in [
            "$CYT",
            "$tot",
            "$P1N",
            "$p12s",
            "$P3DATATYPE",
            "$P4TAG",
            "$P5ANALYTE",
            "$P6DET",
            "$P7FEATURE",
            "$P8TYPE",
            "$P9V",
            "CYTIQ_ACQUISITION",
        ] {
            assert!(is_allowed(key), "{key}");
        }
    }

    #[test]
    fn identifying_and_vendor_keywords_are_denied() {
        for key in [
            "$FIL",
            "$SMNO",
            "$OP",
            "$SRC",
            "$INST",
            "$COM",
            "$PROJ",
            "$EXP",
            "$DATE",
            "$BTIM",
            "$ETIM",
            "$LAST_MODIFIER",
            "$CYTSN",
            "$PLATEID",
            "$WELLID",
            "$CELLS",
            "$SYS",
            "@CYTOMETERID",
            "FILENAME",
            "$P",
            "$PN",
            "$P0N",
            "$P1",
            "$P1X",
            "$PAR2",
            "é",
            "$Pé",
        ] {
            assert!(!is_allowed(key), "{key}");
        }
    }

    #[test]
    fn structural_keywords_are_never_exported() {
        let policy = PrivacyPolicy {
            profile: PrivacyProfile::Controlled,
            disclosures: vec![Disclosure {
                contract: "DUA-1".into(),
                purpose: "QC".into(),
                keywords: vec![
                    "$SPILLOVER".into(),
                    "CYTIQ_COMPENSATION".into(),
                    "$CYTSN".into(),
                ],
                source_locations: false,
                provenance_records: false,
            }],
        };
        assert!(!policy.exports("$SPILLOVER"));
        assert!(!policy.exports("cytiq_compensation"));
        assert!(policy.exports("$cytsn"));
        assert!(policy.exports("CYTIQ_ACQUISITION"));
        let mut issues = Issues::new();
        policy.check("/privacy", &mut issues);
        assert_eq!(issues.errors(), 2, "{:?}", issues.items());
    }

    #[test]
    fn export_keeps_only_allowed_keywords() {
        let exported = PrivacyPolicy::deidentified().export([
            ("$CYT", "Navios"),
            ("$FIL", "Jane_Doe_01.fcs"),
            ("$P1N", "FS INT"),
            ("$DATE", "01-JAN-2024"),
            ("CYTIQ_COMPENSATION", "{\"source\":\"/home/user/m.csv\"}"),
        ]);
        assert_eq!(
            exported.keys().map(String::as_str).collect::<Vec<_>>(),
            ["$CYT", "$P1N"]
        );
    }

    #[test]
    fn policies_must_be_explicit() {
        let mut issues = Issues::new();
        PrivacyPolicy {
            profile: PrivacyProfile::Controlled,
            disclosures: vec![],
        }
        .check("/privacy", &mut issues);
        PrivacyPolicy {
            profile: PrivacyProfile::Deidentified,
            disclosures: vec![Disclosure {
                contract: " ".into(),
                purpose: String::new(),
                keywords: vec![],
                source_locations: true,
                provenance_records: false,
            }],
        }
        .check("/privacy", &mut issues);
        assert_eq!(issues.errors(), 3, "{:?}", issues.items());
    }
}
