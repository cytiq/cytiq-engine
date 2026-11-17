//! The analytics product of the two-panel test dataset with a gate label
//! table, a design, observations and their annotation descriptor: tables,
//! parity with the observations, cohort availability and determinism.

mod common;

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{Array, BooleanArray, StringArray, UInt64Array};
use common::{Built, build};
use cytiq_population::analytics::{
    AnalyticsRunner, ExportDatasetConfig, ParityLevel, Product, VIEWS_FILE,
};
use cytiq_population::dataset::{
    self, ArtifactEntry, ArtifactRole, Design, Factor, FactorLevel, SampleDesign, SpecimenDesign,
    SubjectEntry, contracts,
};
use cytiq_population::labels::{
    DESCRIPTOR_CONTRACT, Definition, Eligibility, LabelBatch, LabelColumn, LabelDescriptor,
    LabelKind, LabelSettings, LabelState, LabelWriter, Producer,
};
use cytiq_population::privacy::{Disclosure, PrivacyPolicy, PrivacyProfile};
use cytiq_population::{
    Denominator, Measure, Method, ObservationStatus, PopulationObservation, Target, TargetKind,
    observation::Lineage,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;

const NATIVE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// Panel T's five events: assessed except event 2; `all` holds every
/// assessed event, `CD3` events 0 and 3.
fn gate_labels(built: &Built, acquisition: &str) -> ArtifactEntry {
    let descriptor = LabelDescriptor {
        contract: DESCRIPTOR_CONTRACT.into(),
        kind: LabelKind::Gate,
        acquisition_id: acquisition.into(),
        raw_events: 5,
        producer: Producer {
            tool: "cytiq gate".into(),
            native_contract: contracts::MASK.into(),
            native_sha256: NATIVE.into(),
            input_sha256: Some("c".repeat(64)),
            input_layer_id: None,
        },
        definition: Some(Definition {
            inputs: BTreeMap::from([("gating_ml".into(), vec!["d".repeat(64)])]),
            parameters: BTreeMap::new(),
        }),
        eligibility: Eligibility {
            masks: vec!["e".repeat(64)],
            verified: true,
        },
        labels: vec![
            LabelColumn {
                name: "all".into(),
                parent: None,
                class: None,
            },
            LabelColumn {
                name: "CD3".into(),
                parent: Some("all".into()),
                class: None,
            },
        ],
    };
    let location = "labels/T/SP01-T.gates.parquet";
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
    let cell = |member: bool, i: usize| kept[i].then_some(member);
    writer
        .write(LabelBatch {
            first: 0,
            states: kept
                .iter()
                .map(|k| {
                    if *k {
                        LabelState::Assessed
                    } else {
                        LabelState::QcRejected
                    }
                })
                .collect(),
            columns: vec![
                Arc::new(BooleanArray::from(
                    (0..5).map(|i| cell(true, i)).collect::<Vec<_>>(),
                )),
                Arc::new(BooleanArray::from(
                    (0..5)
                        .map(|i| cell(i == 0 || i == 3, i))
                        .collect::<Vec<_>>(),
                )),
            ],
        })
        .unwrap();
    let written = writer.finish().unwrap();
    ArtifactEntry::labels(
        &written.summary,
        Some("T".into()),
        vec![],
        location.into(),
        dataset::content_of(&path).unwrap(),
    )
}

fn target(id: &str) -> Target {
    Target {
        kind: TargetKind::Population,
        id: id.into(),
        scope: "cells".into(),
        state: None,
    }
}

/// What `cytiq population observe` writes for the gates `all` (4 events)
/// and `CD3` (2 of `all`): a count and a fraction each.
fn observations(acquisition: &str, cd3: u64) -> Vec<PopulationObservation> {
    let mut rows = Vec::new();
    for (id, count, denominator_target, denominator) in [
        ("cells", 4, None, 4),
        ("t_cells", cd3, Some(target("cells")), 4),
    ] {
        let mut observation = PopulationObservation {
            schema_version: 1,
            observation_id: String::new(),
            specimen_id: "SP01".into(),
            panel_id: "T".into(),
            acquisition_id: Some("plan-label".into()),
            source_id: "gates".into(),
            target: target(id),
            measure: Measure::ObservedCount,
            unit: "events".into(),
            status: ObservationStatus::Measured,
            value_u64: Some(count),
            value_f64: None,
            denominator: Some(Denominator {
                target: denominator_target,
                scope: "cells".into(),
                observed_count: denominator,
            }),
            method: Method {
                name: "gating".into(),
                version: "1".into(),
                definition_sha256: "d".repeat(64),
                experimental: false,
                source_observation_ids: Vec::new(),
            },
            lineage: Lineage {
                acquisition_sha256: Some(acquisition.into()),
                tree_sha256: "1".repeat(64),
                panel_profile_sha256: "2".repeat(64),
                projection_sha256: "3".repeat(64),
                annotations_sha256: Some(NATIVE.into()),
                producer_provenance_sha256: None,
                analysis_mask_sha256: None,
            },
            n_events: 4,
            supporting_count: None,
            reason: None,
        };
        observation.seal().unwrap();
        rows.push(observation.clone());
        observation.measure = Measure::Fraction;
        observation.unit = "fraction".into();
        observation.value_u64 = None;
        #[allow(clippy::cast_precision_loss)]
        let fraction = count as f64 / denominator as f64;
        observation.value_f64 = Some(fraction);
        observation.seal().unwrap();
        rows.push(observation);
    }
    rows
}

/// The `event-annotations.json` of those observations: its bindings name
/// gate `all` as `cells` and `CD3` as `t_cells`.
fn descriptor(acquisition: &str) -> serde_json::Value {
    let reference =
        |c: char| json!({"path": "/somewhere/file", "sha256": c.to_string().repeat(64)});
    json!({
        "schema_version": 1,
        "context": {
            "specimen_id": "SP01", "manifest_sha256": "4".repeat(64), "panel_id": "T",
            "acquisition_id": "plan-label", "acquisition_sha256": acquisition, "raw_events": 5,
            "source_id": "gates", "projection": reference('5'),
            "source": {
                "format": "gate_membership", "definition": reference('d'),
                "bindings": [
                    {"label": "all", "target": {"kind": "population", "id": "cells", "scope": "cells"}},
                    {"label": "CD3", "target": {"kind": "population", "id": "t_cells", "scope": "cells"},
                     "denominator": "all"}
                ]
            }
        },
        "data": {"path": "/somewhere/membership.parquet", "sha256": NATIVE},
        "producer_provenance": reference('6'), "analysis_mask": reference('e'),
        "technical_qc": reference('7'), "technical_qc_provenance": reference('8'),
        "technical_status": "pass", "assessed_events": 4, "unknown_events": 0
    })
}

fn design() -> Design {
    Design {
        factors: vec![Factor {
            name: "group".into(),
            level: FactorLevel::Subject,
        }],
        subjects: vec![SubjectEntry {
            id: "P01".into(),
            factors: BTreeMap::from([("group".into(), "case".into())]),
        }],
        specimens: vec![SpecimenDesign {
            specimen: "SP01".into(),
            subject: "P01".into(),
            factors: BTreeMap::new(),
        }],
        samples: vec![SampleDesign {
            sample: "SP01-T".into(),
            factors: BTreeMap::new(),
        }],
    }
}

/// The test dataset with labels, observations of `cd3` CD3 events, their
/// descriptor and a design, written as `dataset.json`.
fn analytics_dataset(cd3: u64) -> Built {
    let mut built = build();
    let acquisition = built.manifest.artifacts[4].acquisition.clone().unwrap();
    let labels = gate_labels(&built, &acquisition);
    let root = built.root.path();
    let observations_path = root.join("observations/T/observations.parquet");
    std::fs::create_dir_all(observations_path.parent().unwrap()).unwrap();
    cytiq_population::table::write(&observations_path, &observations(&acquisition, cd3)).unwrap();
    let descriptor_path = root.join("observations/T/event-annotations.json");
    std::fs::write(&descriptor_path, descriptor(&acquisition).to_string()).unwrap();
    let manifest = &mut built.manifest;
    manifest.artifacts.push(labels);
    manifest.artifacts.push(ArtifactEntry::object(
        ArtifactRole::Observations,
        contracts::OBSERVATIONS,
        Some(acquisition.clone()),
        vec![],
        "observations/T/observations.parquet".into(),
        dataset::content_of(&observations_path).unwrap(),
    ));
    manifest.artifacts.push(ArtifactEntry::object(
        ArtifactRole::Annotation,
        contracts::ANNOTATIONS[0],
        Some(acquisition),
        vec![],
        "observations/T/event-annotations.json".into(),
        dataset::content_of(&descriptor_path).unwrap(),
    ));
    manifest.contracts.extend([
        contracts::LABELS.into(),
        contracts::OBSERVATIONS.into(),
        contracts::ANNOTATIONS[0].into(),
    ]);
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
    manifest.design = Some(design());
    manifest.dataset_id = None;
    std::fs::write(root.join("dataset.json"), manifest.canonical_json()).unwrap();
    built
}

fn export(built: &Built, name: &str) -> cytiq_population::Result<Product> {
    AnalyticsRunner::run(&ExportDatasetConfig {
        manifest: built.root.path().join("dataset.json"),
        root: None,
        output: built.root.path().join(name),
        overwrite: false,
    })
}

fn column<T: Array + Clone + 'static>(path: &Path, name: &str) -> Vec<T> {
    ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .build()
        .unwrap()
        .map(|batch| {
            batch
                .unwrap()
                .column_by_name(name)
                .unwrap()
                .as_any()
                .downcast_ref::<T>()
                .unwrap()
                .clone()
        })
        .collect()
}

#[test]
fn observations_agree_with_the_label_counts() {
    let built = analytics_dataset(2);
    let product = export(&built, "product").unwrap();
    assert_eq!(product.parity.len(), 1);
    assert_eq!(product.parity[0].level, ParityLevel::Counts);
    assert_eq!(product.parity[0].observations, 4);
    assert!(product.cohort.available, "{:?}", product.cohort);
    let names: Vec<&str> = product.tables.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"label_counts") && names.contains(&"observations"));
    for view in [
        "sample_design",
        "gate_membership",
        "cohort_population",
        "cohort_labels",
    ] {
        assert!(product.views.names.iter().any(|n| n == view), "{view}");
    }

    let tables = built.root.path().join("product/tables");
    let counts = tables.join("label_counts.parquet");
    let columns: Vec<String> = column::<StringArray>(&counts, "label_column")
        .iter()
        .flat_map(|a| a.iter().map(|v| v.unwrap().to_owned()).collect::<Vec<_>>())
        .collect();
    let values: Vec<Option<String>> = column::<StringArray>(&counts, "label_value")
        .iter()
        .flat_map(|a| a.iter().map(|v| v.map(str::to_owned)).collect::<Vec<_>>())
        .collect();
    let n: Vec<u64> = column::<UInt64Array>(&counts, "count")
        .iter()
        .flat_map(|a| a.values().to_vec())
        .collect();
    let cd3_true = columns
        .iter()
        .zip(&values)
        .zip(&n)
        .find(|((c, v), _)| c.as_str() == "CD3" && v.as_deref() == Some("true"))
        .map(|(_, n)| *n);
    assert_eq!(cd3_true, Some(2));

    // The plan's acquisition label never reaches the product; the sample is
    // the dataset's.
    let observations = tables.join("observations.parquet");
    let samples: Vec<Option<String>> = column::<StringArray>(&observations, "sample_id")
        .iter()
        .flat_map(|a| a.iter().map(|v| v.map(str::to_owned)).collect::<Vec<_>>())
        .collect();
    assert!(samples.iter().all(|s| s.as_deref() == Some("SP01-T")));
    let bytes = std::fs::read(&observations).unwrap();
    assert!(!bytes.windows(10).any(|w| w == b"plan-label"));
    for path in std::fs::read_dir(&tables).unwrap() {
        let bytes = std::fs::read(path.unwrap().path()).unwrap();
        assert!(
            !bytes.windows(10).any(|w| w == b"/somewhere"),
            "a path leaked"
        );
    }
}

#[test]
fn observations_that_disagree_with_their_labels_publish_nothing() {
    let built = analytics_dataset(3);
    let error = export(&built, "product").unwrap_err().to_string();
    assert!(error.contains("holds count Some(3)"), "{error}");
    assert!(!built.root.path().join("product").exists());
}

#[test]
fn the_product_is_deterministic() {
    let built = analytics_dataset(2);
    let one = export(&built, "one").unwrap();
    let two = export(&built, "two").unwrap();
    assert_eq!(one, two);
    let read = |name: &str| std::fs::read(built.root.path().join(name).join(VIEWS_FILE)).unwrap();
    assert_eq!(read("one"), read("two"));
    let views = String::from_utf8(read("one")).unwrap();
    assert!(views.contains("count(DISTINCT o.observation_id)"));
    assert!(!views.contains(&built.root.path().display().to_string()));
}

#[test]
fn cohort_views_need_a_subject_for_every_sample() {
    let mut built = analytics_dataset(2);
    built.manifest.design.as_mut().unwrap().specimens.clear();
    built.manifest.dataset_id = None;
    std::fs::write(
        built.root.path().join("dataset.json"),
        built.manifest.canonical_json(),
    )
    .unwrap();
    let product = export(&built, "product").unwrap();
    assert!(!product.cohort.available);
    assert_eq!(
        product.cohort.reason.as_deref(),
        Some("sample `SP01-T` has no subject")
    );
    assert!(!product.views.names.iter().any(|n| n.starts_with("cohort_")));
}

#[test]
fn invalid_datasets_and_existing_outputs_are_refused() {
    let built = analytics_dataset(2);
    let mut broken = built.manifest.clone();
    broken.samples[0].acquisition = "f".repeat(64);
    broken.dataset_id = None;
    std::fs::write(
        built.root.path().join("dataset.json"),
        broken.canonical_json(),
    )
    .unwrap();
    let error = export(&built, "product").unwrap_err().to_string();
    assert!(error.contains("does not validate"), "{error}");

    std::fs::write(
        built.root.path().join("dataset.json"),
        built.manifest.canonical_json(),
    )
    .unwrap();
    export(&built, "product").unwrap();
    assert!(matches!(
        export(&built, "product").unwrap_err(),
        cytiq_population::PopulationError::Publish(cytiq_publish::PublishError::Exists { .. })
    ));
}
