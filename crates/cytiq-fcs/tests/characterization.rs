//! Pins what the reader produces for every FCS fixture in the repository, so
//! reader refactors can prove they change nothing. Run with `CYTIQ_BLESS=1`
//! to rewrite `data/fixtures/characterization/fcs.json` after an intended
//! change.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use cytiq_fcs::{ColumnData, FcsDataset, FcsReader};
use sha2::{Digest, Sha256};

#[test]
fn reader_output_matches_characterization_digests() {
    let root = repository_root();
    let fixtures = root.join("data/fixtures");
    let mut paths = Vec::new();
    collect_fcs_files(&fixtures, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "no FCS fixtures under {fixtures:?}");

    let actual = paths
        .iter()
        .map(|path| {
            let name = path
                .strip_prefix(&root)
                .expect("fixture is under the repository root")
                .to_string_lossy()
                .replace('\\', "/");
            (name, digest_read(path))
        })
        .collect::<BTreeMap<_, _>>();

    let golden = root.join("data/fixtures/characterization/fcs.json");
    if std::env::var_os("CYTIQ_BLESS").is_some() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CYTIQ_BLESS rewrites a reviewed golden; CI never regenerates goldens"
        );
        fs::create_dir_all(golden.parent().expect("golden has a parent"))
            .expect("create characterization directory");
        let mut json = serde_json::to_string_pretty(&actual).expect("serialize digests");
        json.push('\n');
        fs::write(&golden, json).expect("write golden digests");
        eprintln!("rewrote {golden:?}; review `git diff` before committing it");
    }

    let expected: BTreeMap<String, String> = serde_json::from_str(
        &fs::read_to_string(&golden)
            .unwrap_or_else(|error| panic!("read {golden:?} (bless with CYTIQ_BLESS=1): {error}")),
    )
    .expect("parse golden digests");
    assert_eq!(
        actual, expected,
        "reader output changed; if intended, rerun with CYTIQ_BLESS=1"
    );
}

/// Writes the same digests for every FCS under `$CYTIQ_REAL_DATA` to
/// `$CYTIQ_DIGESTS_OUT`, so a reader change can be compared on local data that
/// cannot be committed.
#[test]
#[ignore = "needs CYTIQ_REAL_DATA and CYTIQ_DIGESTS_OUT"]
fn writes_real_data_digests() {
    let input = PathBuf::from(std::env::var_os("CYTIQ_REAL_DATA").expect("set CYTIQ_REAL_DATA"));
    let output =
        PathBuf::from(std::env::var_os("CYTIQ_DIGESTS_OUT").expect("set CYTIQ_DIGESTS_OUT"));
    let mut paths = Vec::new();
    collect_fcs_files(&input, &mut paths);
    paths.sort();
    let digests = paths
        .iter()
        .map(|path| (path.to_string_lossy().into_owned(), digest_read(path)))
        .collect::<BTreeMap<_, _>>();
    let mut json = serde_json::to_string_pretty(&digests).expect("serialize digests");
    json.push('\n');
    fs::write(output, json).expect("write digests");
}

fn digest_read(path: &Path) -> String {
    match FcsReader::open(path).and_then(FcsReader::read) {
        Ok(dataset) => hex(&Sha256::digest(canonical_bytes(&dataset))),
        Err(error) => format!("error: {error}"),
    }
}

fn canonical_bytes(dataset: &FcsDataset) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_field(&mut bytes, dataset.version.as_str());
    push_field(&mut bytes, &format!("{:?}", dataset.header));
    push_field(&mut bytes, &dataset.text.primary.delimiter.to_string());
    for (key, value) in dataset.text.iter() {
        push_field(&mut bytes, key);
        push_field(&mut bytes, value);
    }
    for parameter in &dataset.parameters {
        push_field(&mut bytes, &format!("{parameter:?}"));
    }
    push_field(&mut bytes, &format!("{:?}", dataset.analysis));
    for column in &dataset.data.columns {
        match column {
            ColumnData::UInt(values) => {
                push_field(&mut bytes, &format!("u64:{}", values.len()));
                bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
            }
            ColumnData::F32(values) => {
                push_field(&mut bytes, &format!("f32:{}", values.len()));
                bytes.extend(
                    values
                        .iter()
                        .flat_map(|value| value.to_bits().to_le_bytes()),
                );
            }
            ColumnData::F64(values) => {
                push_field(&mut bytes, &format!("f64:{}", values.len()));
                bytes.extend(
                    values
                        .iter()
                        .flat_map(|value| value.to_bits().to_le_bytes()),
                );
            }
        }
    }
    bytes
}

fn push_field(bytes: &mut Vec<u8>, field: &str) {
    bytes.extend(field.as_bytes());
    bytes.push(0);
}

fn collect_fcs_files(directory: &Path, paths: &mut Vec<PathBuf>) {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).expect("read fixture directory") {
            let path = entry.expect("read fixture entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("fcs"))
            {
                paths.push(path);
            }
        }
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root exists")
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
