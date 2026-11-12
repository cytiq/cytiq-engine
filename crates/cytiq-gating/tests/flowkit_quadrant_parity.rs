//! `QuadrantGate` support against `FlowKit` 1.3.0 (`data/fixtures/gating/quadrant`)
//! and lossless Gating-ML round trips of both fixture strategies.

use std::fs;
use std::path::PathBuf;

use cytiq_fcs::FcsReader;
use cytiq_gating::{GateKind, GatingError, parse_gatingml, to_gatingml};
use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/gating")
        .join(path)
}

fn decode_membership(hex: &str, event_count: usize) -> Vec<bool> {
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("valid hex"))
        .collect::<Vec<_>>();
    (0..event_count)
        .map(|event| bytes[event / 8] & (0x80 >> (event % 8)) != 0)
        .collect()
}

#[test]
fn quadrants_match_flowkit_membership_counts_and_paths() {
    let xml = fs::read_to_string(fixture("quadrant/gates.xml")).expect("gates.xml");
    let reference: Value = serde_json::from_str(
        &fs::read_to_string(fixture("quadrant/flowkit_reference.json")).expect("reference"),
    )
    .expect("JSON");
    let dataset = FcsReader::open(fixture("quadrant/sample.fcs"))
        .and_then(FcsReader::read)
        .expect("sample.fcs");
    let strategy = parse_gatingml(&xml).expect("quadrant strategy parses");
    let results = strategy.gate_sample(&dataset).expect("gating succeeds");

    let event_count = usize::try_from(reference["event_count"].as_u64().unwrap()).unwrap();
    let expected = reference["gates"].as_object().unwrap();
    assert_eq!(results.gate_ids().count(), expected.len());
    let report = results.report();
    for (gate_id, expected) in expected {
        let membership = results
            .membership(gate_id)
            .unwrap_or_else(|| panic!("no gate `{gate_id}`"));
        let reference =
            decode_membership(expected["membership_hex"].as_str().unwrap(), event_count);
        let mismatches = membership
            .iter()
            .zip(&reference)
            .enumerate()
            .filter(|(_, (actual, expected))| actual != expected)
            .map(|(event, _)| event)
            .collect::<Vec<_>>();
        assert!(
            mismatches.is_empty(),
            "`{gate_id}` differs at {mismatches:?}"
        );
        assert_eq!(
            results.count(gate_id),
            Some(usize::try_from(expected["count"].as_u64().unwrap()).unwrap())
        );
        let row = report.iter().find(|row| row.gate_id == *gate_id).unwrap();
        assert_eq!(row.gate_path, expected["gate_path"].as_str().unwrap());
    }
    // Quadrants of one QuadrantGate partition its parent.
    let quadrants = ["FSChi_SSChi", "FSChi_SSClo", "FSClo_SSChi", "FSClo_SSClo"];
    let total = quadrants
        .iter()
        .map(|id| results.count(id).unwrap())
        .sum::<usize>();
    assert_eq!(Some(total), results.count("Cells"));
    assert!(matches!(
        strategy.gate("CD3_mid").unwrap().kind,
        GateKind::Quadrant { .. }
    ));
}

#[test]
fn strategies_round_trip_through_gating_ml_without_loss() {
    // `gates.xml` uses every transformation, logicle and hyperlog included.
    for path in ["gates.xml", "quadrant/gates.xml"] {
        let xml = fs::read_to_string(fixture(path)).expect("fixture");
        let strategy = parse_gatingml(&xml).expect("parses");
        let written = to_gatingml(&strategy).expect("writes");
        let reparsed = parse_gatingml(&written).expect("written document parses");
        assert_eq!(reparsed, strategy, "{path}");
        assert_eq!(to_gatingml(&reparsed).expect("writes"), written, "{path}");
    }
}

/// `cytiq_written.xml` is what `FlowKit` reads back in `parity/compare.py`
/// (the same membership as `flowkit_reference.json`). Pinning its bytes means
/// any writer change is checked against `FlowKit` again before it lands.
#[test]
fn written_quadrant_strategy_is_the_one_flowkit_reads_back() {
    let xml = fs::read_to_string(fixture("quadrant/gates.xml")).expect("gates.xml");
    let written = to_gatingml(&parse_gatingml(&xml).expect("parses")).expect("writes");
    let pinned = fixture("quadrant/cytiq_written.xml");
    let expected = fs::read_to_string(&pinned).unwrap_or_default();
    if written != expected {
        let actual = std::env::temp_dir().join("cytiq_written.actual.xml");
        fs::write(&actual, &written).expect("actual is written");
        panic!(
            "the writer output changed; check `{}` with FlowKit (`just parity`) before replacing `{}`",
            actual.display(),
            pinned.display()
        );
    }
}

fn quadrant_document(gate: &str, extra: &str) -> String {
    format!(
        r#"<gating:Gating-ML xmlns:gating="http://www.isac-net.org/std/Gating-ML/v2.0/gating"
                     xmlns:data-type="http://www.isac-net.org/std/Gating-ML/v2.0/datatypes">
  <gating:QuadrantGate gating:id="Quad">{gate}</gating:QuadrantGate>
  {extra}
</gating:Gating-ML>"#
    )
}

const DIVIDER: &str = r#"<gating:divider gating:id="D" gating:compensation-ref="uncompensated">
    <data-type:fcs-dimension data-type:name="FSC-A"/><gating:value>10</gating:value></gating:divider>"#;

#[test]
fn malformed_quadrant_gates_are_rejected() {
    let quadrant = r#"<gating:Quadrant gating:id="Q"><gating:position gating:divider_ref="D" gating:location="20"/></gating:Quadrant>"#;
    let cases = [
        (quadrant_document(quadrant, ""), "divider"),
        (quadrant_document(DIVIDER, ""), "quadrant"),
        (
            quadrant_document(&format!(r#"{DIVIDER}<gating:Quadrant gating:id="Q"/>"#), ""),
            "no position",
        ),
        (
            quadrant_document(
                &format!(
                    r#"{DIVIDER}<gating:Quadrant gating:id="Q"><gating:position gating:divider_ref="X" gating:location="1"/></gating:Quadrant>"#
                ),
                "",
            ),
            "unknown divider",
        ),
        (
            quadrant_document(
                &format!(
                    "{DIVIDER}{}{quadrant}",
                    DIVIDER.replace(r#"gating:id="D""#, r#"gating:id="E""#)
                ),
                "",
            ),
            "two dividers",
        ),
    ];
    for (xml, reason) in cases {
        match parse_gatingml(&xml) {
            Err(GatingError::InvalidGate {
                id,
                reason: message,
            }) => {
                assert_eq!(id, "Quad");
                assert!(message.contains(reason), "{message} (expected `{reason}`)");
            }
            other => panic!("expected an invalid QuadrantGate ({reason}), got {other:?}"),
        }
    }
    let nan = quadrant_document(
        &format!(
            r#"{DIVIDER}<gating:Quadrant gating:id="Q"><gating:position gating:divider_ref="D" gating:location="NaN"/></gating:Quadrant>"#
        ),
        "",
    );
    assert!(matches!(
        parse_gatingml(&nan),
        Err(GatingError::InvalidGatingMl(_))
    ));
}

#[test]
fn quadrant_gate_ids_name_no_population() {
    let quadrants = format!(
        "{DIVIDER}{}",
        r#"<gating:Quadrant gating:id="Q"><gating:position gating:divider_ref="D" gating:location="20"/></gating:Quadrant>"#
    );
    let referenced = quadrant_document(
        &quadrants,
        r#"<gating:RectangleGate gating:id="Child" gating:parent_id="Quad">
             <gating:dimension gating:min="1" gating:compensation-ref="uncompensated">
               <data-type:fcs-dimension data-type:name="SSC-A"/></gating:dimension>
           </gating:RectangleGate>"#,
    );
    match parse_gatingml(&referenced) {
        Err(GatingError::InvalidGate { id, reason }) => {
            assert_eq!(id, "Child");
            assert!(reason.contains("QuadrantGate"), "{reason}");
        }
        other => panic!("expected a rejected QuadrantGate reference, got {other:?}"),
    }
    let clash = quadrant_document(
        &quadrants,
        r#"<gating:RectangleGate gating:id="Quad">
             <gating:dimension gating:min="1" gating:compensation-ref="uncompensated">
               <data-type:fcs-dimension data-type:name="SSC-A"/></gating:dimension>
           </gating:RectangleGate>"#,
    );
    assert!(matches!(
        parse_gatingml(&clash),
        Err(GatingError::DuplicateGate { id }) if id == "Quad"
    ));
}
