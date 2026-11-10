//! What the fuzz targets check. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

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

/// Any bytes as an observation table: reading never panics, and every row it
/// accepts is a valid observation with an intact identity.
pub fn observations_parquet(bytes: &[u8]) {
    let limits = table::ParquetReadLimits {
        file_bytes: 4 << 20,
        metadata_bytes: 1 << 20,
        page_bytes: 8 << 20,
        decoded_bytes: 64 << 20,
        rows: 100_000,
        pages: 10_000,
    };
    let Ok(rows) = table::read_from_with_limits(bytes::Bytes::copy_from_slice(bytes), limits)
    else {
        return;
    };
    for row in &rows {
        row.validate()
            .expect("accepted rows are valid observations");
    }
}
