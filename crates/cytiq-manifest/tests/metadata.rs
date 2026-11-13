//! Rules of the `schema_version: 3` `MIFlowCyt` metadata.

use std::path::Path;

use cytiq_manifest::{ControlRole, RunManifest};

fn v3() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v3.yaml"))
        .unwrap()
}

fn load(text: &str) -> Result<RunManifest, String> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("run.yaml");
    std::fs::write(&path, text).unwrap();
    RunManifest::load(&path).map_err(|error| error.to_string())
}

fn rejects(text: &str, expected: &str) {
    let error = load(text).expect_err("manifest should be invalid");
    assert!(
        error.contains(expected),
        "expected `{expected}` in: {error}"
    );
}

#[test]
fn accepts_the_reference_document() {
    let manifest = load(&v3()).unwrap();
    assert_eq!(manifest.instruments[0].detectors.len(), 3);
    assert_eq!(manifest.controls[1].role, Some(ControlRole::SingleStain));
    assert_eq!(manifest.specimens[0].ontology[1].id, "UBERON:0000178");
}

#[test]
fn earlier_versions_cannot_use_v3_fields() {
    let downgraded = v3().replace("schema_version: 3", "schema_version: 2");
    rejects(&downgraded, "requires manifest schema_version 3");
    let v1 = "cytometry: conventional\npanel:\n  markers: [{ marker: CD3, reporter: FITC }]\nsamples: [{ id: S01, file: a.fcs }]\n";
    rejects(
        v1,
        "`markers[].reporter` requires manifest schema_version 3",
    );
}

#[test]
fn unknown_metadata_fields_fail_loudly() {
    rejects(
        &v3().replace("organism: Homo sapiens", "organsim: Homo sapiens"),
        "unknown field",
    );
    rejects(
        &v3().replace(
            "kind: pmt, excitation_nm: 488, emission_filter: 530/30",
            "kind: ccd",
        ),
        "unknown variant",
    );
}

#[test]
fn instrument_and_detector_references_must_resolve() {
    rejects(
        &v3().replace(
            "  - name: B\n    instrument: canto-1",
            "  - name: B\n    instrument: aria-2",
        ),
        "instrument `aria-2` is not defined",
    );
    rejects(
        &v3().replace("detector: R660 }", "detector: V450 }"),
        "detector `V450` is not defined",
    );
    rejects(
        &v3().replace("  - name: B\n    instrument: canto-1\n", "  - name: B\n"),
        "names no `instrument`",
    );
    rejects(
        &v3().replace("- { id: B585,", "- { id: b530,"),
        "detector `b530` appears more than once",
    );
    rejects(
        &v3().replace("excitation_nm: 633", "excitation_nm: 0"),
        "`excitation_nm` must be between",
    );
}

#[test]
fn reporters_and_channels_are_unique_within_a_panel() {
    rejects(
        &v3().replace("reporter: PE,", "reporter: fitc,"),
        "reporter `fitc` labels more than one marker",
    );
    rejects(
        &v3().replace("channel: PE-A", "channel: fitc-a"),
        "channel `fitc-a` carries more than one marker",
    );
    // The same reporter in another panel is a different marker assignment.
    load(&v3().replace("reporter: APC,", "reporter: FITC,").replace(
        "dye: APC, primary_channel: APC-A",
        "dye: FITC, primary_channel: APC-A",
    ))
    .unwrap();
}

#[test]
fn specimens_must_be_declared_once_and_referenced() {
    rejects(
        &v3().replace(
            "specimen: SP01 }\n  - { id: SP01-B",
            "specimen: SP02 }\n  - { id: SP01-B",
        ),
        "specimen `SP02` is not defined",
    );
    rejects(
        &v3().replace(
            "ontology:\n      - { id: \"NCBITaxon:9606\"",
            "ontology:\n      - { id: \"NCBITaxon 9606\"",
        ),
        "is not a `PREFIX:LOCAL` CURIE",
    );
    rejects(
        &v3().replace("material: peripheral blood", "material: \" \""),
        "`material` must not be empty",
    );
}

#[test]
fn control_roles_agree_with_their_dye() {
    rejects(
        &v3().replace(
            "dye: Autofluorescence, role: unstained",
            "dye: Autofluorescence, role: fmo",
        ),
        "its `role` must be `unstained`",
    );
    rejects(
        &v3().replace(
            "dye: FITC, primary_channel: FITC-A, panel: T, role: single_stain",
            "dye: FITC, primary_channel: FITC-A, panel: T, role: unstained",
        ),
        "`role: unstained` needs",
    );
    rejects(
        &v3().replace("dye: APC, primary_channel", "dye: PerCP, primary_channel"),
        "no marker of panel `B` has reporter `PerCP`",
    );
}

#[test]
fn experiment_fields_are_not_empty() {
    rejects(
        &v3().replace(
            "keywords: [lymphocytes, immunophenotyping]",
            "keywords: [lymphocytes, \"\"]",
        ),
        "experiment keyword must not be empty",
    );
    rejects(
        &v3().replace("timepoint: Days after enrolment", "timepoint: \"\""),
        "experiment variable `timepoint` must not be empty",
    );
}
