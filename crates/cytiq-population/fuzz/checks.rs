//! What the fuzz targets check. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use cytiq_mask::parquet::ScalarParquetReader;
use cytiq_population::serialization::{self, DocumentFormat};
use cytiq_population::table;

/// Any bytes: parsing returns `Ok` or `Err`, never panics, aborts or hangs.
/// An accepted document is a mapping whose canonical digest is stable, and
/// parsing is deterministic.
pub fn document(bytes: &[u8], format: DocumentFormat) {
    let Ok(value) = serialization::parse(bytes, format) else {
        return;
    };
    assert!(value.is_object());
    let digest = serialization::digest(&value);
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let again = serialization::parse(bytes, format).expect("accepted twice");
    assert_eq!(serialization::digest(&again), digest);
}

pub fn document_json(bytes: &[u8]) {
    document(bytes, DocumentFormat::Json);
}

pub fn document_yaml(bytes: &[u8]) {
    document(bytes, DocumentFormat::Yaml);
}

const LIMITS: table::ParquetReadLimits = table::ParquetReadLimits {
    file_bytes: 4 << 20,
    metadata_bytes: 1 << 20,
    page_bytes: 8 << 20,
    decoded_bytes: 64 << 20,
    rows: 100_000,
    pages: 10_000,
};

/// Any bytes as an observation table: reading never panics, and every row it
/// accepts is a valid observation with an intact identity.
pub fn observations_parquet(bytes: &[u8]) {
    let Ok(rows) = table::read_from_with_limits(bytes::Bytes::copy_from_slice(bytes), LIMITS)
    else {
        return;
    };
    for row in &rows {
        row.validate()
            .expect("accepted rows are valid observations");
    }
}

/// Any bytes as a mask or classify annotation table, read as the annotation
/// readers do: the bounded reader with a projection (here every column in
/// reverse order) never panics, and accepted batches carry exactly the
/// projected columns and, in total, the footer's row count.
pub fn projected_parquet(bytes: &[u8]) {
    let Ok(reader) = ScalarParquetReader::new(bytes::Bytes::copy_from_slice(bytes), LIMITS) else {
        return;
    };
    let declared = reader.metadata().file_metadata().num_rows();
    let schema = std::sync::Arc::clone(reader.schema());
    let names = schema
        .fields()
        .iter()
        .rev()
        .map(|field| field.name().as_str())
        .collect::<Vec<_>>();
    let Ok(reader) = reader.project(&names) else {
        return;
    };
    let mut rows = 0_i64;
    for batch in reader {
        let Ok(batch) = batch else {
            return;
        };
        let batch_schema = batch.schema();
        let emitted = batch_schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(emitted, names, "batches carry the projected columns");
        rows += i64::try_from(batch.num_rows()).expect("batch rows fit in i64");
    }
    assert_eq!(rows, declared, "accepted tables yield the declared rows");
}

/// Whether `projected_parquet` accepts the bytes as a whole table.
pub fn projected_parquet_accepts(bytes: &[u8]) -> bool {
    let Ok(reader) = ScalarParquetReader::new(bytes::Bytes::copy_from_slice(bytes), LIMITS) else {
        return false;
    };
    let schema = std::sync::Arc::clone(reader.schema());
    let names = schema
        .fields()
        .iter()
        .rev()
        .map(|field| field.name().as_str())
        .collect::<Vec<_>>();
    reader
        .project(&names)
        .is_ok_and(|reader| reader.into_iter().all(|batch| batch.is_ok()))
}
