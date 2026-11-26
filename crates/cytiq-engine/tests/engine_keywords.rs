//! Every TEXT keyword the engine reads is one a file must not repeat.
//!
//! `cytiq_fcs::is_engine_keyword` decides which repeated keywords a file is
//! refused for. A lookup of a keyword it does not know would read a value that
//! quarantine hides, so the sources are searched for lookups.

use std::fs;
use std::path::{Path, PathBuf};

fn sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    let mut pending: Vec<PathBuf> = fs::read_dir(crates)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("src"))
        .filter(|path| path.is_dir())
        .collect();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// `(arguments before it, literal)` of every call whose name is `call` and that
/// passes a string literal.
fn literals<'a>(source: &'a str, call: &str) -> Vec<(&'a str, &'a str)> {
    let mut found = Vec::new();
    for (at, _) in source.match_indices(call) {
        // `parse_keyword(` is not `keyword(`.
        let part_of_a_name = source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if part_of_a_name {
            continue;
        }
        let arguments = &source[at + call.len()..];
        let arguments = &arguments[..arguments.find(')').unwrap_or(arguments.len())];
        if let Some(open) = arguments.find('"')
            && let Some(length) = arguments[open + 1..].find('"')
        {
            found.push((&arguments[..open], &arguments[open + 1..open + 1 + length]));
        }
    }
    found
}

/// A keyword with its `format!` placeholders filled in, or the `$Pn` keyword
/// of a suffix looked up on a parameter.
fn keyword(arguments: &str, literal: &str) -> String {
    let on_a_parameter = arguments.contains("parameter") && !arguments.contains("format!");
    let suffix = literal.bytes().all(|byte| byte.is_ascii_uppercase());
    if on_a_parameter && suffix {
        return format!("$P1{literal}");
    }
    // `flowCore_$P{n}{suffix}` in `margins.rs`.
    if ["Rmin", "Rmax"].contains(&literal) {
        return format!("flowCore_$P1{literal}");
    }
    literal.replace("{}", "1")
}

#[test]
fn every_looked_up_keyword_must_not_be_repeated() {
    let mut looked_up = Vec::new();
    for path in sources() {
        let source = fs::read_to_string(&path).unwrap();
        // Rust formatting may break the line before `.get`.
        let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
        let source = source.replace("text .get(", "text.get(");
        for call in ["text.get(", "keyword(", "keyword_f64(", "keyword_value("] {
            for (arguments, literal) in literals(&source, call) {
                looked_up.push((path.clone(), keyword(arguments, literal)));
            }
        }
        for (at, _) in source.match_indices("KEYWORD: &str = \"") {
            let value = &source[at + "KEYWORD: &str = \"".len()..];
            let value = &value[..value.find('"').unwrap()];
            looked_up.push((path.clone(), value.to_owned()));
        }
    }
    let names: Vec<&str> = looked_up.iter().map(|(_, key)| key.as_str()).collect();
    for expected in [
        "$NEXTDATA",
        "$P1TYPE",
        "$P1R",
        "CYT",
        "OriginalInstrument",
        "CYTIQ_TRANSFORM",
        "flowCore_$P1Rmax",
        "maxRange",
    ] {
        assert!(names.contains(&expected), "the search misses `{expected}`");
    }
    for (path, key) in &looked_up {
        assert!(
            cytiq_fcs::is_engine_keyword(key),
            "{} looks up `{key}`, which a file may repeat: add it to \
             cytiq-fcs/src/keywords.rs",
            path.display()
        );
    }
}

/// `pairs` and the segments show a file; only `iter` and `get` are lookups.
#[test]
fn only_the_reader_and_meta_see_quarantined_pairs() {
    for path in sources() {
        let name = path.to_string_lossy().replace('\\', "/");
        if name.contains("/cytiq-fcs/src/") || name.ends_with("/cytiq-inspect/src/meta/runner.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for access in [
            "text.pairs()",
            "text.primary",
            "text.supplemental",
            ".primary()",
        ] {
            assert!(!source.contains(access), "{name} reads `{access}`");
        }
    }
}
