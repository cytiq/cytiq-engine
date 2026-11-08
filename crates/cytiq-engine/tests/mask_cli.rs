mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_mask::EventMask;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/clean")
        .join(name)
}

fn create_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&directory).expect("temp dir should be created");
    directory
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

fn run(args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn combines_peacoqc_and_clean_masks_by_source_event() {
    let directory = create_temp_dir("cytiq-engine-mask-combine");
    let raw = fixture("sample.fcs");
    let peacoqc = directory.join("sample.peacoqc.fcs");
    let clean = directory.join("clean");
    let combined_path = directory.join("analysis.parquet");
    run(&[
        "peacoqc",
        "--input",
        utf8(&raw),
        "--output",
        utf8(&peacoqc),
        "--channels",
        "FL1-A,FL2-A",
        "--remove-margins",
    ]);
    // clean on the PeacoQC output: its masks cover only the retained events.
    run(&["clean", "--input", utf8(&peacoqc), "--output", utf8(&clean)]);
    let peacoqc_masks = directory.join("sample.peacoqc.masks.parquet");
    let clean_masks = clean.join("masks.parquet");

    run(&[
        "mask",
        "combine",
        "--mask",
        utf8(&peacoqc_masks),
        "--mask",
        utf8(&clean_masks),
        "--output",
        utf8(&combined_path),
    ]);

    let peacoqc_mask = EventMask::read_parquet(&peacoqc_masks).expect("peacoqc mask");
    let clean_mask = EventMask::read_parquet(&clean_masks).expect("clean mask");
    let combined = EventMask::read_parquet(&combined_path).expect("combined mask");
    // Every mask, and the derived FCS, point back at the raw file.
    let raw_sha256 = common::sha256_hex(&raw);
    assert_eq!(common::acquisition_of(&peacoqc), raw_sha256);
    assert_eq!(peacoqc_mask.acquisition, raw_sha256);
    assert_eq!(clean_mask.acquisition, raw_sha256);
    assert_eq!(combined.acquisition, raw_sha256);
    assert_eq!(combined.source_event_ids, peacoqc_mask.source_event_ids);
    let clean_kept = clean_mask
        .source_event_ids
        .iter()
        .zip(&clean_mask.keep)
        .filter_map(|(id, keep)| keep.then_some(*id))
        .collect::<std::collections::HashSet<_>>();
    let expected = peacoqc_mask
        .source_event_ids
        .iter()
        .zip(&peacoqc_mask.keep)
        .map(|(id, keep)| *keep && clean_kept.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(combined.keep, expected);
    assert!(combined.kept() <= clean_mask.kept());
    let names = combined
        .filters
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    common::assert_provenance(
        &directory.join("analysis.parquet.provenance.json"),
        "cytiq mask combine",
        &[&peacoqc_masks, &clean_masks],
        &[&combined_path],
    );
    common::assert_provenance(
        &clean.join("provenance.json"),
        "cytiq clean",
        &[&peacoqc],
        &[&clean_masks, &clean.join("metrics.json")],
    );
    assert_eq!(
        names,
        [
            "margin_pass",
            "margin_lower_pass",
            "margin_upper_pass",
            "peacoqc_pass",
            "non_debris",
            "singlets",
            "live"
        ]
    );
}

#[test]
fn refuses_to_replace_combined_mask_without_overwrite() {
    let directory = create_temp_dir("cytiq-engine-mask-overwrite");
    let mask = directory.join("mask.parquet");
    EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        vec![true, false],
        None,
    )
    .write_parquet(&mask)
    .expect("mask should write");
    let output = directory.join("combined.parquet");
    fs::write(&output, b"existing").expect("placeholder should write");

    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args([
            "mask",
            "combine",
            "--mask",
            utf8(&mask),
            "--output",
            utf8(&output),
        ])
        .output()
        .expect("command should run");

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--overwrite"));
}

fn combine_error(masks: &[&Path], output: &Path) -> String {
    let mut args = vec!["mask", "combine"];
    for mask in masks {
        args.extend(["--mask", utf8(mask)]);
    }
    args.extend(["--output", utf8(output)]);
    let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(&args)
        .output()
        .expect("command should run");
    assert!(!result.status.success());
    assert!(!output.exists());
    String::from_utf8_lossy(&result.stderr).into_owned()
}

#[test]
fn fails_to_combine_masks_from_different_files() {
    let directory = create_temp_dir("cytiq-engine-mask-acquisitions");
    let sample = directory.join("sample.parquet");
    let control = directory.join("control.parquet");
    // Same rows, different raw files: only the acquisition tells them apart.
    EventMask::new(
        common::acquisition_of(&fixture("sample.fcs")),
        vec![true; 4],
        None,
    )
    .write_parquet(&sample)
    .expect("mask should write");
    EventMask::new(
        common::acquisition_of(&fixture("control.fcs")),
        vec![true; 4],
        None,
    )
    .write_parquet(&control)
    .expect("mask should write");

    let stderr = combine_error(&[&sample, &control], &directory.join("combined.parquet"));
    assert!(
        stderr.contains("mask 2 was computed on acquisition"),
        "{stderr}"
    );
}

#[test]
fn rejects_a_mask_without_acquisition() {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, BooleanArray, RecordBatch, UInt64Array};

    let directory = create_temp_dir("cytiq-engine-mask-legacy");
    let legacy = directory.join("legacy.parquet");
    let batch = RecordBatch::try_from_iter([
        (
            "source_event_id",
            Arc::new(UInt64Array::from(vec![0_u64, 1])) as ArrayRef,
        ),
        (
            "keep",
            Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef,
        ),
    ])
    .expect("batch should build");
    let mut writer = parquet::arrow::ArrowWriter::try_new(
        fs::File::create(&legacy).expect("file should be created"),
        batch.schema(),
        None,
    )
    .expect("writer should open");
    writer.write(&batch).expect("batch should write");
    writer.close().expect("writer should close");

    let stderr = combine_error(&[&legacy], &directory.join("combined.parquet"));
    assert!(stderr.contains("regenerate it"), "{stderr}");
}
