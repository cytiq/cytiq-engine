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

#[test]
fn migration_raises_the_version_and_keeps_the_document() {
    for name in DOCUMENTS {
        let original = RunManifest::load(&fixture(name)).unwrap();
        let migrated = original
            .clone()
            .migrate(&fixture(name))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(migrated.schema_version, cytiq_manifest::SCHEMA_VERSION);
        let mut relabelled = original;
        relabelled.schema_version = cytiq_manifest::SCHEMA_VERSION;
        assert_eq!(migrated, relabelled, "{name}: migration changed the run");
        let again = migrated.clone().migrate(&fixture(name)).unwrap();
        assert_eq!(again, migrated, "{name}: migration is not idempotent");
    }
}

#[test]
fn migration_applies_the_current_rules() {
    // v1 accepted an empty specimen id; v2 began to reject it.
    let text = std::fs::read_to_string(fixture("v1.yaml"))
        .unwrap()
        .replace(
            "{ id: S01, file: fcs/s01.fcs }",
            "{ id: S01, file: fcs/s01.fcs, specimen: \" \" }",
        );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("v1-empty-specimen.yaml");
    std::fs::write(&path, text).unwrap();
    let manifest = RunManifest::load(&path).unwrap();
    let error = manifest.migrate(&path).unwrap_err();
    assert!(error.to_string().contains("specimen ids"), "{error}");
}

#[test]
fn current_version_documents_load_from_yaml_and_json() {
    let yaml = RunManifest::load(&fixture("v3.yaml")).unwrap();
    assert_eq!(yaml.schema_version, 3);
    assert_eq!(parse("v3.yaml"), parse("v3.json"));
    let marker = &yaml.panels[0].markers[0];
    assert_eq!(marker.marker, "CD3");
    assert_eq!(marker.channel.as_deref(), Some("FITC-A"));
    assert_eq!(marker.reporter.as_deref(), Some("FITC"));
    assert_eq!(marker.detector.as_deref(), Some("B530"));
    // A v3 document round-trips through its own serialization.
    let json = serde_json::to_string(&parse("v3.yaml")).unwrap();
    assert_eq!(
        serde_json::from_str::<RunManifest>(&json).unwrap(),
        parse("v3.yaml")
    );
}

#[test]
fn unknown_versions_are_rejected() {
    for version in [0, cytiq_manifest::SCHEMA_VERSION + 1, 99] {
        let text = std::fs::read_to_string(fixture("v3.yaml"))
            .unwrap()
            .replace("schema_version: 3", &format!("schema_version: {version}"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("future.yaml");
        std::fs::write(&path, text).unwrap();
        let error = RunManifest::load(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported manifest schema_version"),
            "{error}"
        );
    }
}
