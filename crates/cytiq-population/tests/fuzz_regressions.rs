//! Replays saved fuzz inputs through the checks the fuzz targets run, so a
//! crash found once stays fixed without nightly or cargo-fuzz. Hand-written
//! seeds also pin a verdict: `valid*` inputs must be accepted, other named
//! inputs rejected; `crash-*` inputs found by the fuzzer only replay.

use std::fs;
use std::path::{Path, PathBuf};

use cytiq_population::serialization::{self, DocumentFormat};
use cytiq_population::table;

#[path = "../fuzz/checks.rs"]
mod checks;

/// The check one fuzz target runs on an input, and whether the input is accepted.
type Check = fn(&[u8]);
type Accepts = fn(&[u8]) -> bool;

/// Each target's inputs live in `tests/fuzz_regressions/<target>/`.
const TARGETS: [(&str, Check, Accepts); 3] = [
    ("document_json", checks::document_json, |bytes| {
        serialization::parse(bytes, DocumentFormat::Json).is_ok()
    }),
    ("document_yaml", checks::document_yaml, |bytes| {
        serialization::parse(bytes, DocumentFormat::Yaml).is_ok()
    }),
    (
        "observations_parquet",
        checks::observations_parquet,
        |bytes| table::read_from(bytes::Bytes::copy_from_slice(bytes)).is_ok(),
    ),
];

fn files_in(directory: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read directory entry").path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[test]
fn replays_every_fuzz_regression() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz_regressions");
    for (target, check, accepts) in TARGETS {
        let inputs = files_in(&root.join(target));
        assert!(
            inputs
                .iter()
                .any(|input| file_name(input).starts_with("valid")),
            "no accepted seed for {target}"
        );
        for input in inputs {
            let bytes = fs::read(&input).expect("read saved input");
            eprintln!("{target}: {}", input.display());
            check(&bytes);
            let name = file_name(&input);
            if !name.starts_with("crash-") {
                assert_eq!(
                    accepts(&bytes),
                    name.starts_with("valid"),
                    "{target}/{name}"
                );
            }
        }
    }
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("UTF-8 file name")
}

#[test]
fn fuzz_checks_accept_every_specimen_fixture() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures/specimen");
    let mut documents = 0;
    for path in files_in(&fixtures) {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let bytes = fs::read(&path).expect("read fixture");
            assert!(serialization::parse(&bytes, DocumentFormat::Json).is_ok());
            checks::document_json(&bytes);
            checks::document_yaml(&bytes);
            documents += 1;
        }
    }
    assert!(
        documents > 0,
        "no JSON fixtures under {}",
        fixtures.display()
    );
}
