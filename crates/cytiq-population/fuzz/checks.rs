//! What the fuzz targets check. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use cytiq_mask::parquet::ScalarParquetReader;
use cytiq_population::dataset;
use cytiq_population::events;
use cytiq_population::issue::Issues;
use cytiq_population::labels;
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

/// Any bytes as a `cytiq.events.v1` table: verification never panics, a
/// rejection always names an error, and an accepted table is internally
/// consistent and verifies the same way twice.
pub fn events_parquet(bytes: &[u8]) {
    let mut issues = Issues::new();
    let summary = events::verify_with_limits(
        bytes::Bytes::copy_from_slice(bytes),
        LIMITS,
        "",
        &mut issues,
    );
    let Some(summary) = summary else {
        assert!(issues.has_errors(), "a rejected table names an error");
        return;
    };
    assert!(!issues.has_errors());
    assert_eq!(summary.layer_id, summary.descriptor.layer_id());
    assert_eq!(summary.values_sha256.len(), 64);
    if let Some(raw_events) = summary.raw_events {
        assert_eq!(raw_events, summary.rows);
    }
    assert_eq!(summary.rows == 0, summary.max_source_event_id.is_none());
    assert_eq!(summary.rows == 0, summary.min_source_event_id.is_none());
    assert!(summary.min_source_event_id <= summary.max_source_event_id);
    assert_ne!(summary.channels.len(), 0, "a table stores a channel");
    assert_eq!(
        summary.all_columns,
        summary.channels.len() == summary.descriptor.channels.len()
    );
    let mut again = Issues::new();
    let repeat =
        events::verify_with_limits(bytes::Bytes::copy_from_slice(bytes), LIMITS, "", &mut again);
    assert_eq!(
        repeat.as_ref(),
        Some(&summary),
        "verification is deterministic"
    );
}

/// Whether [`events_parquet`] accepts `bytes`.
pub fn events_parquet_accepts(bytes: &[u8]) -> bool {
    events::verify_with_limits(
        bytes::Bytes::copy_from_slice(bytes),
        LIMITS,
        "",
        &mut Issues::new(),
    )
    .is_some()
}

/// Any bytes as a dataset manifest: parsing and document validation never
/// panic, and an accepted manifest has a stable identity and round-trips
/// through its canonical JSON.
pub fn dataset_json(bytes: &[u8]) {
    let mut issues = Issues::new();
    let Some(manifest) = dataset::parse(bytes, DocumentFormat::Json, &mut issues) else {
        assert!(issues.has_errors(), "a rejected manifest names an error");
        return;
    };
    for profile in [
        dataset::Profile::Structural,
        dataset::Profile::MiflowcytMinimal,
    ] {
        dataset::validate_document(&manifest, profile, &mut Issues::new());
    }
    let canonical = manifest.canonical_json();
    let again = dataset::parse(
        canonical.as_bytes(),
        DocumentFormat::Json,
        &mut Issues::new(),
    )
    .expect("canonical JSON of an accepted manifest parses");
    assert_eq!(again, manifest);
    assert_eq!(again.dataset_id(), manifest.dataset_id());
}

/// Whether [`dataset_json`] parses `bytes` and finds no document error.
pub fn dataset_json_accepts(bytes: &[u8]) -> bool {
    let mut issues = Issues::new();
    dataset::parse(bytes, DocumentFormat::Json, &mut issues).is_some_and(|manifest| {
        dataset::validate_document(&manifest, dataset::Profile::Structural, &mut issues);
        !issues.has_errors()
    })
}

/// Any bytes as a `cytiq.events.labels.v1` table: verification never panics,
/// a rejection always names an error, and an accepted table is internally
/// consistent, counts every row of every counted column once, and verifies
/// the same way twice.
pub fn labels_parquet(bytes: &[u8]) {
    let mut issues = Issues::new();
    let summary = labels::verify_with_limits(
        bytes::Bytes::copy_from_slice(bytes),
        LIMITS,
        "",
        &mut issues,
    );
    let Some(summary) = summary else {
        assert!(issues.has_errors(), "a rejected table names an error");
        return;
    };
    assert!(!issues.has_errors());
    let descriptor = &summary.descriptor;
    assert_eq!(summary.annotation_id, descriptor.annotation_id());
    assert_eq!(summary.definition_id, descriptor.definition_id());
    assert_eq!(summary.rows, descriptor.raw_events);
    assert_eq!(summary.states.values().sum::<u64>(), summary.rows);
    assert!(
        summary
            .states
            .keys()
            .all(|state| descriptor.kind.states().contains(state))
    );
    let mut per_column = std::collections::BTreeMap::<&str, u64>::new();
    for count in &summary.counts {
        *per_column.entry(count.column.as_str()).or_default() += count.count;
    }
    assert!(per_column.values().all(|rows| *rows == summary.rows));
    let mut again = Issues::new();
    let repeat =
        labels::verify_with_limits(bytes::Bytes::copy_from_slice(bytes), LIMITS, "", &mut again);
    assert_eq!(
        repeat.as_ref(),
        Some(&summary),
        "verification is deterministic"
    );
}

/// Whether [`labels_parquet`] accepts `bytes`.
pub fn labels_parquet_accepts(bytes: &[u8]) -> bool {
    labels::verify_with_limits(
        bytes::Bytes::copy_from_slice(bytes),
        LIMITS,
        "",
        &mut Issues::new(),
    )
    .is_some()
}

/// Any bytes as `FlowSOM` assignments: reading never panics, accepted records
/// number nodes and metaclusters from 1 in increasing input rows, and the
/// records end at the first error.
pub fn flowsom_csv(bytes: &[u8]) {
    let mut previous: Option<u64> = None;
    let mut failed = false;
    for record in labels::read_flowsom_assignments(bytes) {
        assert!(!failed, "no record follows an error");
        match record {
            Ok(assignment) => {
                assert!(assignment.node > 0 && assignment.metacluster > 0);
                assert!(previous < Some(assignment.event_index));
                previous = Some(assignment.event_index);
            }
            Err(_) => failed = true,
        }
    }
}

/// Whether [`flowsom_csv`] reads every record of `bytes`.
pub fn flowsom_csv_accepts(bytes: &[u8]) -> bool {
    labels::read_flowsom_assignments(bytes).all(|record| record.is_ok())
}
