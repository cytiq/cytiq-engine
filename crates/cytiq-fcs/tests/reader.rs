use std::error::Error;
use std::num::NonZero;
use std::path::PathBuf;

use cytiq_fcs::{
    ColumnData, Error as FcsError, FcsReadLimits, FcsReader, FcsVersion, FcsWriteOptions,
    FcsWriteParameter,
};

mod common;

use common::{build_fcs, encode_u16_rows, overwrite_header_range};

#[test]
fn reads_fcs3_0_integer_fixture() -> Result<(), Box<dyn Error>> {
    let dataset = FcsReader::open(fixture_path("fcs3_0_integer.fcs"))?.read()?;

    assert_eq!(dataset.version, FcsVersion::V3_0);
    assert_eq!(dataset.parameters.len(), 2);
    assert_eq!(dataset.parameters[0].name, "FSC-A");
    assert_eq!(dataset.parameters[1].name, "SSC-A");

    assert_uint_column(dataset.data.column(0), &[1, 2, 3]);
    assert_uint_column(dataset.data.column(1), &[10, 20, 30]);

    Ok(())
}

#[test]
fn reads_fcs3_1_variable_integer_fixture() -> Result<(), Box<dyn Error>> {
    let dataset = FcsReader::open(fixture_path("fcs3_1_variable_integer.fcs"))?.read()?;

    assert_eq!(dataset.version, FcsVersion::V3_1);
    assert_eq!(dataset.parameters.len(), 2);
    assert_uint_column(dataset.data.column(0), &[7, 8]);
    assert_uint_column(dataset.data.column(1), &[1000, 2000]);

    Ok(())
}

#[test]
fn reads_fcs3_2_mixed_fixture() -> Result<(), Box<dyn Error>> {
    let dataset = FcsReader::open(fixture_path("fcs3_2_mixed.fcs"))?.read()?;

    assert_eq!(dataset.version, FcsVersion::V3_2);
    assert_eq!(dataset.parameters.len(), 3);
    assert_uint_column(dataset.data.column(0), &[1, 2]);
    assert_f32_column(dataset.data.column(1), &[1.5, 3.5]);
    assert_f64_column(dataset.data.column(2), &[2.5, 4.5]);
    assert!(
        dataset.parameters[1]
            .keywords
            .iter()
            .any(|(key, value)| key == "$P2DATATYPE" && value == "F")
    );
    assert!(
        dataset.parameters[2]
            .keywords
            .iter()
            .any(|(key, value)| key == "$P3DATATYPE" && value == "D")
    );

    Ok(())
}

#[test]
fn merges_supplemental_text_keywords() -> Result<(), Box<dyn Error>> {
    let bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        Some(vec![("EXTRA", "value")]),
        encode_u16_rows(&[[42]], true),
        None,
    );

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(dataset.text.get("EXTRA"), Some("value"));

    Ok(())
}

/// One event of one 16-bit parameter, with the given `$NEXTDATA`.
fn single_event_with_next_data(next_data: &str) -> Vec<u8> {
    build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", next_data),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[42]], true),
        None,
    )
}

#[test]
fn files_with_more_than_one_data_set_are_refused() {
    // The first data set names the second by its offset. Reading only the
    // first would drop the second's events without a word.
    let length = single_event_with_next_data("00000000").len();
    let mut bytes = single_event_with_next_data(&format!("{length:08}"));
    assert_eq!(bytes.len(), length);
    bytes.extend(single_event_with_next_data("00000000"));

    let error = FcsReader::new(std::io::Cursor::new(&bytes))
        .read()
        .unwrap_err();
    assert!(
        matches!(error, FcsError::UnsupportedLayout(message) if message.contains("$NEXTDATA")),
        "{error}"
    );
    let streamed = FcsReader::new(std::io::Cursor::new(&bytes))
        .stream(NonZero::<usize>::MIN, FcsReadLimits::default())
        .map(|_| ());
    assert!(matches!(streamed, Err(FcsError::UnsupportedLayout(_))));
}

#[test]
fn a_next_data_offset_that_is_not_a_number_is_refused() {
    for value in ["next", "-1", "1.5"] {
        let bytes = single_event_with_next_data(value);
        let error = FcsReader::new(std::io::Cursor::new(bytes))
            .read()
            .unwrap_err();
        assert!(
            matches!(&error, FcsError::InvalidKeywordValue { key, .. } if key == "$NEXTDATA"),
            "{value}: {error}"
        );
    }
}

#[test]
fn zero_next_data_offsets_are_read() -> Result<(), Box<dyn Error>> {
    for value in ["0", "00000000", "000000000000", "           0"] {
        let bytes = single_event_with_next_data(value);
        let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
        assert_uint_column(dataset.data.column(0), &[42]);
    }
    Ok(())
}

#[test]
fn keyword_names_are_case_insensitive() -> Result<(), Box<dyn Error>> {
    // FCS 3.1, 3.2.10: a reader must ignore keyword case. Optional keywords
    // in another case were dropped without a word.
    let bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$byteord", "1,2,3,4"),
            ("$DataType", "I"),
            ("$mode", "L"),
            ("$NextData", "00000000"),
            ("$par", "1"),
            ("$tot", "1"),
            ("$p1b", "16"),
            ("$p1e", "0,0"),
            ("$p1n", "FL1-A"),
            ("$p1r", "1024"),
            ("$p1s", "CD3"),
            ("$P1g", "2.0"),
            ("$timestep", "0.01"),
            ("Vendor", "Kept As Written"),
        ],
        None,
        encode_u16_rows(&[[42]], true),
        None,
    );

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_uint_column(dataset.data.column(0), &[42]);
    let parameter = &dataset.parameters[0];
    assert_eq!(parameter.name, "FL1-A");
    assert_eq!(parameter.label.as_deref(), Some("CD3"));
    for pair in [("$P1G", "2.0"), ("$P1S", "CD3")] {
        let pair = (pair.0.to_owned(), pair.1.to_owned());
        assert!(parameter.keywords.contains(&pair), "{pair:?}");
    }
    assert_eq!(dataset.text.get("$TIMESTEP"), Some("0.01"));
    assert_eq!(dataset.text.get("VENDOR"), Some("Kept As Written"));
    // TEXT itself is kept as written.
    assert!(dataset.text.iter().any(|(key, _)| key == "$p1s"));
    Ok(())
}

#[test]
fn keywords_that_differ_only_by_case_are_duplicates() {
    let bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "FL1-A"),
            ("$P1R", "1024"),
            ("$SPILLOVER", "1,FL1-A,1"),
            ("$spillover", "1,FL1-A,2"),
        ],
        None,
        encode_u16_rows(&[[42]], true),
        None,
    );
    let error = FcsReader::new(std::io::Cursor::new(bytes))
        .read()
        .unwrap_err();
    assert!(
        matches!(&error, cytiq_fcs::Error::DuplicateKeyword(key) if key == "$spillover"),
        "{error}"
    );
}

#[test]
fn reads_analysis_segment_bytes() -> Result<(), Box<dyn Error>> {
    let bytes = build_fcs(
        "FCS3.0",
        vec![
            ("$BYTEORD", "1,2"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[7]], true),
        Some(vec![0x41, 0x4e, 0x41, 0x4c]),
    );

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(dataset.analysis, Some(b"ANAL".to_vec()));

    Ok(())
}

/// TEXT pairs for list-mode DATA of `data_type` with one `$PnB` per parameter.
fn layout_pairs(
    byte_order: &str,
    data_type: &str,
    rows: usize,
    widths: &[&str],
) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("$BYTEORD".to_owned(), byte_order.to_owned()),
        ("$DATATYPE".to_owned(), data_type.to_owned()),
        ("$MODE".to_owned(), "L".to_owned()),
        ("$NEXTDATA".to_owned(), "0".to_owned()),
        ("$PAR".to_owned(), widths.len().to_string()),
        ("$TOT".to_owned(), rows.to_string()),
    ];
    for (index, width) in widths.iter().enumerate() {
        let n = index + 1;
        pairs.push((format!("$P{n}B"), (*width).to_owned()));
        pairs.push((format!("$P{n}E"), "0,0".to_owned()));
        pairs.push((format!("$P{n}N"), format!("P{n}")));
        pairs.push((format!("$P{n}R"), "1024".to_owned()));
    }
    pairs
}

fn read_layout(
    version: &str,
    pairs: &[(String, String)],
    data: Vec<u8>,
) -> Result<cytiq_fcs::FcsDataset, cytiq_fcs::Error> {
    let pairs = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    FcsReader::new(std::io::Cursor::new(build_fcs(
        version, pairs, None, data, None,
    )))
    .read()
}

#[test]
fn reads_ascii_data() -> Result<(), Box<dyn Error>> {
    let pairs = layout_pairs("1,2,3,4", "A", 2, &["3", "5"]);
    let dataset = read_layout("FCS3.1", &pairs, b"  7  123 12    9".to_vec())?;

    assert_uint_column(dataset.data.column(0), &[7, 12]);
    assert_uint_column(dataset.data.column(1), &[123, 9]);
    Ok(())
}

#[test]
fn reads_big_endian_fcs3_1_data() -> Result<(), Box<dyn Error>> {
    let pairs = layout_pairs("4,3,2,1", "I", 2, &["16"]);
    let dataset = read_layout(
        "FCS3.1",
        &pairs,
        encode_u16_rows(&[[0x0102], [0xa0b0]], false),
    )?;
    assert_uint_column(dataset.data.column(0), &[0x0102, 0xa0b0]);

    let pairs = layout_pairs("4,3,2,1", "F", 2, &["32", "32"]);
    let data = [1.5_f32, -2.0, 0.25, 1e6]
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect();
    let dataset = read_layout("FCS3.1", &pairs, data)?;
    assert_f32_column(dataset.data.column(0), &[1.5, 0.25]);
    assert_f32_column(dataset.data.column(1), &[-2.0, 1e6]);
    Ok(())
}

#[test]
fn reads_fcs3_0_permuted_byte_order() -> Result<(), Box<dyn Error>> {
    // `$BYTEORD=3,4,1,2`: the least significant byte is stored third.
    let pairs = layout_pairs("3,4,1,2", "I", 1, &["32"]);
    let dataset = read_layout("FCS3.0", &pairs, vec![0x0b, 0x0a, 0x0d, 0x0c])?;

    assert_uint_column(dataset.data.column(0), &[0x0a0b_0c0d]);
    Ok(())
}

#[test]
fn reads_rows_across_data_chunk_boundaries() -> Result<(), Box<dyn Error>> {
    // 3-byte rows: 1.2 MB of DATA, not a multiple of the 1 MiB read chunk.
    const ROWS: usize = 400_000;
    let first = (0..ROWS).map(|row| (row % 251) as u64).collect::<Vec<_>>();
    let second = (0..ROWS)
        .map(|row| (row * 7 % 65_521) as u64)
        .collect::<Vec<_>>();
    let mut data = Vec::with_capacity(ROWS * 3);
    for (low, wide) in first.iter().zip(&second) {
        data.push(u8::try_from(*low)?);
        data.extend(u16::try_from(*wide)?.to_le_bytes());
    }

    let pairs = layout_pairs("1,2,3,4", "I", ROWS, &["8", "16"]);
    let dataset = read_layout("FCS3.1", &pairs, data)?;

    assert_uint_column(dataset.data.column(0), &first);
    assert_uint_column(dataset.data.column(1), &second);
    Ok(())
}

#[test]
fn rejects_ascii_value_in_a_later_chunk() {
    // 20-byte rows: the bad value sits past the first 1 MiB of DATA.
    const ROWS: usize = 60_000;
    let mut data = (0..ROWS)
        .flat_map(|row| format!("{row:>20}").into_bytes())
        .collect::<Vec<_>>();
    let last = data.len() - 20;
    data[last..].copy_from_slice(format!("{:>20}", "x1").as_bytes());

    let pairs = layout_pairs("1,2,3,4", "A", ROWS, &["20"]);
    let error = read_layout("FCS3.1", &pairs, data).expect_err("`x1` is not an integer");

    assert!(
        matches!(&error, cytiq_fcs::Error::EventValueParse { parameter: 1, value, .. } if value.trim() == "x1"),
        "{error}"
    );
}

#[test]
fn parses_escaped_text_delimiter() -> Result<(), Box<dyn Error>> {
    let bytes = build_fcs(
        "FCS3.0",
        vec![
            ("$BYTEORD", "1,2"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "CD4"),
            ("$P1S", "CD4|CD8"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[9]], true),
        None,
    );

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(dataset.parameters[0].label.as_deref(), Some("CD4|CD8"));

    Ok(())
}

#[test]
fn parses_whitespace_padded_layout_keywords() -> Result<(), Box<dyn Error>> {
    let bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4   "),
            ("$DATATYPE", "I   "),
            ("$MODE", "L   "),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1   "),
            ("$TOT", "1               "),
            ("$P1B", "16   "),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[42]], true),
        None,
    );

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_uint_column(dataset.data.column(0), &[42]);

    Ok(())
}

#[test]
fn rejects_duplicate_keywords() {
    let bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$TOT", "2"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[1]], true),
        None,
    );

    let error = FcsReader::new(std::io::Cursor::new(bytes))
        .read()
        .expect_err("duplicate keyword should fail");
    assert!(error.to_string().contains("duplicate keyword"));
}

#[test]
fn rejects_header_text_offset_mismatch() {
    let mut bytes = build_fcs(
        "FCS3.1",
        vec![
            ("$BYTEORD", "1,2,3,4"),
            ("$DATATYPE", "I"),
            ("$MODE", "L"),
            ("$NEXTDATA", "00000000"),
            ("$PAR", "1"),
            ("$TOT", "1"),
            ("$P1B", "16"),
            ("$P1E", "0,0"),
            ("$P1N", "Time"),
            ("$P1R", "1024"),
        ],
        None,
        encode_u16_rows(&[[99]], true),
        None,
    );
    overwrite_header_range(&mut bytes, 26, 34, 1, 2);

    let error = FcsReader::new(std::io::Cursor::new(bytes))
        .read()
        .expect_err("mismatched DATA offsets should fail");
    assert!(error.to_string().contains("offsets do not match"));
}

#[test]
fn writes_and_reads_fcs3_1_float32() -> Result<(), Box<dyn Error>> {
    let parameters = vec![
        FcsWriteParameter::new("FSC-A"),
        FcsWriteParameter::with_label("FITC", "FITC-A"),
    ];
    let columns = vec![vec![1.0_f32, 2.0, 3.0], vec![10.5_f32, 20.5, 30.5]];
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer(&mut bytes, &parameters, &columns)?;

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(dataset.version, FcsVersion::V3_1);
    assert_eq!(dataset.parameters.len(), 2);
    assert_eq!(dataset.parameters[0].name, "FSC-A");
    assert_eq!(dataset.parameters[1].label.as_deref(), Some("FITC-A"));
    assert_f32_column(dataset.data.column(0), &[1.0, 2.0, 3.0]);
    assert_f32_column(dataset.data.column(1), &[10.5, 20.5, 30.5]);

    Ok(())
}

#[test]
fn writes_and_reads_custom_text_keywords() -> Result<(), Box<dyn Error>> {
    let parameters = vec![FcsWriteParameter::new("FL1-A")];
    let columns = vec![vec![1.0_f32, 2.0, 3.0]];
    let options =
        FcsWriteOptions::new().with_text_keyword("CYTIQ_TRANSFORM", "{\"method\":\"none\"}");
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer_with_options(
        &mut bytes,
        &parameters,
        &columns,
        &options,
    )?;

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(
        dataset.text.get("CYTIQ_TRANSFORM"),
        Some("{\"method\":\"none\"}")
    );

    Ok(())
}

#[test]
fn parameter_keywords_do_not_leak_across_indexes() -> Result<(), Box<dyn Error>> {
    let parameters = (1..=12)
        .map(|index| FcsWriteParameter::new(format!("CH{index}")))
        .collect::<Vec<_>>();
    let columns = vec![vec![1.0_f32, 2.0]; parameters.len()];
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer(&mut bytes, &parameters, &columns)?;

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    let first = &dataset.parameters[0].keywords;
    assert!(first.iter().any(|(key, _)| key == "$P1N"));
    assert!(
        first
            .iter()
            .all(|(key, _)| !key.starts_with("$P10") && !key.starts_with("$P11")),
        "unexpected keywords for $P1: {first:?}"
    );

    Ok(())
}

fn fixture_path(name: &str) -> PathBuf {
    fixture_dir().join(name)
}

fn fixture_dir() -> PathBuf {
    for start in fixture_search_roots() {
        for candidate in start
            .ancestors()
            .map(|ancestor| ancestor.join("data").join("fixtures").join("fcs"))
        {
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    panic!("fixture directory should exist: data/fixtures/fcs");
}

fn fixture_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(current_dir) = std::env::current_dir() {
        roots.push(current_dir);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        roots.push(current_exe);
    }

    roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    roots
}

fn assert_uint_column(column: Option<&ColumnData>, expected: &[u64]) {
    match column {
        Some(ColumnData::UInt(values)) => assert_eq!(values, expected),
        other => panic!("unexpected uint column: {other:?}"),
    }
}

fn assert_f32_column(column: Option<&ColumnData>, expected: &[f32]) {
    match column {
        Some(ColumnData::F32(values)) => assert_eq!(values, expected),
        other => panic!("unexpected f32 column: {other:?}"),
    }
}

fn assert_f64_column(column: Option<&ColumnData>, expected: &[f64]) {
    match column {
        Some(ColumnData::F64(values)) => assert_eq!(values, expected),
        other => panic!("unexpected f64 column: {other:?}"),
    }
}

#[test]
fn round_trips_f64_dataset() -> Result<(), Box<dyn Error>> {
    let parameters = vec![FcsWriteParameter::new("Original_ID")];
    // 2^24 + 1 is the first integer f32 cannot represent.
    let ids = vec![16_777_217.0_f64, 16_777_218.0, 16_777_219.0];
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f64_to_writer_with_options(
        &mut bytes,
        &parameters,
        std::slice::from_ref(&ids),
        &FcsWriteOptions::new(),
    )?;

    let dataset = FcsReader::new(std::io::Cursor::new(bytes)).read()?;
    assert_eq!(dataset.text.get("$DATATYPE"), Some("D"));
    assert_eq!(dataset.text.get("$P1B"), Some("64"));
    assert_f64_column(dataset.data.column(0), &ids);

    Ok(())
}

#[test]
fn carries_source_keywords_into_derived_file() -> Result<(), Box<dyn Error>> {
    let source_parameters = vec![
        FcsWriteParameter::new("FSC-A")
            .with_range(262_144.0)
            .with_keyword("V", "350"),
        FcsWriteParameter::with_label("FL1-A", "CD3").with_keyword("G", "1.5"),
    ];
    let options = FcsWriteOptions::new()
        .with_text_keyword("$CYT", "Aurora")
        .with_text_keyword("$PROJ", "study")
        .with_text_keyword("$SPILLOVER", "1,FL1-A,1");
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer_with_options(
        &mut bytes,
        &source_parameters,
        &[vec![1.0_f32, 2.0], vec![3.0_f32, 4.0]],
        &options,
    )?;
    let source = FcsReader::new(std::io::Cursor::new(bytes)).read()?;

    let parameters = source
        .parameters
        .iter()
        .map(FcsWriteParameter::from_source)
        .collect::<Vec<_>>();
    let options =
        FcsWriteOptions::new().with_text_keywords(cytiq_fcs::carried_keywords(&source.text));
    let mut bytes = Vec::new();
    cytiq_fcs::write_fcs3_1_f32_to_writer_with_options(
        &mut bytes,
        &parameters,
        &[vec![1.0_f32], vec![3.0_f32]],
        &options,
    )?;
    let derived = FcsReader::new(std::io::Cursor::new(bytes)).read()?;

    assert_eq!(derived.text.get("$CYT"), Some("Aurora"));
    assert_eq!(derived.text.get("$PROJ"), Some("study"));
    assert_eq!(derived.text.get("$SPILLOVER"), Some("1,FL1-A,1"));
    assert_eq!(derived.text.get("$TOT"), Some("1"));
    assert_eq!(derived.text.get("$P1R"), Some("262144"));
    assert_eq!(derived.text.get("$P1V"), Some("350"));
    assert_eq!(derived.text.get("$P2S"), Some("CD3"));
    assert_eq!(derived.text.get("$P2G"), Some("1.5"));

    Ok(())
}

#[test]
#[ignore = "writes a ~120 MB file; run with --ignored"]
fn writes_and_reads_dataset_over_100mb() -> Result<(), Box<dyn Error>> {
    let events = 30_000_000_usize;
    let parameters = vec![FcsWriteParameter::new("FL1-A")];
    #[allow(clippy::cast_precision_loss)]
    let column = (0..events)
        .map(|index| (index % 1024) as f32)
        .collect::<Vec<_>>();
    let directory = std::env::temp_dir().join(format!("cytiq-large-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("large.fcs");
    cytiq_fcs::write_fcs3_1_f32(&path, &parameters, &[column])?;

    let dataset = FcsReader::open(&path)?.read()?;
    std::fs::remove_dir_all(&directory)?;
    assert_eq!(dataset.data.column(0).map(ColumnData::len), Some(events));
    match dataset.data.column(0) {
        Some(ColumnData::F32(values)) => assert_eq!(values[events - 1], 895.0),
        other => panic!("expected F32 column, got {other:?}"),
    }

    Ok(())
}

type Pairs<'a> = Vec<(&'a str, &'a str)>;

/// A one-parameter file whose TEXT also holds `extra`, and `supplemental` in a
/// supplemental TEXT segment.
fn read_with_keywords(
    extra: &[(&str, &str)],
    supplemental: Option<Pairs<'_>>,
) -> Result<cytiq_fcs::FcsDataset, FcsError> {
    let mut pairs = vec![
        ("$BYTEORD", "1,2,3,4"),
        ("$DATATYPE", "I"),
        ("$MODE", "L"),
        ("$NEXTDATA", "00000000"),
        ("$PAR", "1"),
        ("$TOT", "1"),
        ("$P1B", "16"),
        ("$P1E", "0,0"),
        ("$P1N", "FL1-A"),
        ("$P1R", "1024"),
    ];
    pairs.extend_from_slice(extra);
    let bytes = build_fcs(
        "FCS3.1",
        pairs,
        supplemental,
        encode_u16_rows(&[[42]], true),
        None,
    );
    FcsReader::new(std::io::Cursor::new(bytes)).read()
}

#[test]
fn a_repeated_vendor_keyword_has_no_value_and_keeps_its_pairs() {
    let cases: [(Pairs<'_>, Option<Pairs<'_>>); 4] = [
        // As the cross-platform T-cell tubes hold it, and all alike.
        (
            vec![("NA", "empty"), ("NA", "CD3-AA750-A"), ("NA", "x")],
            None,
        ),
        (vec![("NA", "empty"), ("NA", "empty")], None),
        // Keywords are case-insensitive.
        (vec![("NA", "a"), ("na", "b"), ("Na", "c")], None),
        (vec![("NA", "a")], Some(vec![("NA", "b")])),
    ];
    for (extra, supplemental) in cases {
        let count = extra.len() + supplemental.as_ref().map_or(0, Vec::len);
        let dataset = read_with_keywords(&extra, supplemental).unwrap();
        for key in ["NA", "na"] {
            assert_eq!(dataset.text.get(key), None, "neither first nor last");
        }
        let visible = |key: &str| {
            dataset
                .text
                .iter()
                .filter(|(found, _)| found.eq_ignore_ascii_case(key))
                .count()
        };
        assert_eq!(visible("NA"), 0, "no lookup finds it");
        assert_eq!(visible("$P1N"), 1);
        let kept = dataset
            .text
            .pairs()
            .filter(|(key, _)| key.eq_ignore_ascii_case("NA"))
            .count();
        assert_eq!(kept, count, "TEXT is kept as written");
        assert_eq!(dataset.text.quarantined().collect::<Vec<_>>(), ["NA"]);
        assert!(
            cytiq_fcs::carried_keywords(&dataset.text)
                .iter()
                .all(|(key, _)| !key.eq_ignore_ascii_case("NA")),
            "a derived file does not inherit it"
        );
        assert_eq!(dataset.data.column(0), Some(&ColumnData::UInt(vec![42])));
    }
    let clean = read_with_keywords(&[("NA", "empty")], None).unwrap();
    assert_eq!(clean.text.get("NA"), Some("empty"));
    assert_eq!(clean.text.quarantined().count(), 0);
}

#[test]
fn a_repeated_keyword_the_engine_reads_is_refused() {
    for key in [
        "$CYT",
        "$SPILLOVER",
        "$VENDORONLY",
        "SPILL",
        "SPILLOVER",
        "CYTIQ_ACQUISITION",
        "CYTIQ_ANYTHING",
        "flowCore_$P1Rmax",
        "transformation",
        "minRange",
        "maxRange",
        "CYT",
        "OriginalInstrument",
        "P1G",
    ] {
        // In TEXT, with equal values, by case, and through supplemental TEXT.
        let lower = key.to_ascii_lowercase();
        for (extra, supplemental) in [
            (vec![(key, "1"), (key, "2")], None),
            (vec![(key, "1"), (key, "1")], None),
            (vec![(key, "1"), (lower.as_str(), "2")], None),
            (vec![(key, "1")], Some(vec![(key, "2")])),
        ] {
            let error = read_with_keywords(&extra, supplemental).unwrap_err();
            assert!(
                matches!(&error, FcsError::DuplicateKeyword(found)
                    if found.eq_ignore_ascii_case(key)),
                "{key}: {error}"
            );
        }
    }
    // A layout keyword in supplemental TEXT repeats the primary one.
    let error = read_with_keywords(&[], Some(vec![("$TOT", "9")])).unwrap_err();
    assert!(matches!(error, FcsError::DuplicateKeyword(_)), "{error}");
}

#[test]
fn several_keywords_can_be_quarantined_and_the_stream_agrees() {
    let dataset = read_with_keywords(
        &[
            ("NA", "1"),
            ("FJ_X", "1"),
            ("NA", "2"),
            ("Plain", "kept"),
            ("fj_x", "1"),
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        dataset.text.quarantined().collect::<Vec<_>>(),
        ["FJ_X", "NA"]
    );
    assert_eq!(dataset.text.get("plain"), Some("kept"));
    assert_eq!(
        dataset.text.pairs().count(),
        dataset.text.iter().count() + 4
    );
}
