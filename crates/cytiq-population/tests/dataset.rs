//! Dataset manifest v1: a two-panel dataset built from synthetic FCS files,
//! the identities it carries, and one test per validation code.

mod common;

use std::path::PathBuf;

use common::{Built, build};
use cytiq_population::dataset::{
    self, ArtifactEntry, ArtifactRole, DatasetManifest, Profile, ReportStatus, ValidateOptions,
    contracts,
};
use cytiq_population::issue::{IssueCode, Issues, Severity};
use cytiq_population::privacy::{Disclosure, PrivacyPolicy, PrivacyProfile};
use cytiq_population::serialization::DocumentFormat;

fn check(built: &Built, profile: Profile, objects: bool) -> Issues {
    let mut issues = Issues::new();
    dataset::validate_document(&built.manifest, profile, &mut issues);
    if objects {
        dataset::verify_objects(&built.manifest, built.root.path(), &mut issues);
    }
    issues
}

fn expect(built: &Built, objects: bool, code: IssueCode) {
    let issues = check(built, Profile::Structural, objects);
    assert!(
        issues.codes(Severity::Error).contains(&code),
        "expected {code:?}, got {:?}",
        issues.items()
    );
}

fn mutated(change: impl FnOnce(&mut DatasetManifest)) -> Built {
    let mut built = build();
    change(&mut built.manifest);
    built.manifest.dataset_id = None;
    built
}

#[test]
fn the_reference_dataset_is_valid_under_every_profile() {
    let built = build();
    for profile in [Profile::Structural, Profile::MiflowcytMinimal] {
        let issues = check(&built, profile, true);
        assert!(!issues.has_errors(), "{profile:?}: {:?}", issues.items());
    }
    let panel = &built.manifest.panels[0];
    assert_eq!(panel.channels[1].marker.as_deref(), Some("CD3"));
    assert_eq!(panel.channels[1].reporter.as_deref(), Some("FITC"));
    assert_eq!(panel.channels[1].detector.as_deref(), Some("B530"));
    assert_eq!(
        panel.channels[0].marker, None,
        "$PnS never becomes a marker"
    );
    let keys: Vec<&str> = built.manifest.acquisitions[0]
        .keywords
        .keys()
        .map(String::as_str)
        .collect();
    assert!(keys.contains(&"$CYT") && !keys.contains(&"$FIL") && !keys.contains(&"$DATE"));
}

#[test]
fn the_dataset_identity_survives_relocation_but_not_meaning() {
    let built = build();
    let id = built.manifest.dataset_id.clone().unwrap();
    let mut moved = built.manifest.clone();
    for artifact in &mut moved.artifacts {
        artifact.location = format!("elsewhere/{}", artifact.location);
        artifact.execution = Some(cytiq_population::identity::ExecutionRef {
            provenance: None,
            fingerprint: Some("f".repeat(64)),
        });
    }
    assert_eq!(moved.dataset_id(), id);
    let mut changed = built.manifest.clone();
    changed.panels[0].channels[1].reporter = Some("BV421".into());
    assert_ne!(changed.dataset_id(), id);
}

#[test]
fn acquisitions_may_share_row_numbers_but_never_identities() {
    let built = build();
    let layers: Vec<&ArtifactEntry> = built
        .manifest
        .artifacts
        .iter()
        .filter(|a| {
            a.role == ArtifactRole::SignalLayer && a.layer.as_ref().unwrap().steps.is_empty()
        })
        .collect();
    assert_eq!(layers.len(), 2);
    assert_ne!(layers[0].acquisition, layers[1].acquisition);
    assert_ne!(layers[0].semantic_id, layers[1].semantic_id);
    // Both raw tables hold source_event_id 0..5; nothing is a duplicate.
    let issues = check(&built, Profile::Structural, true);
    assert!(
        !issues
            .codes(Severity::Error)
            .contains(&IssueCode::DuplicateEventIdentity)
    );
    // A layer claiming another acquisition's events is caught in its rows.
    let built = mutated(|m| {
        let other = m.acquisitions[1].id.clone();
        m.artifacts[0].acquisition = Some(other);
        m.artifacts[0].panel = Some("B".into());
    });
    expect(&built, true, IssueCode::InvalidReference);
}

#[test]
fn versions_and_documents_are_checked_before_their_content() {
    let built = build();
    let text = built.manifest.canonical_json();
    let mut issues = Issues::new();
    assert!(dataset::parse(text.as_bytes(), DocumentFormat::Json, &mut issues).is_some());
    assert_eq!(issues.items().len(), 0);
    for (from, to, code) in [
        (
            "\"schema_version\": 1",
            "\"schema_version\": 2",
            IssueCode::UnsupportedSchema,
        ),
        (
            "\"schema_version\": 1",
            "\"schema_version\": 0",
            IssueCode::UnsupportedSchema,
        ),
        (
            "\"artifact_type\": \"cytiq_dataset_manifest\"",
            "\"artifact_type\": \"x\"",
            IssueCode::UnsupportedSchema,
        ),
        (
            "\"samples\": [",
            "\"sampels\": [], \"samples\": [",
            IssueCode::MalformedDocument,
        ),
        (
            "\"samples\": [",
            "\"samples\": [], \"samples\": [",
            IssueCode::MalformedDocument,
        ),
    ] {
        let mut issues = Issues::new();
        let changed = text.replacen(from, to, 1);
        assert_ne!(changed, text);
        assert!(dataset::parse(changed.as_bytes(), DocumentFormat::Json, &mut issues).is_none());
        assert_eq!(issues.codes(Severity::Error), [code].into(), "{to}");
    }
    // Version 1 migrates to itself.
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(dataset::migrate(value.clone()).unwrap(), value);
}

#[test]
fn unknown_contracts_are_unsupported() {
    expect(
        &mutated(|m| m.contracts.push("cytiq.events.v9".into())),
        false,
        IssueCode::UnsupportedSchema,
    );
    expect(
        &mutated(|m| m.artifacts[4].contract = contracts::EVENTS.into()),
        false,
        IssueCode::UnsupportedSchema,
    );
}

#[test]
fn profiles_require_their_metadata() {
    let built = mutated(|m| {
        m.study = None;
        m.panels[0].channels[1].detector = None;
        m.specimens[0].organism = None;
    });
    let issues = check(&built, Profile::MiflowcytMinimal, false);
    let missing = issues
        .items()
        .iter()
        .filter(|i| i.code == IssueCode::MissingMetadata)
        .count();
    assert!(missing >= 4, "{:?}", issues.items());
    assert!(!check(&built, Profile::Structural, false).has_errors());
}

#[test]
fn panels_keep_detectors_markers_and_reporters_consistent() {
    expect(
        &mutated(|m| m.panels[0].channels[1].detector = Some("V450".into())),
        false,
        IssueCode::PanelMismatch,
    );
    expect(
        &mutated(|m| m.panels[0].channels[2].reporter = Some("fitc".into())),
        false,
        IssueCode::PanelMismatch,
    );
    expect(
        &mutated(|m| m.samples[1].panel = "T".into()),
        false,
        IssueCode::PanelMismatch,
    );
    let conflated = check(
        &mutated(|m| m.panels[0].channels[1].marker = Some("FITC".into())),
        Profile::Structural,
        false,
    );
    assert!(
        conflated
            .codes(Severity::Warning)
            .contains(&IssueCode::PanelMismatch)
    );
    // Objects: the table's channels must be the panel's.
    expect(
        &mutated(|m| m.panels[0].channels.pop().map(|_| ()).unwrap()),
        true,
        IssueCode::PanelMismatch,
    );
}

#[test]
fn signal_states_must_be_compatible() {
    // Transform recorded before compensation.
    expect(
        &mutated(|m| m.artifacts[2].layer.as_mut().unwrap().steps.reverse()),
        false,
        IssueCode::IncompatibleSignalState,
    );
    // A conventional panel cannot hold unmixed files.
    expect(
        &mutated(|m| m.panels[0].signal_state = cytiq_manifest::SignalState::Unmixed),
        false,
        IssueCode::IncompatibleSignalState,
    );
    // Panel T's compensated files declared as already compensated elsewhere.
    expect(
        &mutated(|m| m.panels[0].signal_state = cytiq_manifest::SignalState::Compensated),
        false,
        IssueCode::IncompatibleSignalState,
    );
    // Two raw layers of one panel that cannot be stacked: B's file has other
    // channels than T's.
    let stacked = mutated(|m| {
        m.acquisitions[1].panel = "T".into();
        m.samples[1].panel = "T".into();
        m.artifacts[3].panel = Some("T".into());
    });
    let issues = check(&stacked, Profile::Structural, false);
    let incompatible: Vec<_> = issues
        .items()
        .iter()
        .filter(|i| i.code == IssueCode::IncompatibleSignalState)
        .collect();
    assert_eq!(incompatible.len(), 1, "{:?}", issues.items());
    assert!(incompatible[0].pointer.ends_with("/layer/recipe_id"));
    // A parent whose steps are not a prefix of its child's.
    expect(
        &mutated(|m| m.artifacts[1].parents = vec![m.artifacts[2].semantic_id.clone()]),
        false,
        IssueCode::IncompatibleSignalState,
    );
}

#[test]
fn event_identities_are_unique() {
    expect(
        &mutated(|m| {
            let mut copy = m.artifacts[0].clone();
            copy.location = "events/T/copy.parquet".into();
            m.artifacts.push(copy);
        }),
        false,
        IssueCode::DuplicateEventIdentity,
    );
}

#[test]
fn acquisitions_and_identifiers_are_unique() {
    expect(
        &mutated(|m| m.acquisitions.push(m.acquisitions[0].clone())),
        false,
        IssueCode::DuplicateAcquisition,
    );
    expect(
        &mutated(|m| m.samples[1].acquisition = m.samples[0].acquisition.clone()),
        false,
        IssueCode::DuplicateAcquisition,
    );
    expect(
        &mutated(|m| m.samples[1].id = m.samples[0].id.to_lowercase()),
        false,
        IssueCode::DuplicateIdentifier,
    );
    let hashed = check(
        &mutated(|m| m.samples[0].id = "a".repeat(64)),
        Profile::Structural,
        false,
    );
    assert!(
        hashed
            .codes(Severity::Warning)
            .contains(&IssueCode::DuplicateIdentifier)
    );
}

#[test]
fn references_must_resolve_to_the_same_acquisition() {
    expect(
        &mutated(|m| m.samples[0].specimen = Some("SP99".into())),
        false,
        IssueCode::InvalidReference,
    );
    expect(
        &mutated(|m| m.artifacts[1].parents = vec!["e".repeat(64)]),
        false,
        IssueCode::InvalidReference,
    );
    expect(
        &mutated(|m| m.artifacts[4].parents = vec![m.artifacts[3].semantic_id.clone()]),
        false,
        IssueCode::InvalidReference,
    );
    expect(
        &mutated(|m| m.artifacts[0].acquisition = Some("e".repeat(64))),
        false,
        IssueCode::InvalidReference,
    );
}

#[test]
fn channel_order_is_physical_order() {
    expect(
        &mutated(|m| m.panels[0].channels.swap(1, 2)),
        false,
        IssueCode::InconsistentChannelOrder,
    );
    expect(
        &mutated(|m| m.panels[0].channels[2].name = "fl1-a".into()),
        false,
        IssueCode::InconsistentChannelOrder,
    );
    // Objects: same channels, other order than the panel's.
    expect(
        &mutated(|m| {
            let channels = &mut m.panels[0].channels;
            let (first, second) = (channels[1].index, channels[2].index);
            channels.swap(1, 2);
            channels[1].index = first;
            channels[2].index = second;
        }),
        true,
        IssueCode::InconsistentChannelOrder,
    );
}

#[test]
fn objects_must_exist_with_their_recorded_digests() {
    let built = build();
    std::fs::remove_file(
        built
            .root
            .path()
            .join(&built.manifest.artifacts[3].location),
    )
    .unwrap();
    expect(&built, true, IssueCode::MissingObject);

    let built = build();
    let path = built
        .root
        .path()
        .join(&built.manifest.artifacts[4].location);
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    expect(&built, true, IssueCode::CorruptArtifactHash);

    // Recorded digest of another layer's table.
    expect(
        &mutated(|m| {
            m.artifacts[0].content.values_sha256 = m.artifacts[1].content.values_sha256.clone();
        }),
        true,
        IssueCode::CorruptArtifactHash,
    );
    // A mask of another acquisition.
    expect(
        &mutated(|m| {
            m.artifacts[4].acquisition = Some(m.acquisitions[1].id.clone());
            m.artifacts[4].parents.clear();
        }),
        true,
        IssueCode::InvalidReference,
    );
}

#[test]
fn identifying_metadata_needs_an_explicit_contract() {
    expect(
        &mutated(|m| {
            m.acquisitions[0]
                .keywords
                .insert("$FIL".into(), "Patient_Name.fcs".into());
        }),
        false,
        IssueCode::PrivacyViolation,
    );
    expect(
        &mutated(|m| m.artifacts[0].location = "/home/user/events.parquet".into()),
        false,
        IssueCode::PrivacyViolation,
    );
    expect(
        &mutated(|m| m.artifacts[0].location = "s3://key:secret@bucket/a.parquet".into()),
        false,
        IssueCode::PrivacyViolation,
    );
    expect(
        &mutated(|m| {
            m.artifacts[0].execution = Some(cytiq_population::identity::ExecutionRef {
                provenance: Some("events/T/x.provenance.json".into()),
                fingerprint: None,
            });
        }),
        false,
        IssueCode::PrivacyViolation,
    );
    // Under a controlled contract the same keyword is allowed.
    let built = mutated(|m| {
        m.privacy = PrivacyPolicy {
            profile: PrivacyProfile::Controlled,
            disclosures: vec![Disclosure {
                contract: "DUA-2026-014".into(),
                purpose: "multi-site QC".into(),
                keywords: vec!["$FIL".into()],
                source_locations: false,
                provenance_records: false,
            }],
        };
        m.acquisitions[0]
            .keywords
            .insert("$FIL".into(), "Patient_Name.fcs".into());
    });
    assert!(!check(&built, Profile::Structural, false).has_errors());
}

#[test]
fn reports_name_the_manifest_and_its_status() {
    let built = build();
    let path = built.root.path().join("dataset.json");
    std::fs::write(&path, built.manifest.canonical_json()).unwrap();
    let options = ValidateOptions {
        profile: Profile::MiflowcytMinimal,
        root: None,
        verify_objects: true,
    };
    let report = dataset::validate(&path, &options).unwrap();
    assert_eq!(report.status, ReportStatus::Valid, "{:?}", report.issues);
    assert_eq!(report.dataset_id, built.manifest.dataset_id);
    assert_eq!(
        report.manifest_sha256,
        cytiq_provenance::hash_file(&path).unwrap().1
    );

    let broken = built.manifest.canonical_json().replace("SP01-B", "SP01-T");
    std::fs::write(&path, broken).unwrap();
    let report = dataset::validate(&path, &options).unwrap();
    assert_eq!(report.status, ReportStatus::Invalid);
    assert!(report.errors > 0);
    assert!(dataset::validate(&built.root.path().join("absent.json"), &options).is_err());
}

#[test]
fn dataset_ids_are_reproducible_from_canonical_json() {
    let built = build();
    let text = built.manifest.canonical_json();
    let mut issues = Issues::new();
    let parsed = dataset::parse(text.as_bytes(), DocumentFormat::Json, &mut issues).unwrap();
    assert_eq!(parsed, built.manifest);
    assert_eq!(
        parsed.canonical_json(),
        text,
        "canonical JSON round-trips byte for byte"
    );
    let _: PathBuf = built.root.path().to_path_buf();
}
