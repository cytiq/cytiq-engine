//! Dataset manifest v1: a two-panel dataset built from synthetic FCS files,
//! the identities it carries, and one test per validation code.

mod common;

use std::path::PathBuf;

use common::{Built, Storage, build, build_storing};
use cytiq_population::dataset::{
    self, ArtifactEntry, ArtifactRole, DatasetManifest, Profile, ReportStatus, ValidateOptions,
    contracts,
};
use cytiq_population::events;
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

#[test]
fn layers_split_into_parts_validate_like_one_table() {
    let whole = build();
    let built = build_storing(Storage::Parts);
    for profile in [Profile::Structural, Profile::MiflowcytMinimal] {
        let issues = check(&built, profile, true);
        assert!(!issues.has_errors(), "{profile:?}: {:?}", issues.items());
    }
    let (table, parts) = (&whole.manifest.artifacts[0], &built.manifest.artifacts[0]);
    assert_eq!(parts.contract, contracts::EVENTS_PARTS);
    assert_eq!(parts.semantic_id, table.semantic_id);
    assert_eq!(parts.content.values_sha256, table.content.values_sha256);
    // The dataset id covers each artifact's contract, so it does change.
    assert_ne!(built.manifest.dataset_id, whole.manifest.dataset_id);
    let manifest = built.root.path().join(&parts.location);
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    dataset::conforms(&document, "events-parts.schema.json").unwrap();
    assert_eq!(document["parts"].as_array().unwrap().len(), 3);

    let directory = manifest.parent().unwrap();
    std::fs::remove_file(directory.join(cytiq_population::events::part_name(2))).unwrap();
    expect(&built, true, IssueCode::MissingObject);
}

/// Rewrite an events table with another source digest in its footer, as a third
/// party could: values and identities stay.
fn with_source(built: &mut Built, artifact: usize, source: &str) {
    use parquet::arrow::ArrowWriter;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let entry = &mut built.manifest.artifacts[artifact];
    let path = built.root.path().join(&entry.location);
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap()).unwrap();
    let schema = builder.schema().clone();
    let batches: Vec<_> = builder.build().unwrap().map(Result::unwrap).collect();
    let mut metadata = schema.metadata().clone();
    metadata.insert(events::SOURCE_SHA256_KEY, source);
    let schema = std::sync::Arc::new(schema.as_ref().clone().with_metadata(metadata));
    let mut writer =
        ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema.clone(), None).unwrap();
    for batch in batches {
        writer
            .write(&batch.with_schema(schema.clone()).unwrap())
            .unwrap();
    }
    writer.close().unwrap();
    entry.content = cytiq_population::identity::ContentRef {
        values_sha256: entry.content.values_sha256.clone(),
        ..dataset::content_of(&path).unwrap()
    };
}

#[test]
fn a_raw_layer_of_every_event_comes_from_its_acquisition() {
    // The same table with its own source stays valid: only the source counts.
    let mut built = build();
    let acquisition = built.manifest.artifacts[0].acquisition.clone().unwrap();
    with_source(&mut built, 0, &acquisition);
    let issues = check(&built, Profile::Structural, true);
    assert!(!issues.has_errors(), "{:?}", issues.items());

    let mut built = build();
    with_source(&mut built, 0, &"d".repeat(64));
    expect(&built, true, IssueCode::InvalidReference);
    let issues = check(&built, Profile::Structural, true);
    assert_eq!(issues.errors(), 1, "{:?}", issues.items());
    assert_eq!(issues.items()[0].pointer, "/artifacts/0");

    // A derived layer comes from the file its last stage wrote.
    for derived in [1, 2] {
        let mut built = build();
        with_source(&mut built, derived, &"d".repeat(64));
        let issues = check(&built, Profile::Structural, true);
        assert!(!issues.has_errors(), "{:?}", issues.items());
    }
}

/// List the record of a `tool` run that made `output` from `input`, under a
/// disclosure that lets the dataset carry records.
fn with_record(
    built: &mut Built,
    tool: &str,
    reason: cytiq_provenance::outcome::OutcomeReason,
    input: &str,
    output: &str,
) {
    use cytiq_provenance::{FileRecord, RunRecorder};
    let file = |path: &str, role: Option<&str>, sha256: &str| FileRecord {
        path: path.into(),
        role: role.map(Into::into),
        bytes: 1,
        sha256: sha256.into(),
    };
    let mut record = RunRecorder::start(tool, "0.0.0", vec![])
        .prepare_staged(&[], reason)
        .unwrap();
    record.inputs = vec![file("instrument.fcs", Some("input"), input)];
    record.outputs = vec![file("repaired.fcs", None, output)];
    let location = "provenance/repaired.fcs.provenance.json";
    let path = built.root.path().join(location);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let manifest = &mut built.manifest;
    manifest.artifacts.push(ArtifactEntry::object(
        ArtifactRole::Provenance,
        contracts::PROVENANCE,
        None,
        vec![],
        location.into(),
        dataset::content_of(&path).unwrap(),
    ));
    if !manifest
        .contracts
        .iter()
        .any(|c| c == contracts::PROVENANCE)
    {
        manifest.contracts.push(contracts::PROVENANCE.into());
    }
    manifest.privacy = PrivacyPolicy {
        profile: PrivacyProfile::Controlled,
        disclosures: vec![Disclosure {
            contract: "DUA-2026-014".into(),
            purpose: "cohort analysis".into(),
            keywords: vec![],
            source_locations: false,
            provenance_records: true,
        }],
    };
    manifest.dataset_id = None;
}

#[test]
fn a_listed_repair_record_vouches_for_the_file_it_made() {
    use cytiq_provenance::outcome::OutcomeReason::{ChecksFailed, Success};
    let made = "d".repeat(64);
    let other = "e".repeat(64);
    let with = |tool: &str, reason, input: Option<&str>, output: &str| {
        let mut built = build();
        let acquisition = built.manifest.artifacts[0].acquisition.clone().unwrap();
        with_source(&mut built, 0, &made);
        with_record(
            &mut built,
            tool,
            reason,
            input.unwrap_or(&acquisition),
            output,
        );
        built
    };

    let repaired = with("cytiq repair", Success, None, &made);
    let issues = check(&repaired, Profile::Structural, true);
    assert!(!issues.has_errors(), "{:?}", issues.items());

    // Another command, a failed run, another acquisition or another file
    // vouch for nothing.
    for built in [
        with("cytiq compensate", Success, None, &made),
        with("cytiq repair", ChecksFailed, None, &made),
        with("cytiq repair", Success, Some(&other), &made),
        with("cytiq repair", Success, None, &other),
    ] {
        let issues = check(&built, Profile::Structural, true);
        assert_eq!(issues.errors(), 1, "{:?}", issues.items());
        assert_eq!(issues.items()[0].code, IssueCode::InvalidReference);
        assert_eq!(issues.items()[0].pointer, "/artifacts/0");
    }
}

#[test]
fn a_projection_is_not_a_signal_layer() {
    let built = build_storing(Storage::Projection);
    let issues = check(&built, Profile::Structural, false);
    assert!(!issues.has_errors(), "{:?}", issues.items());
    expect(&built, true, IssueCode::PanelMismatch);
}

/// Add a QC label table of panel T's acquisition, listed like `cytiq export
/// labels` output, and return its artifact index.
fn with_labels(built: &mut Built) -> usize {
    use cytiq_population::labels::{
        DESCRIPTOR_CONTRACT, Eligibility, LabelBatch, LabelColumn, LabelDescriptor, LabelKind,
        LabelSettings, LabelState, LabelWriter, Producer,
    };
    let acquisition = built.manifest.artifacts[4].acquisition.clone().unwrap();
    let descriptor = LabelDescriptor {
        contract: DESCRIPTOR_CONTRACT.into(),
        kind: LabelKind::Qc,
        acquisition_id: acquisition,
        raw_events: 5,
        producer: Producer {
            tool: "cytiq compensate".into(),
            native_contract: contracts::MASK.into(),
            native_sha256: built.manifest.artifacts[4].content.sha256.clone(),
            input_sha256: Some("c".repeat(64)),
            input_layer_id: None,
        },
        definition: None,
        eligibility: Eligibility {
            masks: vec![],
            verified: false,
        },
        labels: vec![LabelColumn {
            name: "margin_pass".into(),
            parent: None,
            class: None,
        }],
    };
    let location = "labels/T/SP01-T.qc.parquet";
    let path = built.root.path().join(location);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = LabelWriter::create(
        &path,
        descriptor,
        &LabelSettings::default(),
        cytiq_mask::parquet::ParquetReadLimits::default(),
    )
    .unwrap();
    let kept = [true, true, false, true, true];
    writer
        .write(LabelBatch {
            first: 0,
            states: kept
                .iter()
                .map(|keep| {
                    if *keep {
                        LabelState::Accepted
                    } else {
                        LabelState::QcRejected
                    }
                })
                .collect(),
            columns: vec![std::sync::Arc::new(arrow_array::BooleanArray::from(
                kept.iter().map(|keep| Some(*keep)).collect::<Vec<_>>(),
            ))],
        })
        .unwrap();
    let written = writer.finish().unwrap();
    let parents = vec![built.manifest.artifacts[4].semantic_id.clone()];
    built.manifest.artifacts.push(ArtifactEntry::labels(
        &written.summary,
        Some("T".into()),
        parents,
        location.into(),
        dataset::content_of(&path).unwrap(),
    ));
    built.manifest.contracts.push(contracts::LABELS.into());
    built.manifest.dataset_id = None;
    built.manifest.artifacts.len() - 1
}

#[test]
fn a_labels_table_is_an_annotation_of_one_acquisition() {
    let mut built = build();
    with_labels(&mut built);
    let issues = check(&built, Profile::Structural, true);
    assert!(!issues.has_errors(), "{:?}", issues.items());

    let mut foreign = build();
    let index = with_labels(&mut foreign);
    let other = foreign.manifest.artifacts[3].acquisition.clone();
    foreign.manifest.artifacts[index].acquisition = other;
    expect(&foreign, true, IssueCode::InvalidReference);
}

#[test]
fn labels_semantic_id_is_the_annotation_id() {
    let mut built = build();
    let index = with_labels(&mut built);
    built.manifest.artifacts[index].semantic_id = "0".repeat(64);
    expect(&built, true, IssueCode::CorruptArtifactHash);

    let mut built = build();
    let index = with_labels(&mut built);
    built.manifest.artifacts[index].content.values_sha256 = Some("0".repeat(64));
    expect(&built, true, IssueCode::CorruptArtifactHash);
}

#[test]
fn population_annotation_descriptors_need_a_provenance_disclosure() {
    let descriptor = |m: &mut DatasetManifest| {
        let acquisition = m.artifacts[4].acquisition.clone();
        m.artifacts.push(ArtifactEntry::object(
            ArtifactRole::Annotation,
            contracts::ANNOTATIONS[0],
            acquisition,
            vec![],
            "observations/T/event-annotations.json".into(),
            m.artifacts[4].content.clone(),
        ));
        m.contracts.push(contracts::ANNOTATIONS[0].into());
    };
    expect(&mutated(descriptor), false, IssueCode::PrivacyViolation);
    let disclosed = mutated(|m| {
        descriptor(m);
        m.privacy = PrivacyPolicy {
            profile: PrivacyProfile::Controlled,
            disclosures: vec![Disclosure {
                contract: "DUA-2026-014".into(),
                purpose: "cohort analysis".into(),
                keywords: vec![],
                source_locations: false,
                provenance_records: true,
            }],
        };
    });
    let issues = check(&disclosed, Profile::Structural, false);
    assert!(
        !issues
            .codes(Severity::Error)
            .contains(&IssueCode::PrivacyViolation),
        "{:?}",
        issues.items()
    );
}

/// Subject P01 in group AML; specimen SP01 at timepoint D0; both samples in
/// batch B1.
fn design() -> dataset::Design {
    use dataset::{Design, Factor, FactorLevel, SampleDesign, SpecimenDesign, SubjectEntry};
    let factors = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    };
    Design {
        factors: vec![
            Factor {
                name: "group".into(),
                level: FactorLevel::Subject,
            },
            Factor {
                name: "timepoint".into(),
                level: FactorLevel::Specimen,
            },
            Factor {
                name: "batch".into(),
                level: FactorLevel::Sample,
            },
        ],
        subjects: vec![SubjectEntry {
            id: "P01".into(),
            factors: factors(&[("group", "AML")]),
        }],
        specimens: vec![SpecimenDesign {
            specimen: "SP01".into(),
            subject: "P01".into(),
            factors: factors(&[("timepoint", "D0")]),
        }],
        samples: ["SP01-T", "SP01-B"]
            .into_iter()
            .map(|sample| SampleDesign {
                sample: sample.into(),
                factors: factors(&[("batch", "B1")]),
            })
            .collect(),
    }
}

#[test]
fn a_design_is_part_of_the_dataset_identity() {
    let built = mutated(|m| m.design = Some(design()));
    let issues = check(&built, Profile::MiflowcytMinimal, false);
    assert!(!issues.has_errors(), "{:?}", issues.items());
    let mut regrouped = built.manifest.clone();
    regrouped.design.as_mut().unwrap().subjects[0]
        .factors
        .insert("group".into(), "CLL".into());
    assert_ne!(regrouped.dataset_id(), built.manifest.dataset_id());
    let text = built.manifest.canonical_json();
    let mut issues = Issues::new();
    let parsed = dataset::parse(text.as_bytes(), DocumentFormat::Json, &mut issues).unwrap();
    assert_eq!(parsed.design, Some(design()));
    dataset::conforms(
        &serde_json::from_str(&text).unwrap(),
        "dataset-manifest.schema.json",
    )
    .unwrap();
}

#[test]
fn design_subjects_are_unique_pseudonyms() {
    expect(
        &mutated(|m| {
            let mut design = design();
            design.subjects.push(design.subjects[0].clone());
            m.design = Some(design);
        }),
        false,
        IssueCode::DuplicateIdentifier,
    );
    let dated = mutated(|m| {
        let mut design = design();
        design.subjects[0].id = "1984-03-07".into();
        design.specimens[0].subject = "1984-03-07".into();
        m.design = Some(design);
    });
    let issues = check(&dated, Profile::Structural, false);
    assert!(
        issues
            .codes(Severity::Warning)
            .contains(&IssueCode::PrivacyViolation),
        "{:?}",
        issues.items()
    );
}

#[test]
fn a_specimen_of_an_unknown_subject_is_invalid() {
    expect(
        &mutated(|m| {
            let mut design = design();
            design.specimens[0].subject = "P02".into();
            m.design = Some(design);
        }),
        false,
        IssueCode::InvalidReference,
    );
    expect(
        &mutated(|m| {
            let mut design = design();
            design.samples[0].sample = "SP09-T".into();
            m.design = Some(design);
        }),
        false,
        IssueCode::InvalidReference,
    );
}

#[test]
fn a_factor_given_at_the_wrong_level_is_invalid() {
    expect(
        &mutated(|m| {
            let mut design = design();
            design.subjects[0]
                .factors
                .insert("timepoint".into(), "D0".into());
            m.design = Some(design);
        }),
        false,
        IssueCode::InvalidReference,
    );
    expect(
        &mutated(|m| {
            let mut design = design();
            design.samples[1].factors.clear();
            m.design = Some(design);
        }),
        false,
        IssueCode::MissingMetadata,
    );
}

#[test]
fn undeclared_factors_and_free_text_are_malformed() {
    expect(
        &mutated(|m| {
            let mut design = design();
            design.subjects[0].factors.insert("sex".into(), "F".into());
            m.design = Some(design);
        }),
        false,
        IssueCode::MalformedDocument,
    );
    expect(
        &mutated(|m| {
            let mut design = design();
            design.subjects[0]
                .factors
                .insert("group".into(), "acute myeloid leukemia".into());
            m.design = Some(design);
        }),
        false,
        IssueCode::MalformedDocument,
    );
}
