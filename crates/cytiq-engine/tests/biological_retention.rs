//! Independent `PeacoQC` masks on contiguous, population-labelled `CyMAE` excerpts.
//! Missing data fail: these are required scientific regression fixtures.
mod common;

use std::{fs, path::PathBuf, process::Command};

use cytiq_fcs::FcsReader;
use cytiq_mask::EventMask;
use serde_json::Value;

#[test]
fn labelled_cymae_masks_match_r_with_bounded_peak_selection() {
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/scientific/retention");
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixtures.join("retention.json")).unwrap()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let expected_populations: Value =
        serde_json::from_slice(&fs::read(fixtures.join("r-population-retention.json")).unwrap())
            .unwrap();
    let mut reports = serde_json::Map::new();
    for (name, case) in manifest["cases"].as_object().unwrap() {
        let input = fixtures.join(case["input"].as_str().unwrap());
        let output = directory.path().join(format!("{name}.fcs"));
        let channels = case["channels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(",");
        let result = Command::new(env!("CARGO_BIN_EXE_cytiq"))
            .args(["peacoqc", "--input"])
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .args(["--channels", &channels])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mask = EventMask::read_parquet(&output.with_extension("masks.parquet")).unwrap();
        let reference: Value = serde_json::from_slice(
            &fs::read(fixtures.join(format!("{name}.r-bounded-mask.json"))).unwrap(),
        )
        .unwrap();
        let expected = reference["good"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_bool().unwrap())
            .collect::<Vec<_>>();
        assert!(
            mask.keep == expected,
            "{name}: {} events differ from independent R with bounded peak indexing",
            mask.keep
                .iter()
                .zip(&expected)
                .filter(|(a, b)| a != b)
                .count()
        );
        assert_eq!(mask.acquisition, common::acquisition_of(&input));
        assert_eq!(mask.source_event_ids, (0..20_000).collect::<Vec<u64>>());
        let data = FcsReader::open(&input).and_then(FcsReader::read).unwrap();
        let labels = population_labels(&data);
        let actual = cytiq_qc::retention::population_retention(
            &mask.acquisition,
            &labels,
            None,
            &[("peacoqc", &mask)],
        )
        .unwrap();
        for population in &actual[0].populations {
            let oracle = expected_populations[name]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["population"] == population.population)
                .unwrap();
            assert_eq!(oracle["original"], population.original);
            assert_eq!(oracle["retained"], population.retained);
            assert!(
                (oracle["frequency_error"].as_f64().unwrap() - population.frequency_error.unwrap())
                    .abs()
                    < 1e-14
            );
            assert!(
                population.good_retention.is_none(),
                "population labels do not identify technical errors"
            );
        }
        assert!(
            actual[0].populations.len() >= 5,
            "{name}: lost biological labels"
        );
        reports.insert(name.clone(), serde_json::to_value(actual).unwrap());
    }
    // Machine-readable evidence survives successful tests under ignored target/.
    let evidence =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/scientific/results");
    fs::create_dir_all(&evidence).unwrap();
    fs::write(
        evidence.join("retention-regression.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
}

fn population_labels(data: &cytiq_fcs::FcsDataset) -> Vec<Option<String>> {
    let vocabulary = data
        .text
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("omiqfilternames"))
        .unwrap()
        .1
        .split(',')
        .collect::<Vec<_>>();
    let column = data.find_parameter("OmiqFilter").unwrap();
    data.data
        .column(column)
        .unwrap()
        .to_f64_vec()
        .into_iter()
        .map(|v| {
            assert!(v.is_finite() && v >= 0.0 && v.fract() == 0.0);
            // Published integer codes, bounded by the pinned vocabulary.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(vocabulary[v as usize].to_owned())
        })
        .collect::<Vec<_>>()
}
