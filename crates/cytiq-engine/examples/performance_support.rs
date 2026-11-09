//! Fixture generation and strict output comparison for `performance/run.py`.
//! This example is a development tool, not a cytiq subcommand.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use cytiq_fcs::{FcsReader, FcsWriteOptions, FcsWriteParameter};
use cytiq_mask::EventMask;
use cytiq_provenance::{FileRecord, RunRecord};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate fingerprints and every recorded input/output hash in an archive.
    Verify {
        #[arg(long)]
        input: PathBuf,
    },
    Generate {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        events: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        labels: bool,
    },
    Inspect {
        #[arg(long)]
        input: PathBuf,
    },
    Compare {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long, value_enum, default_value_t = ComparisonProfile::Exact)]
        profile: ComparisonProfile,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ComparisonProfile {
    Exact,
    Refactor,
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Verify { input } => {
            let mut paths = vec![input.clone()];
            let mut verified = 0;
            while let Some(path) = paths.pop() {
                if path.is_dir() {
                    for entry in fs::read_dir(&path)? {
                        paths.push(entry?.path());
                    }
                } else if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with("provenance.json"))
                {
                    refactor_provenance(&path, &input)?;
                    verified += 1;
                }
            }
            if verified == 0 {
                return Err("missing archived provenance".into());
            }
            Ok(())
        }
        Command::Generate {
            output,
            events,
            seed,
            labels,
        } => generate_options(&output, events, seed, labels),
        Command::Inspect { input } => {
            let stream = FcsReader::open(input)?.stream(
                NonZeroUsize::new(8192).unwrap(),
                cytiq_fcs::FcsReadLimits::default(),
            )?;
            println!(
                "{}",
                serde_json::json!({"events": stream.metadata.event_count})
            );
            Ok(())
        }
        Command::Compare {
            baseline,
            candidate,
            profile,
        } => match profile {
            ComparisonProfile::Exact => compare(&baseline, &candidate),
            ComparisonProfile::Refactor => compare_refactor(&baseline, &candidate),
        },
    }
}

/// `SplitMix64`: fixed integer operations make the input independent of Rust's
/// random-library version. Generator version 1; record seed and file hashes.
fn next_random(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
fn generate(output: &Path, events: usize, seed: u64) -> Result<()> {
    generate_options(output, events, seed, false)
}

#[allow(clippy::cast_precision_loss)] // Synthetic time/noise are stored as FCS float32.
fn generate_options(output: &Path, events: usize, seed: u64, labels: bool) -> Result<()> {
    if events == 0 {
        return Err("events must be positive".into());
    }
    let mut names = ["Time", "FSC-A", "FSC-H", "SSC-A"]
        .map(str::to_owned)
        .to_vec();
    names.extend((1..=26).map(|index| format!("FL{index}-A")));
    let parameters = names
        .iter()
        .map(|name| {
            if labels {
                FcsWriteParameter::with_label(name, name).with_range(262_144.0)
            } else {
                FcsWriteParameter::new(name).with_range(262_144.0)
            }
        })
        .collect::<Vec<_>>();
    let mut state = seed;
    let columns = (0..names.len())
        .map(|channel| {
            (0..events)
                .map(|event| {
                    if channel == 0 {
                        return event as f32;
                    }
                    let noise = (next_random(&mut state) >> 40) as f32 / 256.0;
                    if channel < 4 {
                        noise + 100.0
                    } else if event % 9973 == 0 {
                        200_000.0 + noise / 10.0
                    } else {
                        noise - 500.0
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut spillover = format!("26,{}", names[4..].join(","));
    for row in 0..26 {
        for column in 0..26 {
            let coefficient = if row == column {
                1.0
            } else if column == (row + 1) % 26 {
                0.02
            } else {
                0.0
            };
            write!(spillover, ",{coefficient}")?;
        }
    }
    let options = FcsWriteOptions::new()
        .with_text_keyword("$SPILLOVER", spillover)
        .with_text_keyword("$TIMESTEP", "0.01");
    cytiq_fcs::write_fcs3_1_f32_with_options(output, &parameters, &columns, &options)?;
    Ok(())
}

fn files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, directory: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &entry.path(), found)?;
            } else if kind.is_file() {
                found.push(entry.path().strip_prefix(root)?.to_path_buf());
            } else {
                return Err(format!("unexpected output type: {}", entry.path().display()).into());
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    visit(root, root, &mut found)?;
    found.sort();
    if found.is_empty() {
        return Err(format!("no output files in {}", root.display()).into());
    }
    Ok(found)
}

fn normalized_provenance(path: &Path) -> Result<serde_json::Value> {
    // Deserialise the contract before removing exactly the three fields that
    // may vary. Missing metadata or changes to config/build/lineage still fail.
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let _: RunRecord = serde_json::from_value(value.clone())?;
    let object = value
        .as_object_mut()
        .ok_or("provenance must be an object")?;
    object.remove("started_at_unix_ms");
    object.remove("finished_at_unix_ms");
    object
        .get_mut("build")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("provenance must contain build")?
        .remove("git_commit");
    Ok(value)
}

fn compare(baseline: &Path, candidate: &Path) -> Result<()> {
    let paths = files(baseline)?;
    if paths != files(candidate)? {
        return Err("output file sets differ".into());
    }
    for relative in paths {
        let left = baseline.join(&relative);
        let right = candidate.join(&relative);
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let equal = if name == "provenance.json" || name.ends_with(".provenance.json") {
            normalized_provenance(&left)? == normalized_provenance(&right)?
        } else if relative.extension().is_some_and(|ext| ext == "parquet") {
            EventMask::read_parquet(&left)? == EventMask::read_parquet(&right)?
        } else {
            // FCS includes its TEXT metadata and numeric metrics. Comparing its
            // bytes catches signed zero and the last bit of every stored value.
            let left = FileRecord::of(&left)?;
            let right = FileRecord::of(&right)?;
            left.bytes == right.bytes && left.sha256 == right.sha256
        };
        if !equal {
            return Err(format!("outputs differ: {}", relative.display()).into());
        }
    }
    Ok(())
}

// Refactor comparisons verify each record before excluding fields that are
// derived from the build or physical encoding. The default comparator above
// deliberately retains its original byte-for-byte policy.
fn refactor_provenance(path: &Path, root: &Path) -> Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let record: RunRecord = serde_json::from_value(value.clone())?;
    if record.reason.is_none()
        || record.schema_version != cytiq_provenance::SCHEMA_VERSION
        || record.execution_fingerprint.as_deref() != Some(record.compute_fingerprint().as_str())
    {
        return Err("invalid execution fingerprint".into());
    }
    let original = root.parent().ok_or("missing archive parent")?.join("work");
    for output in &record.outputs {
        let relative = Path::new(&output.path).strip_prefix(&original)?;
        let actual = FileRecord::of(&root.join(relative))?;
        if actual.bytes != output.bytes || actual.sha256 != output.sha256 {
            return Err(format!("output hash mismatch: {}", output.path).into());
        }
    }
    for input in &record.inputs {
        let actual = FileRecord::of(Path::new(&input.path))?;
        if actual.bytes != input.bytes || actual.sha256 != input.sha256 {
            return Err("input hash mismatch".into());
        }
    }
    let object = value
        .as_object_mut()
        .ok_or("provenance must be an object")?;
    object.remove("execution_fingerprint");
    object.remove("started_at_unix_ms");
    object.remove("finished_at_unix_ms");
    object
        .get_mut("build")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("missing build")?
        .remove("git_commit");
    for output in object
        .get_mut("outputs")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("missing outputs")?
    {
        let output = output.as_object_mut().ok_or("invalid output")?;
        output.remove("bytes");
        output.remove("sha256");
    }
    Ok(value)
}

fn same_float(left: f64, right: f64) -> bool {
    left.is_finite()
        && right.is_finite()
        && (left - right).abs() <= 1e-6 * left.abs().max(right.abs()).max(1.0)
}

fn same_metrics(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (left, right) {
        (Value::Number(a), Value::Number(b)) if a.is_f64() && b.is_f64() => {
            same_float(a.as_f64().unwrap(), b.as_f64().unwrap())
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_metrics(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_metrics(a, b)))
        }
        _ => left == right,
    }
}

fn same_fcs(left: &Path, right: &Path) -> Result<bool> {
    use cytiq_fcs::ColumnData;
    let mut left = FcsReader::open(left)?.read()?;
    let mut right = FcsReader::open(right)?.read()?;
    let a = std::mem::take(&mut left.data.columns);
    let b = std::mem::take(&mut right.data.columns);
    Ok(left == right
        && a.len() == b.len()
        && a.iter().zip(&b).enumerate().all(|(index, (a, b))| {
            if left.parameters[index].name == cytiq_fcs::ORIGINAL_ID_PARAMETER {
                return a == b;
            }
            match (a, b) {
                (ColumnData::UInt(a), ColumnData::UInt(b)) => a == b,
                (ColumnData::F32(a), ColumnData::F32(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|(&a, &b)| same_float(f64::from(a), f64::from(b)))
                }
                (ColumnData::F64(a), ColumnData::F64(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| same_float(a, b))
                }
                _ => false,
            }
        }))
}

fn compare_refactor(baseline: &Path, candidate: &Path) -> Result<()> {
    let paths = files(baseline)?;
    if paths != files(candidate)? {
        return Err("output file sets differ".into());
    }
    for relative in paths {
        let left = baseline.join(&relative);
        let right = candidate.join(&relative);
        let name = relative
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let equal = if name == "provenance.json" || name.ends_with(".provenance.json") {
            refactor_provenance(&left, baseline)? == refactor_provenance(&right, candidate)?
        } else if relative.extension().is_some_and(|ext| ext == "fcs") {
            same_fcs(&left, &right)?
        } else if name == "metrics.json" {
            same_metrics(
                &serde_json::from_slice(&fs::read(left)?)?,
                &serde_json::from_slice(&fs::read(right)?)?,
            )
        } else if relative.extension().is_some_and(|ext| ext == "parquet") {
            let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                fs::File::open(&left)?,
            )?;
            if builder
                .schema()
                .metadata()
                .contains_key("cytiq.population.schema_version")
            {
                cytiq_population::table::read(&left)? == cytiq_population::table::read(&right)?
            } else {
                EventMask::read_parquet(&left)? == EventMask::read_parquet(&right)?
            }
        } else {
            fs::read(&left)? == fs::read(&right)?
        };
        if !equal {
            return Err(format!("outputs differ: {}", relative.display()).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_repeatable_and_compensable() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("a.fcs");
        let right = directory.path().join("b.fcs");
        generate(&left, 8193, 42).unwrap();
        generate(&right, 8193, 42).unwrap();
        assert_eq!(fs::read(&left).unwrap(), fs::read(&right).unwrap());
        let dataset = FcsReader::open(&left).unwrap().read().unwrap();
        assert_eq!(dataset.event_count(), 8193);
        assert_eq!(dataset.parameters.len(), 30);
        assert!(
            cytiq_fcs::Spillover::from_dataset(&dataset)
                .unwrap()
                .unwrap()
                .compensation_matrix()
                .is_ok()
        );
    }

    #[test]
    fn comparison_rejects_empty_missing_and_changed_outputs() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        assert!(compare(&left, &right).is_err());
        fs::write(left.join("metrics.json"), "{\"mean\":-0.0}").unwrap();
        assert!(compare(&left, &right).is_err());
        fs::write(right.join("metrics.json"), "{\"mean\":-0.0}").unwrap();
        compare(&left, &right).unwrap();
        fs::write(right.join("metrics.json"), "{\"mean\":0.0}").unwrap();
        assert!(compare(&left, &right).is_err());
    }

    #[test]
    fn provenance_ignores_only_clock_and_commit() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let mut record = serde_json::json!({
            "schema_version": 2, "tool": "cytiq compensate", "version": "0.2.1",
            "build": {"git_commit": "baseline", "rustc": "same", "target": "same", "profile": "release"},
            "platform": {"os": "same", "arch": "same", "cpus": 4},
            "arguments": [], "config": {"threads": 4}, "constants": {}, "algorithms": {},
            "started_at_unix_ms": 1, "finished_at_unix_ms": 2, "inputs": [], "outputs": []
        });
        fs::write(
            left.join("provenance.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        record["build"]["git_commit"] = "candidate".into();
        record["started_at_unix_ms"] = 3.into();
        record["finished_at_unix_ms"] = 4.into();
        fs::write(
            right.join("provenance.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        compare(&left, &right).unwrap();
        record["config"]["threads"] = 1.into();
        fs::write(
            right.join("provenance.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(compare(&left, &right).is_err());
        record["config"]["threads"] = 4.into();
        record["build"]["profile"] = "debug".into();
        fs::write(
            right.join("provenance.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(compare(&left, &right).is_err());
    }
    #[test]
    fn refactor_metrics_keep_integers_and_document_hashes_exact() {
        use serde_json::json;
        assert!(same_metrics(
            &json!({"value":1.0,"n":9007199254740993u64}),
            &json!({"value":1.0000001,"n":9007199254740993u64})
        ));
        assert!(!same_metrics(
            &json!({"n":9007199254740993u64}),
            &json!({"n":9007199254740992u64})
        ));
        assert!(!same_metrics(
            &json!({"sha256":"a"}),
            &json!({"sha256":"b"})
        ));
        assert!(!same_float(f64::NAN, 0.0));
        assert!(!same_float(1.0, 1.01));
    }
    #[test]
    fn refactor_provenance_verifies_changed_encodings_and_rejects_tampering() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut records = Vec::new();
        for (side, bytes) in [
            ("baseline", b"{\"x\":1.0}".as_slice()),
            ("candidate", b"{\"x\":1.0000001}".as_slice()),
        ] {
            let archive = root.join(side);
            fs::create_dir(&archive).unwrap();
            fs::write(archive.join("metrics.json"), bytes).unwrap();
            let mut output = FileRecord::of(&archive.join("metrics.json")).unwrap();
            output.path = root
                .join("work/metrics.json")
                .to_string_lossy()
                .into_owned();
            let mut record:RunRecord=serde_json::from_value(serde_json::json!({
                "schema_version":3,"tool":"cytiq test","version":"same",
                "build":{"git_commit":side,"rustc":"same","target":"same","profile":"release"},
                "platform":{"os":"same","arch":"same","cpus":4},"arguments":[],"config":{},"constants":{},"algorithms":{},
                "started_at_unix_ms":1,"finished_at_unix_ms":2,"inputs":[],"outputs":[output],"reason":"success"
            })).unwrap();
            record.execution_fingerprint = Some(record.compute_fingerprint());
            fs::write(
                archive.join("provenance.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            records.push(record);
        }
        let baseline = root.join("baseline");
        let candidate = root.join("candidate");
        compare_refactor(&baseline, &candidate).unwrap();
        assert!(compare(&baseline, &candidate).is_err());
        fs::write(candidate.join("metrics.json"), b"{\"x\":1.0}").unwrap();
        assert!(compare_refactor(&baseline, &candidate).is_err());
        let mut record = records.pop().unwrap();
        record.execution_fingerprint = Some("invalid".into());
        fs::write(
            candidate.join("provenance.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(refactor_provenance(&candidate.join("provenance.json"), &candidate).is_err());
    }
}
