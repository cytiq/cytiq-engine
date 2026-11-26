//! What the fuzz targets check. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use std::io::Cursor;
use std::num::NonZeroUsize;

use cytiq_fcs::{ColumnData, Error, FcsDataset, FcsReadLimits, FcsReader, FcsStream, Spillover};

/// Limits small enough that fuzzed files reach them.
const TIGHT_LIMITS: FcsReadLimits = FcsReadLimits {
    decoded_bytes_max: 4096,
    analysis_bytes_max: 64,
};

/// Any bytes: the reader returns `Ok` or `Err`, never panics, aborts or hangs,
/// and what it accepts is consistent. Read limits only ever refuse a file:
/// a read within tight limits returns what an unlimited read returns, or
/// `LimitExceeded`. Streaming decodes the same events, or fails the same way.
pub fn read_fcs(bytes: &[u8]) {
    let read = FcsReader::new(Cursor::new(bytes)).read();
    if let Ok(dataset) = &read {
        check_keywords(dataset);
        let events = dataset.event_count();
        assert_eq!(dataset.data.columns.len(), dataset.parameters.len());
        assert!(
            dataset
                .data
                .columns
                .iter()
                .all(|column| column.len() == events)
        );
        assert!(events <= bytes.len());
    }
    match (
        FcsReader::new(Cursor::new(bytes)).read_with_limits(TIGHT_LIMITS),
        &read,
    ) {
        (Ok(limited), Ok(dataset)) => assert_same(&limited, dataset),
        (Ok(_), Err(error)) => panic!("only the limited read accepted the file: {error}"),
        (Err(Error::LimitExceeded { .. }), _) => {}
        (Err(limited), Ok(_)) => panic!("only the unlimited read accepted the file: {limited}"),
        (Err(limited), Err(error)) => assert_eq!(limited.to_string(), error.to_string()),
    }
    stream_matches_read(bytes, &read);
}

/// A keyword either has one value or is quarantined, and nothing the engine
/// reads is ever quarantined.
fn check_keywords(dataset: &FcsDataset) {
    let text = &dataset.text;
    for key in text.quarantined() {
        assert!(!cytiq_fcs::is_engine_keyword(key), "{key}");
        assert_eq!(text.get(key), None, "{key}");
        assert!(text.iter().all(|(name, _)| !name.eq_ignore_ascii_case(key)));
        let written = |(name, _): &(&str, &str)| name.eq_ignore_ascii_case(key);
        assert!(text.pairs().filter(written).count() > 1, "{key}");
    }
    let mut seen = std::collections::HashSet::new();
    for (key, value) in text.iter() {
        assert!(seen.insert(key.to_ascii_uppercase()), "{key} repeats");
        assert_eq!(text.get(key), Some(value), "{key}");
    }
    let hidden = text.pairs().count() - text.iter().count();
    assert!(hidden >= 2 * text.quarantined().count());
}

fn stream_matches_read(bytes: &[u8], read: &Result<FcsDataset, Error>) {
    let size = NonZeroUsize::new(1 + bytes.len() % 7).expect("positive");
    let streamed = FcsReader::new(Cursor::new(bytes))
        .stream(size, FcsReadLimits::default())
        .and_then(|FcsStream { metadata, batches }| {
            let mut columns = batches
                .empty_batch()
                .columns
                .iter()
                .map(column_bits)
                .collect::<Vec<_>>();
            for batch in batches {
                for (whole, part) in columns.iter_mut().zip(&batch?.columns) {
                    whole.1.extend(column_bits(part).1);
                }
            }
            Ok((metadata, columns))
        });
    match (streamed, read) {
        (Ok((metadata, columns)), Ok(dataset)) => {
            assert_eq!(metadata.text, dataset.text);
            assert_eq!(metadata.parameters, dataset.parameters);
            assert_eq!(metadata.event_count, dataset.event_count());
            let read = dataset
                .data
                .columns
                .iter()
                .map(column_bits)
                .collect::<Vec<_>>();
            assert_eq!(columns, read);
        }
        (Err(streamed), Err(error)) => assert_eq!(streamed.to_string(), error.to_string()),
        // A stream never reads ANALYSIS, so only a read can fail on it.
        (Ok(_), Err(error)) => assert!(error.to_string().contains("ANALYSIS"), "{error}"),
        (Err(streamed), Ok(_)) => panic!("only the stream failed: {streamed}"),
    }
}

/// Derived equality fails on the NaN values DATA may hold, so columns are
/// compared bit for bit.
fn assert_same(left: &FcsDataset, right: &FcsDataset) {
    assert_eq!(left.version, right.version);
    assert_eq!(left.header, right.header);
    assert_eq!(left.text, right.text);
    assert_eq!(left.parameters, right.parameters);
    assert_eq!(left.analysis, right.analysis);
    let bits = |dataset: &FcsDataset| {
        dataset
            .data
            .columns
            .iter()
            .map(column_bits)
            .collect::<Vec<_>>()
    };
    assert_eq!(bits(left), bits(right));
}

fn column_bits(column: &ColumnData) -> (&'static str, Vec<u64>) {
    match column {
        ColumnData::UInt(values) => ("u64", values.clone()),
        ColumnData::F32(values) => (
            "f32",
            values
                .iter()
                .map(|value| u64::from(value.to_bits()))
                .collect(),
        ),
        ColumnData::F64(values) => ("f64", values.iter().map(|value| value.to_bits()).collect()),
    }
}

/// Any keyword value: parsing never panics, and an accepted matrix is square
/// over its channels and finite. Values that are not UTF-8 cannot be TEXT
/// values and are skipped.
pub fn spillover_keyword(bytes: &[u8]) {
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return;
    };
    if let Ok(spillover) = Spillover::parse_keyword(raw) {
        let size = spillover.size();
        assert_eq!(spillover.channels().len(), size);
        assert_eq!(spillover.matrix().len(), size * size);
        assert!(spillover.matrix().iter().all(|value| value.is_finite()));
    }
}
