//! `cytiq.events.v1` export settings: bit exactness in every stored type,
//! determinism, projections, codecs, verifier limits and layers split into
//! parts.

use std::fs::File;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};

use arrow_array::{Array, Float32Array, Float64Array, RecordBatch, UInt64Array};
use cytiq_fcs::{ColumnData, FcsReader, FcsWriteParameter};
use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_population::events::{
    self, Codec, ExportOptions, ExportSummary, LayerState, PartsManifest, RowSet, WriteSettings,
};
use cytiq_population::issue::{IssueCode, Issues, Severity};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};

/// A minimal FCS 3.1 file: one `$DATATYPE` for every parameter,
/// little-endian, `data` row-major.
fn fcs(path: &Path, datatype: &str, bits: u32, names: &[&str], events: usize, data: &[u8]) {
    let placeholder = "00000000";
    let mut pairs: Vec<(String, String)> = [
        ("$BEGINANALYSIS", "0"),
        ("$ENDANALYSIS", "0"),
        ("$BEGINSTEXT", "0"),
        ("$ENDSTEXT", "0"),
        ("$BEGINDATA", placeholder),
        ("$ENDDATA", placeholder),
        ("$BYTEORD", "1,2,3,4"),
        ("$DATATYPE", datatype),
        ("$MODE", "L"),
        ("$NEXTDATA", "0"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    pairs.push(("$PAR".into(), names.len().to_string()));
    pairs.push(("$TOT".into(), events.to_string()));
    let range = if datatype == "I" {
        u64::MAX.to_string()
    } else {
        "262144".to_owned()
    };
    for (i, name) in names.iter().enumerate() {
        let n = i + 1;
        pairs.push((format!("$P{n}B"), bits.to_string()));
        pairs.push((format!("$P{n}E"), "0,0".into()));
        pairs.push((format!("$P{n}N"), (*name).to_owned()));
        pairs.push((format!("$P{n}R"), range.clone()));
    }
    let encode = |pairs: &[(String, String)]| {
        let mut text = String::from("/");
        for (key, value) in pairs {
            text.push_str(key);
            text.push('/');
            text.push_str(value);
            text.push('/');
        }
        text
    };
    let text_start = 58;
    let text_end = text_start + encode(&pairs).len() - 1;
    // An empty DATA segment has zero offsets.
    let (data_start, data_end) = if data.is_empty() {
        (0, 0)
    } else {
        (text_end + 1, text_end + data.len())
    };
    pairs[4].1 = format!("{data_start:08}");
    pairs[5].1 = format!("{data_end:08}");
    let text = encode(&pairs);
    let header = format!(
        "FCS3.1    {text_start:>8}{text_end:>8}{data_start:>8}{data_end:>8}{:>8}{:>8}",
        0, 0
    );
    let mut bytes = header.into_bytes();
    bytes.extend(text.as_bytes());
    bytes.extend(data);
    std::fs::write(path, bytes).unwrap();
}

/// A float32 file of `events` events on `channels` channels.
fn sample(directory: &Path, name: &str, channels: usize, events: usize) -> PathBuf {
    let path = directory.join(name);
    if events == 0 {
        let names: Vec<String> = (1..=channels).map(|i| format!("C{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        fcs(&path, "F", 32, &names, 0, &[]);
        return path;
    }
    let parameters: Vec<FcsWriteParameter> = (1..=channels)
        .map(|i| FcsWriteParameter::new(format!("C{i}")))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let columns: Vec<Vec<f32>> = (0..channels)
        .map(|c| (0..events).map(|e| (e * 31 + c * 7) as f32 / 3.0).collect())
        .collect();
    cytiq_fcs::write_fcs3_1_f32(&path, &parameters, &columns).unwrap();
    path
}

fn export_with(input: &Path, output: &Path, settings: &WriteSettings) -> ExportSummary {
    events::export_with(
        input,
        output,
        &ExportOptions::default(),
        settings,
        ParquetReadLimits::default(),
    )
    .unwrap()
}

fn verify(path: &Path) -> events::EventsSummary {
    let mut issues = Issues::new();
    events::verify_file(path, "", &mut issues)
        .unwrap_or_else(|| panic!("{}: {:?}", path.display(), issues.items()))
}

/// Every batch, through the standard arrow-rs reader.
fn arrow(path: &Path) -> Vec<RecordBatch> {
    ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
        .unwrap()
        .build()
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Column `index` of every batch as bit patterns.
fn bits(batches: &[RecordBatch], index: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for batch in batches {
        let any = batch.column(index).as_any();
        if let Some(values) = any.downcast_ref::<UInt64Array>() {
            out.extend(values.values().iter().copied());
        } else if let Some(values) = any.downcast_ref::<Float32Array>() {
            out.extend(values.values().iter().map(|v| u64::from(v.to_bits())));
        } else {
            let values = any.downcast_ref::<Float64Array>().unwrap();
            out.extend(values.values().iter().map(|v| v.to_bits()));
        }
    }
    out
}

fn fcs_bits(column: &ColumnData) -> Vec<u64> {
    match column {
        ColumnData::UInt(values) => values.clone(),
        ColumnData::F32(values) => values.iter().map(|v| u64::from(v.to_bits())).collect(),
        ColumnData::F64(values) => values.iter().map(|v| v.to_bits()).collect(),
    }
}

#[test]
fn every_stored_type_keeps_its_exact_bits() {
    let directory = tempfile::tempdir().unwrap();
    let f32_values = [
        -0.0_f32,
        f32::from_bits(1),           // smallest subnormal
        f32::from_bits(0x7fc0_1234), // NaN with a payload
        f32::from_bits(0xff80_0001), // signalling NaN, sign set
        f32::INFINITY,
        f32::MAX,
    ];
    let f64_values = [
        -0.0_f64,
        f64::from_bits(1),
        f64::from_bits(0x7ff8_0000_dead_beef),
        f64::NEG_INFINITY,
        f64::MAX,
        std::f64::consts::PI,
    ];
    let u64_values = [0_u64, 1, 1 << 53 | 1, 1 << 63, u64::MAX - 1, u64::MAX];
    let cases: [(&str, u32, Vec<u8>); 3] = [
        (
            "F",
            32,
            f32_values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        ),
        (
            "D",
            64,
            f64_values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        ),
        (
            "I",
            64,
            u64_values.iter().flat_map(|v| v.to_le_bytes()).collect(),
        ),
    ];
    for (datatype, bits_per_value, data) in cases {
        // Two channels of three events each, row-major.
        let input = directory.path().join(format!("{datatype}.fcs"));
        fcs(&input, datatype, bits_per_value, &["A", "B"], 3, &data);
        let source = FcsReader::open(&input).unwrap().read().unwrap();
        for codec in [Codec::Zstd(3), Codec::Snappy] {
            let output = directory
                .path()
                .join(format!("{datatype}-{codec:?}.parquet"));
            let settings = WriteSettings {
                codec,
                ..WriteSettings::default()
            };
            let summary = export_with(&input, &output, &settings);
            assert_eq!(verify(&output).values_sha256, summary.values_sha256);
            let batches = arrow(&output);
            for (index, column) in source.data.columns.iter().enumerate() {
                assert_eq!(
                    bits(&batches, index + 3),
                    fcs_bits(column),
                    "{datatype} {codec:?} column {index}"
                );
            }
            let expected = match codec {
                Codec::Zstd(_) => "ZSTD",
                Codec::Snappy => "SNAPPY",
            };
            let reader = SerializedFileReader::new(File::open(&output).unwrap()).unwrap();
            let compression = reader.metadata().row_group(0).column(3).compression();
            assert!(
                format!("{compression:?}").starts_with(expected),
                "{compression:?}"
            );
        }
    }
}

#[test]
fn unusual_channel_names_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let long = "L".repeat(180);
    let names = [
        "FSC-A",
        "CD3 FITC",
        "CD4·PE-Cy7 (α)",
        "FL1.INT_CD14.FITC",
        "Quote\"d 'name'",
        "a,b;c",
        long.as_str(),
    ];
    let parameters: Vec<FcsWriteParameter> = names
        .iter()
        .map(|name| FcsWriteParameter::new(*name))
        .collect();
    let columns = vec![vec![1.0_f32, 2.0]; names.len()];
    let input = directory.path().join("names.fcs");
    cytiq_fcs::write_fcs3_1_f32(&input, &parameters, &columns).unwrap();
    let output = directory.path().join("names.parquet");
    let summary = export_with(&input, &output, &WriteSettings::default());
    let stored: Vec<&str> = summary.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(stored, names);
    let batches = arrow(&output);
    let schema = batches[0].schema();
    let columns: Vec<&str> = schema.fields()[3..]
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    assert_eq!(columns, names);
    verify(&output);
}

#[test]
fn exports_are_deterministic_and_batches_change_no_value() {
    let directory = tempfile::tempdir().unwrap();
    let input = sample(directory.path(), "s.fcs", 5, 1000);
    let first = directory.path().join("a.parquet");
    let second = directory.path().join("b.parquet");
    let reference = export_with(&input, &first, &WriteSettings::default());
    export_with(&input, &second, &WriteSettings::default());
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "the same input and settings give the same bytes"
    );
    let content = |path: &Path| -> Vec<Vec<u64>> {
        let batches = arrow(path);
        (2..8).map(|index| bits(&batches, index)).collect()
    };
    for (batch, group) in [(1, 1000), (7, 64), (65_536, 3)] {
        let output = directory.path().join(format!("batch{batch}.parquet"));
        let settings = WriteSettings {
            batch_rows: NonZeroUsize::new(batch),
            row_group_rows: NonZeroUsize::new(group).unwrap(),
            ..WriteSettings::default()
        };
        let summary = export_with(&input, &output, &settings);
        assert_eq!(
            summary.values_sha256, reference.values_sha256,
            "batch {batch}"
        );
        assert_eq!(content(&output), content(&first), "batch {batch}");
        let groups = SerializedFileReader::new(File::open(&output).unwrap())
            .unwrap()
            .metadata()
            .num_row_groups();
        assert_eq!(groups, 1000_usize.div_ceil(group), "batch {batch}");
    }
}

#[test]
fn projections_keep_the_layer_and_pn_order() {
    let directory = tempfile::tempdir().unwrap();
    let input = sample(directory.path(), "s.fcs", 4, 10);
    let full = directory.path().join("full.parquet");
    let all = export_with(&input, &full, &WriteSettings::default());
    let output = directory.path().join("projection.parquet");
    let settings = WriteSettings {
        channels: Some(vec!["C4".into(), "C2".into()]),
        ..WriteSettings::default()
    };
    let summary = export_with(&input, &output, &settings);
    assert_eq!(summary.layer_id, all.layer_id);
    let verified = verify(&output);
    assert!(!verified.all_columns);
    let stored: Vec<&str> = verified.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(stored, ["C2", "C4"]);
    assert_eq!(bits(&arrow(&output), 4), bits(&arrow(&full), 6));
    // Selecting every channel is the whole table.
    let every = WriteSettings {
        channels: Some(vec!["C3".into(), "C1".into(), "C4".into(), "C2".into()]),
        ..WriteSettings::default()
    };
    let output = directory.path().join("every.parquet");
    export_with(&input, &output, &every);
    assert!(verify(&output).all_columns);
}

#[test]
fn the_channel_dictionary_is_readable_without_arrow_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("labelled.fcs");
    let parameters = vec![
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::with_label("FL1-A", "CD3")
            .with_keyword("TAG", "FITC")
            .with_keyword("DET", "B530"),
    ];
    cytiq_fcs::write_fcs3_1_f32(&input, &parameters, &[vec![1.0], vec![2.0]]).unwrap();
    let output = directory.path().join("labelled.parquet");
    export_with(&input, &output, &WriteSettings::default());
    let reader = SerializedFileReader::new(File::open(&output).unwrap()).unwrap();
    let footer = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap();
    let value = |key: &str| {
        footer
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.clone())
            .unwrap_or_else(|| panic!("{key} is missing"))
    };
    let dictionary: serde_json::Value = serde_json::from_str(&value(events::CHANNELS_KEY)).unwrap();
    cytiq_population::dataset::conforms(&dictionary, "events-channels.schema.json").unwrap();
    let mut extra = dictionary.clone();
    extra[0]["comment"] = "free text".into();
    assert!(
        cytiq_population::dataset::conforms(&extra, "events-channels.schema.json").is_err(),
        "the dictionary takes no other field"
    );
    let channel = &dictionary[1];
    for (key, expected) in [
        ("index", serde_json::json!(2)),
        ("name", "FL1-A".into()),
        ("value_type", "float32".into()),
        ("kind", "unspecified".into()),
        ("label", "CD3".into()),
        ("reporter", "FITC".into()),
        ("detector", "B530".into()),
    ] {
        assert_eq!(channel[key], expected, "{key}");
    }
    assert!(
        channel.get("analyte").is_none(),
        "absent keywords are omitted"
    );
    assert_eq!(value(events::COLUMN_SET_KEY), "all");
    assert_eq!(
        value(events::SOURCE_SHA256_KEY),
        cytiq_provenance::hash_file(&input).unwrap().1
    );
}

#[test]
fn tables_the_verifier_would_refuse_are_refused_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let input = sample(directory.path(), "s.fcs", 2, 10);
    let output = directory.path().join("refused.parquet");
    let limits = ParquetReadLimits {
        rows: 4,
        ..ParquetReadLimits::default()
    };
    let error = events::export_with(
        &input,
        &output,
        &ExportOptions::default(),
        &WriteSettings::default(),
        limits,
    )
    .unwrap_err();
    assert!(error.to_string().contains("--max-file-bytes"), "{error}");
    assert!(!output.exists(), "nothing is written before the refusal");
    // Parts of four events each fit.
    let parts = directory.path().join("parts");
    std::fs::create_dir(&parts).unwrap();
    let settings = WriteSettings {
        max_file_bytes: NonZeroU64::new(4 * (8 + 2 * 4)),
        ..WriteSettings::default()
    };
    let summary =
        events::export_with(&input, &parts, &ExportOptions::default(), &settings, limits).unwrap();
    assert_eq!(summary.parts.unwrap().parts.len(), 3);
}

#[test]
fn processed_files_without_their_acquisition_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("transformed.fcs");
    let options = cytiq_fcs::FcsWriteOptions::new().with_text_keyword(
        "CYTIQ_TRANSFORM",
        r#"{"method":"arcsinh","parameters":{"cofactor":150},"files":[{"channels":["A"]}]}"#,
    );
    cytiq_fcs::write_fcs3_1_f32_with_options(
        &input,
        &[FcsWriteParameter::new("A")],
        &[vec![1.0]],
        &options,
    )
    .unwrap();
    let error = events::export(
        &input,
        &directory.path().join("t.parquet"),
        &ExportOptions {
            layer: LayerState::Transformed,
            ..ExportOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("CYTIQ_ACQUISITION"), "{error}");
}

/// Export `events` events in parts of `per_part` events.
fn split(
    directory: &Path,
    events: usize,
    per_part: u64,
) -> (PathBuf, ExportSummary, ExportSummary) {
    let input = sample(directory, &format!("s{events}.fcs"), 3, events);
    let single = directory.join(format!("single{events}.parquet"));
    let whole = export_with(&input, &single, &WriteSettings::default());
    let parts = directory.join(format!("parts{events}"));
    std::fs::create_dir(&parts).unwrap();
    let settings = WriteSettings {
        max_file_bytes: NonZeroU64::new(per_part * (8 + 3 * 4)),
        row_group_rows: NonZeroUsize::new(2).unwrap(),
        ..WriteSettings::default()
    };
    let summary = export_with(&input, &parts, &settings);
    (parts, whole, summary)
}

fn verify_parts(directory: &Path) -> (Option<events::EventsSummary>, Issues) {
    let mut issues = Issues::new();
    let summary = events::verify_parts(&directory.join(events::PARTS_FILE), "", &mut issues);
    (summary, issues)
}

#[test]
fn split_layers_equal_one_table() {
    let directory = tempfile::tempdir().unwrap();
    for (events, per_part, expected) in [(1, 3, 1), (3, 3, 1), (9, 3, 3), (10, 3, 4)] {
        let (parts, whole, summary) = split(directory.path(), events, per_part);
        let manifest = summary.parts.clone().unwrap();
        assert_eq!(manifest.parts.len(), expected, "{events} events");
        assert_eq!(
            summary.values_sha256, whole.values_sha256,
            "{events} events"
        );
        assert_eq!(manifest.values_sha256, whole.values_sha256);
        assert_eq!(manifest.row_set, RowSet::All);
        assert_eq!(manifest.raw_events, Some(events as u64));
        assert_eq!(
            PartsManifest::read(&parts.join(events::PARTS_FILE)).unwrap(),
            manifest
        );
        let (layer, issues) = verify_parts(&parts);
        let layer = layer.unwrap_or_else(|| panic!("{events}: {:?}", issues.items()));
        assert_eq!(layer.rows, events as u64);
        assert_eq!(layer.layer_id, whole.layer_id);
        assert_eq!(
            layer.max_source_event_id,
            events.checked_sub(1).map(|e| e as u64)
        );
        let mut next = 0;
        for (index, part) in manifest.parts.iter().enumerate() {
            assert_eq!(part.location, events::part_name(index as u64));
            if part.rows > 0 {
                assert_eq!(part.first_source_event_id, Some(next));
                next += part.rows;
                assert_eq!(part.last_source_event_id, Some(next - 1));
            }
            // A part is a table of its own; only a lone part is the whole layer.
            let table = verify(&parts.join(&part.location));
            assert_eq!(table.raw_events.is_some(), manifest.parts.len() == 1);
        }
    }
}

fn codes(issues: &Issues) -> std::collections::BTreeSet<IssueCode> {
    issues.codes(Severity::Error)
}

fn edit_manifest(parts: &Path, change: impl FnOnce(&mut PartsManifest)) {
    let path = parts.join(events::PARTS_FILE);
    let mut manifest = PartsManifest::read(&path).unwrap();
    change(&mut manifest);
    manifest.write(&path).unwrap();
}

#[test]
fn tampered_parts_are_reported_by_code() {
    let directory = tempfile::tempdir().unwrap();
    let fresh = |name: &str| {
        let root = directory.path().join(name);
        std::fs::create_dir(&root).unwrap();
        let (parts, _, _) = split(&root, 9, 3);
        parts
    };
    let expect = |parts: &Path, code: IssueCode| {
        let (summary, issues) = verify_parts(parts);
        assert!(summary.is_none(), "{code:?}");
        assert!(
            codes(&issues).contains(&code),
            "{code:?}: {:?}",
            issues.items()
        );
    };

    let parts = fresh("missing");
    std::fs::remove_file(parts.join(events::part_name(1))).unwrap();
    expect(&parts, IssueCode::MissingObject);

    let parts = fresh("swapped");
    std::fs::rename(parts.join(events::part_name(0)), parts.join("tmp")).unwrap();
    std::fs::rename(
        parts.join(events::part_name(1)),
        parts.join(events::part_name(0)),
    )
    .unwrap();
    std::fs::rename(parts.join("tmp"), parts.join(events::part_name(1))).unwrap();
    expect(&parts, IssueCode::CorruptArtifactHash);

    let parts = fresh("renamed");
    edit_manifest(&parts, |manifest| manifest.parts.swap(0, 1));
    expect(&parts, IssueCode::InvalidReference);

    let parts = fresh("gap");
    edit_manifest(&parts, |manifest| {
        manifest.parts.remove(1);
        manifest.parts[1].location = events::part_name(1);
        manifest.rows -= 3;
        manifest.raw_events = Some(manifest.rows);
    });
    std::fs::rename(
        parts.join(events::part_name(2)),
        parts.join(events::part_name(1)),
    )
    .unwrap();
    expect(&parts, IssueCode::DuplicateEventIdentity);

    let parts = fresh("digest");
    edit_manifest(&parts, |manifest| manifest.values_sha256 = "0".repeat(64));
    expect(&parts, IssueCode::CorruptArtifactHash);

    let parts = fresh("contract");
    edit_manifest(&parts, |manifest| {
        manifest.contract = "cytiq.events.parts.v9".into();
    });
    expect(&parts, IssueCode::UnsupportedSchema);

    // A part of another layer, with its own bytes recorded.
    let parts = fresh("foreign");
    let other = directory.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let input = other.join("other.fcs");
    let parameters: Vec<FcsWriteParameter> = (1..=3)
        .map(|i| FcsWriteParameter::new(format!("C{i}")))
        .collect();
    cytiq_fcs::write_fcs3_1_f32(&input, &parameters, &vec![vec![5.0_f32; 3]; 3]).unwrap();
    let foreign = other.join("foreign.parquet");
    export_with(&input, &foreign, &WriteSettings::default());
    std::fs::copy(&foreign, parts.join(events::part_name(2))).unwrap();
    let (bytes, sha256) = cytiq_provenance::hash_file(&foreign).unwrap();
    edit_manifest(&parts, |manifest| {
        manifest.parts[2].bytes = bytes;
        manifest.parts[2].sha256 = sha256;
    });
    expect(&parts, IssueCode::CorruptArtifactHash);

    std::fs::write(parts.join(events::PARTS_FILE), b"{\"contract\": 1}").unwrap();
    expect(&parts, IssueCode::CorruptArtifactHash);
}

#[test]
fn files_without_events_are_refused_and_write_nothing() {
    // cytiq-fcs requires a DATA segment: a file of no events has none.
    let directory = tempfile::tempdir().unwrap();
    let input = sample(directory.path(), "empty.fcs", 2, 0);
    let output = directory.path().join("empty.parquet");
    let error = events::export(&input, &output, &ExportOptions::default()).unwrap_err();
    assert!(error.to_string().contains("$BEGINDATA"), "{error}");
    assert!(!output.exists());
}

#[test]
fn parts_directories_must_be_empty() {
    let directory = tempfile::tempdir().unwrap();
    let input = sample(directory.path(), "s.fcs", 1, 4);
    let parts = directory.path().join("parts");
    std::fs::create_dir(&parts).unwrap();
    std::fs::write(parts.join("stray"), b"").unwrap();
    let settings = WriteSettings {
        max_file_bytes: NonZeroU64::new(1 << 20),
        ..WriteSettings::default()
    };
    let error = events::export_with(
        &input,
        &parts,
        &ExportOptions::default(),
        &settings,
        ParquetReadLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not empty"), "{error}");
}
