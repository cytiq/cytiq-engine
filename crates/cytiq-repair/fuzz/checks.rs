//! What the fuzz target checks. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use std::io::{Cursor, Read as _};

use cytiq_fcs::{FcsReadLimits, FcsReader};
use cytiq_repair::{Diagnosis, diagnose};

const ACQUISITION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Any bytes: a diagnosis, never a panic. A conforming file is one the strict
/// reader accepts. What a plan writes is read strictly, needs no repair, holds
/// the window the plan names byte for byte and exactly `$TOT` events, and
/// records the acquisition; nothing is dropped without a code it needs.
pub fn repair_plan(bytes: &[u8]) {
    let diagnosis = diagnose(&mut Cursor::new(bytes)).expect("memory is readable");
    let (plan, reader) = match diagnosis {
        Diagnosis::Conforming => return,
        Diagnosis::Refused { refusal, reader } => {
            assert!(!reader.is_empty() && !refusal.message.is_empty());
            return;
        }
        Diagnosis::Repairable { plan, reader } => (plan, reader),
    };
    assert!(!reader.is_empty(), "the reader refuses what is repaired");
    let codes = plan.codes();
    assert!(!codes.is_empty());
    assert!(
        plan.dropped
            .iter()
            .all(|entry| entry.code.is_none_or(|code| codes.contains(&code)))
    );
    assert!(
        plan.dropped
            .iter()
            .all(|entry| entry.code.is_some() || entry.blank)
    );

    let mut repaired = Vec::new();
    plan.repaired(Cursor::new(bytes), ACQUISITION)
        .read_to_end(&mut repaired)
        .expect("the source is whole");
    let again = diagnose(&mut Cursor::new(&repaired)).expect("memory is readable");
    assert_eq!(again, Diagnosis::Conforming);

    let stream = FcsReader::new(Cursor::new(&repaired))
        .stream(std::num::NonZeroUsize::MIN, FcsReadLimits::default())
        .expect("the reader accepts it");
    let text = &stream.metadata.text;
    assert_eq!(text.get("CYTIQ_ACQUISITION"), Some(ACQUISITION));
    assert_eq!(stream.metadata.event_count as u64, plan.data.events);
    assert_eq!(
        stream.metadata.parameters.len() as u64,
        plan.data.parameters
    );
    let at = usize::try_from(plan.data.offset).expect("in memory");
    let length = usize::try_from(plan.data.bytes).expect("in memory");
    let offset = |key| -> usize {
        text.get(key)
            .and_then(|value| value.trim().parse().ok())
            .expect("a DATA offset")
    };
    assert_eq!(
        &repaired[offset("$BEGINDATA")..=offset("$ENDDATA")],
        &bytes[at..at + length],
        "DATA is copied as it is"
    );
}
