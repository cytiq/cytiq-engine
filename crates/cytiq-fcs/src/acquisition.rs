//! Identity of the raw acquisition a dataset and its event masks come from.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::FcsError;
use crate::types::FcsText;

/// SHA-256 of the raw FCS file a derived file comes from. Every cytiq stage
/// that writes FCS records it, so masks from any stage can be matched to the
/// acquisition whose rows their `source_event_id`s count.
pub const ACQUISITION_KEYWORD: &str = "CYTIQ_ACQUISITION";

/// The repairs `cytiq repair` made to a file whose instrument wrote it against
/// the standard. DATA is untouched: such a file, with no processing recorded,
/// stands for its acquisition.
pub const REPAIR_KEYWORD: &str = "CYTIQ_REPAIR";

/// Whether a file is an acquisition that `cytiq repair` made readable: it
/// records both the repair and the acquisition, and no cytiq processing.
/// A subset of its rows, which `Original_ID` marks, is not one.
///
/// # Errors
///
/// Returns an error when a recorded signal state is unreadable.
pub fn is_repaired_acquisition(
    text: &FcsText,
    parameters: &[crate::types::Parameter],
) -> Result<bool, FcsError> {
    let state = crate::signal::SignalState::of(text)?;
    Ok(text.get(REPAIR_KEYWORD).is_some()
        && text.get(ACQUISITION_KEYWORD).is_some()
        && state.compensation.is_none()
        && state.unmixing.is_none()
        && state.transform.is_none()
        && state.normalization.is_none()
        && parameters
            .iter()
            .all(|parameter| parameter.name != crate::lineage::ORIGINAL_ID_PARAMETER))
}

/// The acquisition a dataset read from `path` belongs to: the recorded
/// `CYTIQ_ACQUISITION`, or the SHA-256 of `path` itself when the file is raw.
///
/// # Errors
///
/// Returns an error when the recorded value is not a lowercase hex SHA-256 or
/// the file cannot be read.
pub fn acquisition_id(path: &Path, text: &FcsText) -> Result<String, FcsError> {
    match text.get(ACQUISITION_KEYWORD) {
        Some(value) if is_sha256(value) => Ok(value.to_owned()),
        Some(value) => Err(FcsError::InvalidKeywordValue {
            key: ACQUISITION_KEYWORD.to_owned(),
            value: value.to_owned(),
        }),
        None => sha256_file(path).map_err(FcsError::Io),
    }
}

/// Record `id` as the acquisition of a file about to be written, replacing any
/// carried value.
pub fn set_acquisition(keywords: &mut Vec<(String, String)>, id: &str) {
    keywords.retain(|(key, _)| key != ACQUISITION_KEYWORD);
    keywords.push((ACQUISITION_KEYWORD.to_owned(), id.to_owned()));
}

/// Whether `value` is a lowercase hex SHA-256.
#[must_use]
pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_empty_small_and_multibuffer_files() {
        let directory =
            std::env::temp_dir().join(format!("cytiq-acquisition-hash-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let large = (0_u8..=250).cycle().take(20_003).collect::<Vec<_>>();
        for (name, data, expected) in [
            (
                "empty",
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "small",
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "large",
                large.as_slice(),
                "ee00de5b1eb73f0d3b7b22862029ded78a3f153910ce1b6f2e4862c04773adf8",
            ),
        ] {
            let path = directory.join(name);
            std::fs::write(&path, data).unwrap();
            assert_eq!(sha256_file(&path).unwrap(), expected, "{name}");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn set_acquisition_replaces_a_carried_value() {
        let mut keywords = vec![
            ("$CYT".to_owned(), "Aurora".to_owned()),
            (ACQUISITION_KEYWORD.to_owned(), "a".repeat(64)),
        ];
        set_acquisition(&mut keywords, &"b".repeat(64));
        assert_eq!(
            keywords,
            vec![
                ("$CYT".to_owned(), "Aurora".to_owned()),
                (ACQUISITION_KEYWORD.to_owned(), "b".repeat(64)),
            ]
        );
    }

    #[test]
    fn recognises_lowercase_sha256() {
        assert!(is_sha256(&"0f".repeat(32)));
        assert!(!is_sha256(&"0F".repeat(32)));
        assert!(!is_sha256("abc"));
    }
}
