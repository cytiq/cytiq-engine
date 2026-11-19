//! Replays saved fuzz inputs through the check the fuzz target runs, so a
//! crash found once stays fixed without nightly or cargo-fuzz. Hand-written
//! seeds also pin a verdict: `valid*` inputs must be accepted, other named
//! inputs refused; `crash-*` inputs found by the fuzzer only replay.

use std::fs;
use std::path::Path;

#[path = "../fuzz/checks.rs"]
mod checks;

#[test]
fn replays_every_fuzz_regression() {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz_regressions/truth_parquet");
    let mut inputs = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read directory entry").path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    inputs.sort();
    let (mut accepted, mut refused) = (0, 0);
    for input in inputs {
        let bytes = fs::read(&input).expect("read saved input");
        eprintln!("truth_parquet: {}", input.display());
        checks::truth_parquet(&bytes);
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 file name");
        if !name.starts_with("crash-") {
            let valid = checks::read(&bytes).is_some();
            assert_eq!(valid, name.starts_with("valid"), "{name}");
            accepted += usize::from(valid);
            refused += usize::from(!valid);
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "seeds must cover both verdicts"
    );
}
