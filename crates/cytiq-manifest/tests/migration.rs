//! Compatibility of run manifest versions: v1 and v2 documents keep loading
//! and serializing exactly as before, and migrate to the current version.

use std::path::{Path, PathBuf};

use cytiq_manifest::RunManifest;

const DOCUMENTS: &[&str] = &["v1.yaml", "v2.yaml", "v2.json", "cytof-v2.yaml"];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Parse without resolving paths, so serialization does not depend on the
/// checkout location.
fn parse(name: &str) -> RunManifest {
    let path = fixture(name);
    let text = std::fs::read_to_string(&path).unwrap();
    if Path::new(name).extension().is_some_and(|ext| ext == "json") {
        serde_json::from_str(&text).unwrap()
    } else {
        serde_saphyr::from_str(&text).unwrap()
    }
}

fn expected(name: &str) -> PathBuf {
    let stem = name.replace('.', "-");
    fixture(&format!("expected/{stem}.json"))
}

#[test]
fn earlier_versions_load_and_validate() {
    for name in DOCUMENTS {
        RunManifest::load(&fixture(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

/// The serialized form of every v1 and v2 document is pinned: additions to
/// the manifest must not change how earlier documents round-trip.
/// `CYTIQ_BLESS=1` rewrites the expectation when a change is intended.
#[test]
fn earlier_versions_serialize_unchanged() {
    for name in DOCUMENTS {
        let actual = serde_json::to_string_pretty(&parse(name)).unwrap() + "\n";
        let path = expected(name);
        if std::env::var_os("CYTIQ_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            continue;
        }
        let pinned = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(actual, pinned, "{name} serializes differently");
    }
}

#[test]
fn yaml_and_json_describe_the_same_document() {
    assert_eq!(parse("v2.yaml"), parse("v2.json"));
}
