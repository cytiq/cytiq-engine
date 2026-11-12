//! Replays saved fuzz inputs through the check the fuzz target runs, so a
//! crash found once stays fixed without nightly or cargo-fuzz. Hand-written
//! seeds also pin a verdict: `valid*` inputs must be accepted, other named
//! inputs rejected; `crash-*` inputs found by the fuzzer only replay.

use std::fs;
use std::path::{Path, PathBuf};

#[path = "../fuzz/checks.rs"]
mod checks;

fn files_in(directory: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read directory entry").path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("UTF-8 file name")
}

#[test]
fn replays_every_fuzz_regression() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz_regressions/gatingml");
    let inputs = files_in(&root);
    assert!(
        inputs
            .iter()
            .any(|input| file_name(input).starts_with("valid")),
        "no accepted seed"
    );
    for input in inputs {
        let bytes = fs::read(&input).expect("read saved input");
        eprintln!("gatingml: {}", input.display());
        checks::gatingml(&bytes);
        let name = file_name(&input);
        if !name.starts_with("crash-") {
            assert_eq!(
                checks::gatingml_accepts(&bytes),
                name.starts_with("valid"),
                "{name}"
            );
        }
    }
}

/// Every Gating-ML document the repository ships round-trips through the
/// writer: the fuzz property holds on real strategies, not only on seeds.
#[test]
fn fuzz_check_holds_for_every_bundled_gating_ml() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fixtures");
    let mut documents = 0;
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for path in fs::read_dir(&directory)
            .expect("read fixtures")
            .map(|entry| entry.expect("read fixture entry").path())
        {
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "xml") {
                let bytes = fs::read(&path).expect("read fixture");
                if checks::gatingml_accepts(&bytes) {
                    checks::gatingml(&bytes);
                    documents += 1;
                }
            }
        }
    }
    assert!(documents >= 3, "only {documents} Gating-ML fixtures parsed");
}
