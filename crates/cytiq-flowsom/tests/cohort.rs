//! Multi-file training: per-acquisition masks and per-file cohort sampling.
#![allow(clippy::cast_precision_loss)]

use std::fs;
use std::path::{Path, PathBuf};

use cytiq_fcs::FcsWriteParameter;
use cytiq_flowsom::{
    CohortSampling, FlowSomConfig, FlowSomError, FlowSomModel, Metaclustering, sample_seed,
    subsample_rows,
};
use cytiq_mask::EventMask;

const CHANNELS: [&str; 3] = ["FL1-A", "FL2-A", "FL3-A"];

fn temp_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "cytiq-flowsom-cohort-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("temp dir");
    directory
}

const LABELS: [&str; 3] = ["CD3", "CD4", "CD8"];

/// An acquisition of `events` rows shifted by `shift`: distinct bytes, so a
/// distinct acquisition id. `labels` are the `$PnS` of `CHANNELS`.
fn write_fcs(path: &Path, events: u16, shift: f32, labels: [&str; 3]) {
    let mut columns = vec![Vec::new(); 4];
    for raw in 0..events {
        let index = f32::from(raw);
        let cluster = if raw % 3 == 0 { 300.0 } else { 0.0 };
        columns[0].push(index);
        columns[1].push(100.0 + shift + cluster + (index * 7.0) % 23.0);
        columns[2].push(180.0 + shift * 0.5 + cluster * 0.5 + (index * 5.0) % 17.0);
        columns[3].push(80.0 + shift * 0.25 + cluster * 0.25 + (index * 3.0) % 11.0);
    }
    let parameters = vec![
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::with_label(CHANNELS[0], labels[0]),
        FcsWriteParameter::with_label(CHANNELS[1], labels[1]),
        FcsWriteParameter::with_label(CHANNELS[2], labels[2]),
    ];
    cytiq_fcs::write_fcs3_1_f32(path, &parameters, &columns).expect("fixture FCS");
}

fn acquisition(path: &Path) -> String {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("readable");
    cytiq_fcs::acquisition_id(path, &dataset.text).expect("acquisition")
}

fn values(path: &Path) -> Vec<Vec<f64>> {
    let dataset = cytiq_fcs::FcsReader::open(path)
        .and_then(cytiq_fcs::FcsReader::read)
        .expect("readable");
    let columns = (1..=3)
        .map(|index| dataset.data.columns[index].to_f64_vec())
        .collect::<Vec<_>>();
    (0..dataset.event_count())
        .map(|row| columns.iter().map(|column| column[row]).collect())
        .collect()
}

fn config(training_events: Option<usize>, sampling: CohortSampling) -> FlowSomConfig {
    let mut config = FlowSomConfig::new(CHANNELS.map(str::to_owned).to_vec());
    config.xdim = 3;
    config.ydim = 3;
    config.metaclusters = 3;
    config.training_events = training_events;
    config.cohort_sampling = sampling;
    config
}

fn cohort(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let directory = temp_dir(name);
    let small = directory.join("small.fcs");
    let large = directory.join("large.fcs");
    write_fcs(&small, 90, 0.0, LABELS);
    write_fcs(&large, 600, 40.0, LABELS);
    (directory, small, large)
}

#[test]
fn per_file_sampling_draws_an_equal_share_and_scales_the_balanced_set() {
    let (directory, small, large) = cohort("balanced");
    let config = config(Some(120), CohortSampling::PerFile);
    let model = FlowSomModel::train_files(
        &[small.clone(), large.clone()],
        &config,
        Metaclustering::Average,
    )
    .expect("trains");
    let cohort = model.cohort.as_ref().expect("per-file summary");
    assert_eq!(cohort.per_file_quota, 60);
    assert_eq!(
        cohort
            .acquisitions
            .iter()
            .map(|entry| (entry.acquisition.clone(), entry.events, entry.sampled))
            .collect::<Vec<_>>(),
        [
            (acquisition(&small), 90, 60),
            (acquisition(&large), 600, 60),
        ]
    );
    assert_eq!(model.training_events, 120);

    // The scale is that of the balanced aggregate, drawn from each file's own
    // stream, not of the 690 pooled events.
    let mut aggregate = Vec::new();
    for path in [&small, &large] {
        let rows = values(path);
        let picks = subsample_rows(rows.len(), 60, sample_seed(config.seed, &acquisition(path)));
        aggregate.extend(picks.into_iter().map(|row| rows[row].clone()));
    }
    for (channel, scale) in model.scale.iter().enumerate() {
        let column = aggregate.iter().map(|row| row[channel]).collect::<Vec<_>>();
        let mean = column.iter().sum::<f64>() / column.len() as f64;
        let variance = column
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (column.len() - 1) as f64;
        assert!((scale.mean - mean).abs() < 1e-9, "{channel}");
        assert!((scale.standard_deviation - variance.sqrt()).abs() < 1e-9);
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn per_file_draws_depend_on_the_acquisition_not_the_input_order() {
    let (directory, small, large) = cohort("order");
    let config = config(Some(500), CohortSampling::PerFile);
    let forward = FlowSomModel::train_files(
        &[small.clone(), large.clone()],
        &config,
        Metaclustering::Average,
    )
    .expect("trains");
    let backward = FlowSomModel::train_files(&[large, small], &config, Metaclustering::Average)
        .expect("trains");
    let mut forward = forward.cohort.expect("summary").acquisitions;
    let mut backward = backward.cohort.expect("summary").acquisitions;
    // A file smaller than the quota contributes every kept event.
    assert!(
        forward
            .iter()
            .any(|entry| entry.events == 90 && entry.sampled == 90)
    );
    forward.sort_by(|a, b| a.acquisition.cmp(&b.acquisition));
    backward.sort_by(|a, b| a.acquisition.cmp(&b.acquisition));
    assert_eq!(forward, backward);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn masks_apply_to_the_input_of_their_acquisition() {
    let (directory, small, large) = cohort("routing");
    let small_mask = directory.join("small.parquet");
    let large_mask = directory.join("large.parquet");
    let keep = |events: usize| (0..events).map(|row| row % 2 == 0).collect::<Vec<_>>();
    EventMask::new(acquisition(&small), keep(90), None)
        .write_parquet(&small_mask)
        .expect("mask");
    EventMask::new(acquisition(&large), keep(600), None)
        .write_parquet(&large_mask)
        .expect("mask");
    let mut per_file = config(Some(1_000), CohortSampling::PerFile);
    // Given in the other order than the inputs: routing is by acquisition.
    per_file.masks = vec![large_mask.clone(), small_mask.clone()];
    let model = FlowSomModel::train_files(
        &[small.clone(), large.clone()],
        &per_file,
        Metaclustering::Average,
    )
    .expect("trains");
    let events = model
        .cohort
        .expect("summary")
        .acquisitions
        .iter()
        .map(|entry| entry.events)
        .collect::<Vec<_>>();
    assert_eq!(events, [45, 300]);

    let mut pooled = config(None, CohortSampling::Pooled);
    pooled.masks = vec![small_mask.clone(), large_mask];
    let model = FlowSomModel::train_files(
        &[small.clone(), large.clone()],
        &pooled,
        Metaclustering::Average,
    )
    .expect("trains");
    assert_eq!(model.training_events, 345);
    assert!(model.cohort.is_none());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn ambiguous_cohorts_are_rejected() {
    let (directory, small, large) = cohort("ambiguous");
    let small_mask = directory.join("small.parquet");
    let large_mask = directory.join("large.parquet");
    let stray_mask = directory.join("stray.parquet");
    EventMask::new(acquisition(&small), vec![true; 90], None)
        .write_parquet(&small_mask)
        .expect("mask");
    EventMask::new(acquisition(&large), vec![true; 600], None)
        .write_parquet(&large_mask)
        .expect("mask");
    EventMask::new("f".repeat(64), vec![true; 90], None)
        .write_parquet(&stray_mask)
        .expect("mask");
    let message = |result: Result<FlowSomModel, FlowSomError>| match result {
        Err(FlowSomError::InvalidConfig(message)) => message,
        other => panic!("expected an invalid configuration, got {other:?}"),
    };
    let inputs = [small.clone(), large.clone()];

    let mut missing = config(None, CohortSampling::Pooled);
    missing.masks = vec![small_mask.clone()];
    let error = message(FlowSomModel::train_files(
        &inputs,
        &missing,
        Metaclustering::Average,
    ));
    assert!(error.contains("no --mask belongs"), "{error}");

    let mut stray = config(None, CohortSampling::Pooled);
    stray.masks = vec![small_mask.clone(), large_mask, stray_mask];
    let error = message(FlowSomModel::train_files(
        &inputs,
        &stray,
        Metaclustering::Average,
    ));
    assert!(error.contains("matches no training input"), "{error}");

    let repeated = config(Some(100), CohortSampling::PerFile);
    let error = message(FlowSomModel::train_files(
        &[small.clone(), small.clone()],
        &repeated,
        Metaclustering::Average,
    ));
    assert!(error.contains("repeats an acquisition"), "{error}");

    for training_events in [None, Some(0)] {
        let unbounded = config(training_events, CohortSampling::PerFile);
        let error = message(FlowSomModel::train_files(
            &inputs,
            &unbounded,
            Metaclustering::Average,
        ));
        assert!(
            error.contains("per-file cohort sampling needs --training-events"),
            "{error}"
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pooled_files_must_measure_the_same_detectors_and_keep_their_labels() {
    let (directory, small, large) = cohort("detectors");
    let mut by_marker = config(None, CohortSampling::Pooled);
    by_marker.channels = LABELS.map(str::to_owned).to_vec();
    let model =
        FlowSomModel::train_files(&[small.clone(), large], &by_marker, Metaclustering::Average)
            .expect("trains");
    assert_eq!(model.channels, CHANNELS);
    assert_eq!(
        model.channel_labels,
        Some(LABELS.map(|label| vec![label.to_owned()]).to_vec())
    );

    // The same markers on swapped detectors: equal labels, different channels.
    let swapped = directory.join("swapped.fcs");
    write_fcs(&swapped, 90, 20.0, ["CD4", "CD3", "CD8"]);
    match FlowSomModel::train_files(&[small, swapped], &by_marker, Metaclustering::Average) {
        Err(FlowSomError::InvalidConfig(message)) => {
            assert!(message.contains("incompatible channels"), "{message}");
        }
        other => panic!("expected incompatible channels, got {other:?}"),
    }
    fs::remove_dir_all(directory).unwrap();
}
