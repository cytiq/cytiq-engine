//! Which keywords may be repeated in TEXT without making it ambiguous.
//!
//! FCS 3.1 (2.2.5): "Keywords are unique in data sets, i.e., there are no
//! multiple instances of the same keyword in the data set." A file that repeats
//! one has no value for it. When the standard or cytiq gives the keyword a
//! meaning, the file is refused. When nothing reads it, the keyword is
//! quarantined: its pairs stay in TEXT as written, and no lookup finds any of
//! them, neither the first nor the last.

/// Keywords outside the standard that cytiq reads, in upper case. Lookups that
/// ignore the `$` of a standard keyword are why `CYT` is here.
const NAMES: [&str; 9] = [
    "SPILL",
    "SPILLOVER",
    "TRANSFORMATION",
    "MINRANGE",
    "MAXRANGE",
    "MIN",
    "MAX",
    "CYT",
    "ORIGINALINSTRUMENT",
];

/// Families of keywords outside the standard that cytiq reads.
const PREFIXES: [&str; 2] = ["CYTIQ_", "FLOWCORE_"];

/// Whether cytiq, or the standard, gives `key` a meaning: such a keyword must
/// not be repeated. Case and surrounding space are ignored.
#[must_use]
pub fn is_engine_keyword(key: &str) -> bool {
    let key = key.trim().to_ascii_uppercase();
    // Only the standard defines keywords that start with `$` (3.2.12).
    key.starts_with('$')
        || NAMES.contains(&key.as_str())
        || PREFIXES.iter().any(|prefix| key.starts_with(prefix))
        || is_parameter_keyword_without_dollar(&key)
}

/// `P1G`: a `$Pn` keyword as lookups that ignore the `$` see it.
fn is_parameter_keyword_without_dollar(key: &str) -> bool {
    key.strip_prefix('P').is_some_and(|rest| {
        let suffix = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        suffix.len() < rest.len()
            && !suffix.is_empty()
            && suffix.bytes().all(|byte| byte.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_and_engine_keywords_are_recognized_in_any_case() {
        for key in [
            "$TOT",
            "$anything",
            "spill",
            "Spillover",
            "cytiq_x",
            "flowCore_$P1Rmin",
            "transformation",
            "minRange",
            "maxRange",
            "min",
            "max",
            "cyt",
            "OriginalInstrument",
            "P1G",
            "p12calibration",
            " SPILL ",
        ] {
            assert!(is_engine_keyword(key), "{key}");
        }
        for key in [
            "NA", "FJ_X", "P", "P1", "PG", "P1G2", "CREATOR", "SPILLED", "", "#NCx",
        ] {
            assert!(!is_engine_keyword(key), "{key}");
        }
    }
}
