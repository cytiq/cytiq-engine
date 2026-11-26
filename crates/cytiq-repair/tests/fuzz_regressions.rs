//! Replays saved fuzz inputs, and one file per deviation, through the check the
//! fuzz target runs.

mod common;

use std::fs;
use std::path::Path;

use common::Spec;

#[path = "../fuzz/checks.rs"]
mod checks;

#[test]
fn replays_every_fuzz_regression() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz_regressions/repair_plan");
    let mut inputs = 0;
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        eprintln!("{}", path.display());
        checks::repair_plan(&fs::read(path).unwrap());
        inputs += 1;
    }
    assert!(inputs > 0, "no saved inputs");
}

#[test]
fn every_fixture_and_every_deviation_passes_the_check() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/fcs");
    for entry in fs::read_dir(fixtures).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "fcs") {
            checks::repair_plan(&fs::read(path).unwrap());
        }
    }
    for spec in [
        Spec::default(),
        Spec {
            raw_text: b"$INST//$EXP/a/".to_vec(),
            padding: b" \0".to_vec(),
            overlap: true,
            ..Spec::default()
        },
        Spec {
            supplemental: Some(true),
            header: [None, None, Some((0, -1))],
            data_end_past: 1,
            ..Spec::default()
        },
        Spec {
            padding: b" ".to_vec(),
            analysis: Some(b"/A/b/".to_vec()),
            tail: b"tail".to_vec(),
            ..Spec::default()
        },
    ] {
        checks::repair_plan(&spec.build());
    }
}
