//! What the fuzz target checks. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use cytiq_mask::parquet::ParquetReadLimits;
use cytiq_qc::truth::{ArtifactTruth, EventTruth};

const LIMITS: ParquetReadLimits = ParquetReadLimits {
    file_bytes: 4 << 20,
    metadata_bytes: 1 << 20,
    page_bytes: 8 << 20,
    decoded_bytes: 64 << 20,
    rows: 100_000,
    pages: 10_000,
};

/// `Some` when the bytes are a valid `cytiq.qc.truth.v1` table.
#[must_use]
pub fn read(bytes: &[u8]) -> Option<ArtifactTruth> {
    ArtifactTruth::read_from(bytes::Bytes::copy_from_slice(bytes), LIMITS).ok()
}

/// Any bytes as artifact truth: reading never panics. An accepted table is
/// valid, its event states follow its classes, and the writer's encoding of
/// it reads back equal.
pub fn truth_parquet(bytes: &[u8]) {
    let Some(truth) = read(bytes) else {
        return;
    };
    truth.validate().expect("accepted truth is valid");
    let states = truth.event_states();
    assert_eq!(states.len(), truth.events());
    for (row, state) in states.iter().enumerate() {
        let mut values = truth.classes.iter().map(|(_, values)| values[row]);
        let expected = if values.clone().any(|value| value == Some(true)) {
            EventTruth::Artifact
        } else if values.all(|value| value == Some(false)) {
            EventTruth::Clean
        } else {
            EventTruth::Undetermined
        };
        assert_eq!(*state, expected, "event {row}");
    }
    let mut encoded = Vec::new();
    truth
        .write_to(&mut encoded)
        .expect("accepted truth encodes");
    assert_eq!(read(&encoded), Some(truth), "accepted truth round-trips");
}
