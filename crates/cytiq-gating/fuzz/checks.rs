//! What the fuzz target checks. `tests/fuzz_regressions.rs` includes this file
//! too, so `cargo test` replays every saved input against the same checks.

use cytiq_gating::{parse_gatingml, to_gatingml};

/// Any bytes as a Gating-ML document: parsing returns `Ok` or `Err`, never
/// panics. An accepted strategy keeps every gate after its parent, writes
/// back without error, reads back as the same strategy, and writes the same
/// bytes again (the writer is deterministic and lossless).
pub fn gatingml(bytes: &[u8]) {
    let Ok(xml) = std::str::from_utf8(bytes) else {
        return;
    };
    let Ok(strategy) = parse_gatingml(xml) else {
        return;
    };
    let gates = strategy.gates();
    for (index, gate) in gates.iter().enumerate() {
        if let Some(parent) = &gate.parent_id {
            let position = gates
                .iter()
                .position(|candidate| &candidate.id == parent)
                .expect("an accepted parent exists");
            assert!(position < index, "parent `{parent}` after `{}`", gate.id);
        }
    }
    let written = to_gatingml(&strategy).expect("an accepted strategy writes back");
    let reread = parse_gatingml(&written).expect("written Gating-ML parses");
    assert_eq!(reread, strategy, "write/read changes the strategy");
    let again = to_gatingml(&reread).expect("a re-read strategy writes back");
    assert_eq!(again, written, "writing is not deterministic");
}

/// Whether the parser accepts the bytes.
pub fn gatingml_accepts(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|xml| parse_gatingml(xml).is_ok())
}
