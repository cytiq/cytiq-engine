//! A file that declares its measurements in `$PnTYPE` (FCS 3.2, 3.3.55):
//! unmixed channels are never exported as a raw layer, and the record says
//! whether the file or the caller stated the layer's base.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use cytiq_fcs::{FcsWriteOptions, FcsWriteParameter};
use serde_json::Value;

fn cytiq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .args(args)
        .output()
        .expect("command should run")
}

fn text(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

/// Scatter, two fluorescence channels and time, each with `$PnTYPE` when
/// given.
fn write(path: &Path, types: [Option<&str>; 4]) {
    let names = ["FSC-A", "FL1-A", "FL2-A", "Time"];
    let parameters: Vec<FcsWriteParameter> = names
        .iter()
        .zip(types)
        .map(|(name, kind)| {
            let mut parameter = FcsWriteParameter::new(*name).with_range(1024.0);
            if let Some(kind) = kind {
                parameter.keywords.push(("TYPE".into(), kind.into()));
            }
            parameter
        })
        .collect();
    let columns = [
        vec![100.0_f32, 200.0, 300.0],
        vec![1.0, 2.0, 3.0],
        vec![4.0, 5.0, 6.0],
        vec![0.0, 1.0, 2.0],
    ];
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &FcsWriteOptions::new())
        .unwrap();
}

fn export(input: &Path, output: &Path, layer: &str, base: &str) -> Output {
    cytiq(&[
        "export",
        "events",
        "--input",
        text(input),
        "--output",
        text(output),
        "--layer",
        layer,
        "--base-state",
        base,
    ])
}

fn basis(output: &Path) -> Value {
    let path = cytiq_provenance::record_path_for_file(output);
    let record: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    record["constants"]["layer_basis"].clone()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const SCATTER: Option<&str> = Some("Forward_Scatter");
const TIME: Option<&str> = Some("Time");

#[test]
fn unmixed_channels_are_refused_as_raw_and_exported_as_unmixed() {
    let root = tempfile::tempdir().unwrap();
    // SpectroFlo's spelling, and the standard's.
    for unmixed in ["Unmixed_Fluorescence", "Unmixed Fluorescence"] {
        let input = root.path().join("unmixed.fcs");
        write(&input, [SCATTER, Some(unmixed), Some(unmixed), TIME]);
        let table = root.path().join("events.parquet");

        let refused = export(&input, &table, "raw", "raw");
        assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
        let message = stderr(&refused);
        assert!(message.contains("`FL1-A`, `FL2-A`"), "{message}");
        assert!(
            message.contains("--layer unmixed --base-state unmixed"),
            "{message}"
        );
        assert!(!table.exists());

        let done = export(&input, &table, "unmixed", "unmixed");
        assert!(done.status.success(), "{}", stderr(&done));
        assert_eq!(basis(&table), "declared_by_file");
        fs::remove_file(&table).unwrap();
        fs::remove_file(cytiq_provenance::record_path_for_file(&table)).unwrap();
    }
}

#[test]
fn a_file_that_declares_raw_and_unmixed_channels_is_refused_under_every_base() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("mixed.fcs");
    let table = root.path().join("events.parquet");
    write(
        &input,
        [
            SCATTER,
            Some("Raw_Fluorescence"),
            Some("Unmixed_Fluorescence"),
            TIME,
        ],
    );
    for (layer, base) in [("raw", "raw"), ("unmixed", "unmixed")] {
        let refused = export(&input, &table, layer, base);
        assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
        assert!(
            stderr(&refused).contains("both raw"),
            "{}",
            stderr(&refused)
        );
        assert!(!table.exists());
    }
}

#[test]
fn the_record_says_whether_the_file_or_the_caller_stated_the_base() {
    let root = tempfile::tempdir().unwrap();
    let cases: [([Option<&str>; 4], &str, &str); 5] = [
        (
            [
                SCATTER,
                Some("Raw_Fluorescence"),
                Some("Raw Fluorescence"),
                TIME,
            ],
            "raw",
            "declared_by_file",
        ),
        ([None, None, None, None], "raw", "asserted_by_caller"),
        ([None, None, None, None], "unmixed", "asserted_by_caller"),
        // Not a value FCS 3.2 defines: no declaration.
        (
            [SCATTER, Some("Fluorescence"), Some("Fluorescence"), TIME],
            "raw",
            "asserted_by_caller",
        ),
        // Values are case sensitive (3.2.14).
        (
            [
                SCATTER,
                Some("unmixed_fluorescence"),
                Some("UNMIXED FLUORESCENCE"),
                TIME,
            ],
            "raw",
            "asserted_by_caller",
        ),
    ];
    for (number, (types, base, expected)) in cases.into_iter().enumerate() {
        let input = root.path().join(format!("{number}.fcs"));
        let table = root.path().join(format!("{number}.parquet"));
        write(&input, types);
        let done = export(&input, &table, base, base);
        assert!(done.status.success(), "case {number}: {}", stderr(&done));
        assert_eq!(basis(&table), expected, "case {number}");
    }
}

/// The `measurement_types` check of a one-sample run whose panel declares
/// `signal_state`.
fn preflight(root: &Path, types: [Option<&str>; 4], signal_state: &str) -> Option<Value> {
    write(&root.join("tube.fcs"), types);
    let manifest = root.join("run.yaml");
    // A raw spectral panel needs its unstained control; the same file serves.
    let controls = if signal_state == "raw" {
        "controls:\n  - { file: tube.fcs, dye: Autofluorescence }\n"
    } else {
        ""
    };
    fs::write(
        &manifest,
        format!(
            "schema_version: 3\ncytometry: spectral\npanels:\n  - name: T\n    signal_state: \
             {signal_state}\n    markers:\n      - {{ marker: CD3, channel: FL1-A }}\nsamples:\n  \
             - {{ id: S1, file: tube.fcs, panel: T }}\n{controls}"
        ),
    )
    .unwrap();
    let report = root.join("preflight.json");
    let output = cytiq(&[
        "preflight",
        "--manifest",
        text(&manifest),
        "--output",
        text(&report),
        "--overwrite",
    ]);
    let document: Value = serde_json::from_slice(
        &fs::read(&report).unwrap_or_else(|_| panic!("{}", stderr(&output))),
    )
    .unwrap();
    document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "measurement_types")
        .cloned()
}

#[test]
fn preflight_holds_the_panel_to_what_the_file_declares() {
    let root = tempfile::tempdir().unwrap();
    let unmixed = [
        SCATTER,
        Some("Unmixed_Fluorescence"),
        Some("Unmixed_Fluorescence"),
        TIME,
    ];
    let check = preflight(root.path(), unmixed, "raw").expect("a raw panel of unmixed files");
    assert_eq!(check["status"], "fail");
    assert!(
        check["message"]
            .as_str()
            .unwrap()
            .contains("`FL1-A`, `FL2-A`")
    );
    assert!(preflight(root.path(), unmixed, "unmixed").is_none());

    let mixed = [
        SCATTER,
        Some("Raw_Fluorescence"),
        Some("Unmixed_Fluorescence"),
        TIME,
    ];
    let check = preflight(root.path(), mixed, "unmixed").expect("a mixed file");
    assert_eq!(check["status"], "fail");

    let unknown = [SCATTER, Some("Fluorescence"), Some("Fluorescence"), TIME];
    let check = preflight(root.path(), unknown, "raw").expect("values FCS 3.2 does not define");
    assert_eq!(check["status"], "warn");
    assert!(
        check["message"]
            .as_str()
            .unwrap()
            .contains("`Fluorescence`")
    );

    let raw = [
        SCATTER,
        Some("Raw_Fluorescence"),
        Some("Raw_Fluorescence"),
        TIME,
    ];
    assert!(preflight(root.path(), raw, "raw").is_none());
}

#[test]
fn values_transformed_from_unmixed_ones_are_no_raw_base() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("unmixed.fcs");
    let transformed = root.path().join("unmixed.arcsinh.fcs");
    write(
        &input,
        [
            SCATTER,
            Some("Unmixed_Fluorescence"),
            Some("Unmixed_Fluorescence"),
            TIME,
        ],
    );
    let done = cytiq(&[
        "transform",
        "--input",
        text(&input),
        "--output",
        text(&transformed),
    ]);
    assert!(done.status.success(), "{}", stderr(&done));
    let table = root.path().join("events.parquet");
    let refused = export(&transformed, &table, "transformed", "raw");
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("unmixed channels"));
    let done = export(&transformed, &table, "transformed", "unmixed");
    assert!(done.status.success(), "{}", stderr(&done));
    assert_eq!(basis(&table), "declared_by_file");
}
