//! Regenerates the reference dataset of `data/fixtures/dataset/reference`
//! with the real commands and checks that the committed one has the same
//! identities: every semantic id, layer recipe and values digest, and the
//! dataset id. Bytes are pinned by `data/fixtures/dataset/sha256.json`
//! (checked offline by `cytiq-population`), since a Parquet upgrade may
//! re-encode identical values.
//!
//! `CYTIQ_BLESS=1` rewrites `reference/`; then run
//! `CYTIQ_BLESS=1 cargo test -p cytiq-population --test dataset_fixtures` to
//! refresh the lock. Never bless to make a change pass: explain the changed
//! identities in the pull request.

mod common;

use std::fs;
use std::path::Path;

use cytiq_population::dataset::{self, DatasetManifest};
use cytiq_population::issue::Issues;
use cytiq_population::serialization::DocumentFormat;

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        let destination = target.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &destination);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
}

fn identities(manifest: &DatasetManifest) -> Vec<(String, Option<String>, Option<String>)> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.semantic_id.clone(),
                artifact.layer.as_ref().map(|layer| layer.recipe_id.clone()),
                artifact.content.values_sha256.clone(),
            )
        })
        .collect()
}

#[test]
fn the_reference_dataset_is_reproduced_by_the_commands() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("dataset");
    let work = temporary.path().join("work");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&work).unwrap();
    let built = common::dataset::build(&root, &work);
    let reference = common::dataset::fixtures().join("dataset/reference");
    if std::env::var_os("CYTIQ_BLESS").is_some() {
        if reference.exists() {
            fs::remove_dir_all(&reference).unwrap();
        }
        copy_tree(&root, &reference);
        return;
    }
    let bytes = fs::read(reference.join("manifest.json")).expect("committed reference manifest");
    let mut issues = Issues::new();
    let committed = dataset::parse(&bytes, DocumentFormat::Json, &mut issues)
        .unwrap_or_else(|| panic!("{:?}", issues.items()));
    assert_eq!(
        committed.dataset_id, built.dataset_id,
        "the dataset identity changed"
    );
    assert_eq!(
        identities(&committed),
        identities(&built),
        "an artifact identity changed"
    );
    let committed_locations: Vec<&str> = committed
        .artifacts
        .iter()
        .map(|a| a.location.as_str())
        .collect();
    let built_locations: Vec<&str> = built
        .artifacts
        .iter()
        .map(|a| a.location.as_str())
        .collect();
    assert_eq!(committed_locations, built_locations);
}
