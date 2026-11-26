//! Each repair on the smallest file that needs it, and each precondition broken.

mod common;

use std::io::{Cursor, Read as _};

use common::{EVENTS, Spec, data};
use cytiq_fcs::{ColumnData, FcsReader};
use cytiq_repair::{Diagnosis, Plan, RefusalCode, RepairCode, diagnose};

const ACQUISITION: &str = "c4f1d2a0c4f1d2a0c4f1d2a0c4f1d2a0c4f1d2a0c4f1d2a0c4f1d2a0c4f1d2a0";

fn found(bytes: &[u8]) -> Diagnosis {
    diagnose(&mut Cursor::new(bytes)).unwrap()
}

fn repairable(bytes: &[u8]) -> Plan {
    match found(bytes) {
        Diagnosis::Repairable { plan, .. } => *plan,
        other => panic!("expected a plan, got {other:?}"),
    }
}

fn refused(bytes: &[u8]) -> RefusalCode {
    match found(bytes) {
        Diagnosis::Refused { refusal, .. } => refusal.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn repair(plan: &Plan, source: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    plan.repaired(Cursor::new(source), ACQUISITION)
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

/// The plan needs exactly `codes`; what it writes is read strictly, holds the
/// same DATA and keywords, and needs no repair.
fn expect(spec: &Spec, codes: &[RepairCode]) -> Plan {
    let source = spec.build();
    let plan = repairable(&source);
    let needed: Vec<RepairCode> = plan.codes().into_iter().collect();
    let mut sorted = codes.to_vec();
    sorted.sort_unstable();
    assert_eq!(needed, sorted);

    let repaired = repair(&plan, &source);
    assert_eq!(found(&repaired), Diagnosis::Conforming, "idempotent");
    let dataset = FcsReader::new(Cursor::new(&repaired)).read().unwrap();
    assert_eq!(dataset.version.as_str(), spec.version);
    assert_eq!(dataset.event_count(), EVENTS.len());
    for (index, column) in dataset.data.columns.iter().enumerate() {
        let ColumnData::F32(values) = column else {
            panic!("float columns")
        };
        let bits: Vec<u32> = values.iter().map(|value| value.to_bits()).collect();
        let expected: Vec<u32> = EVENTS.iter().map(|event| event[index].to_bits()).collect();
        assert_eq!(bits, expected);
    }
    let at = usize::try_from(plan.data.offset).unwrap();
    let length = usize::try_from(plan.data.bytes).unwrap();
    assert_eq!(&source[at..at + length], data(), "the window is DATA");
    let range = dataset.header.data.unwrap();
    let range = usize::try_from(range.start).unwrap()..=usize::try_from(range.end).unwrap();
    assert_eq!(&repaired[range], data(), "DATA is copied as it is");

    let text = &dataset.text;
    assert_eq!(text.get("CYTIQ_ACQUISITION"), Some(ACQUISITION));
    let recorded: serde_json::Value =
        serde_json::from_str(text.get("CYTIQ_REPAIR").unwrap()).unwrap();
    let names: Vec<&str> = sorted.iter().map(|code| code.as_str()).collect();
    assert_eq!(recorded, serde_json::json!({"version": 1, "codes": names}));
    // The value holds the delimiter, escaped.
    let model = format!("Model{}X", char::from(spec.delimiter));
    for (key, value) in [
        ("$CYT", model.as_str()),
        ("VENDOR", "kept"),
        ("$P2N", "FL1-A"),
    ] {
        assert_eq!(text.get(key), Some(value), "{key} is kept");
    }
    let codes = plan.codes();
    assert!(
        plan.dropped
            .iter()
            .all(|entry| entry.code.is_none_or(|code| codes.contains(&code))),
        "nothing is dropped without its code"
    );
    plan
}

fn dropped_keywords(plan: &Plan) -> Vec<&str> {
    plan.dropped
        .iter()
        .flat_map(|entry| entry.keywords.iter().map(String::as_str))
        .collect()
}

#[test]
fn a_conforming_file_needs_nothing() {
    for spec in [
        Spec::default(),
        Spec {
            tail: b"00000000".to_vec(),
            ..Spec::default()
        },
        Spec {
            analysis: Some(b"/A/b/".to_vec()),
            ..Spec::default()
        },
        // Bytes nothing declares do not stop the reader, so they stay.
        Spec {
            tail: b"vendor".to_vec(),
            ..Spec::default()
        },
    ] {
        assert_eq!(found(&spec.build()), Diagnosis::Conforming);
    }
}

#[test]
fn padding_after_text_is_removed() {
    for padding in [&b"\0"[..], b"      ", b" \0 "] {
        let plan = expect(
            &Spec {
                padding: padding.to_vec(),
                ..Spec::default()
            },
            &[RepairCode::TextTrailingPadding],
        );
        let entry = &plan.dropped[0];
        assert_eq!(
            (entry.what, entry.blank, entry.bytes),
            ("text_padding", true, padding.len() as u64)
        );
    }
    // As the ID7000 writes it: newlines delimit, and a CRC follows.
    let plan = expect(
        &Spec {
            delimiter: b'\n',
            text_start: 256,
            padding: b"\0".to_vec(),
            gap: vec![0; 39],
            tail: b"00000000".to_vec(),
            ..Spec::default()
        },
        &[RepairCode::TextTrailingPadding],
    );
    let what: Vec<&str> = plan.dropped.iter().map(|entry| entry.what).collect();
    assert_eq!(what, ["text_padding", "gap", "gap"]);
}

#[test]
fn padding_that_holds_anything_is_not_padding() {
    for padding in [&b"  x"[..], b"\x01", b" \n"] {
        let spec = Spec {
            padding: padding.to_vec(),
            ..Spec::default()
        };
        assert_eq!(refused(&spec.build()), RefusalCode::TextUnterminated);
    }
}

#[test]
fn a_byte_shared_by_text_and_data_belongs_to_data() {
    // As CyTOF 6 wrote it: padded, and the end of DATA past the file.
    expect(
        &Spec {
            overlap: true,
            padding: vec![b' '; 65],
            data_end_past: 1,
            ..Spec::default()
        },
        &[
            RepairCode::TextOverlapsData,
            RepairCode::TextTrailingPadding,
            RepairCode::DataEndExclusive,
        ],
    );
    expect(
        &Spec {
            overlap: true,
            ..Spec::default()
        },
        &[RepairCode::TextOverlapsData],
    );
}

#[test]
fn an_end_of_data_past_the_file_is_one_too_far() {
    expect(
        &Spec {
            data_end_past: 1,
            ..Spec::default()
        },
        &[RepairCode::DataEndExclusive],
    );
    // Two bytes are not one, and need no explanation that fits.
    let spec = Spec {
        data_end_past: 2,
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::DataLengthMismatch);
    // With a byte after DATA either end may be the wrong one.
    let spec = Spec {
        data_end_past: 1,
        tail: b"00000000".to_vec(),
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::AmbiguousDataOffsets);
    // DATA that `$TOT` does not describe.
    for (key, value) in [("$TOT", "4"), ("$TOT", "0"), ("$P1B", "64")] {
        let spec = Spec {
            padding: b" ".to_vec(),
            overrides: vec![(key, value.to_owned())],
            ..Spec::default()
        };
        let code = refused(&spec.build());
        assert!(
            [
                RefusalCode::DataLengthMismatch,
                RefusalCode::UnsupportedLayout
            ]
            .contains(&code),
            "{key}={value}: {code}"
        );
    }
}

#[test]
fn header_and_text_must_agree_on_data() {
    // The CyAn shape: the same length, 46 bytes apart.
    let spec = Spec {
        header: [None, Some((400, 423)), None],
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::AmbiguousDataOffsets);
    // HEADER holds zeros when DATA does not fit its digits.
    let spec = Spec {
        header: [None, Some((0, 0)), None],
        padding: b" ".to_vec(),
        ..Spec::default()
    };
    expect(&spec, &[RepairCode::TextTrailingPadding]);
}

#[test]
fn supplemental_text_inside_text_holds_nothing_new() {
    for whole in [true, false] {
        expect(
            &Spec {
                supplemental: Some(whole),
                ..Spec::default()
            },
            &[RepairCode::SupplementalTextInsideText],
        );
    }
    // Part of TEXT, or a segment of its own.
    for (start, end) in [("58", "70"), ("60", "60"), ("400", "410")] {
        let spec = Spec {
            padding: b" ".to_vec(),
            overrides: vec![("$BEGINSTEXT", start.into()), ("$ENDSTEXT", end.into())],
            ..Spec::default()
        };
        assert_eq!(refused(&spec.build()), RefusalCode::SupplementalText);
    }
}

#[test]
fn a_negative_end_of_analysis_means_none_when_text_says_so() {
    let negative = [None, None, Some((0, -1))];
    expect(
        &Spec {
            header: negative,
            ..Spec::default()
        },
        &[RepairCode::AnalysisOffsetsNegative],
    );
    let spec = Spec {
        header: negative,
        analysis: Some(b"/A/b/".to_vec()),
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::AmbiguousAnalysis);
    // Any other negative offset.
    for header in [
        [None, None, Some((5, -1))],
        [None, None, Some((-1, -1))],
        [None, Some((0, -1)), None],
        [Some((58, -1)), None, None],
    ] {
        let spec = Spec {
            header,
            ..Spec::default()
        };
        assert_eq!(refused(&spec.build()), RefusalCode::HeaderUnreadable);
    }
}

#[test]
fn analysis_is_carried_with_new_offsets() {
    let analysis = b"/COMMENT/as it was/".to_vec();
    let spec = Spec {
        padding: b"   ".to_vec(),
        gap: vec![b' '; 100],
        analysis: Some(analysis.clone()),
        ..Spec::default()
    };
    let plan = expect(&spec, &[RepairCode::TextTrailingPadding]);
    let repaired = repair(&plan, &spec.build());
    let dataset = FcsReader::new(Cursor::new(&repaired)).read().unwrap();
    assert_eq!(dataset.analysis, Some(analysis));
}

#[test]
fn pairs_that_can_be_read_in_several_ways_are_dropped_and_named() {
    // The Attune shape.
    let plan = expect(
        &Spec {
            raw_text: b"$INST//$EXP/Admin/$OP/me/$COM//$SMNO//$LOST//".to_vec(),
            padding: vec![b' '; 9],
            ..Spec::default()
        },
        &[RepairCode::TextEmptyValues, RepairCode::TextTrailingPadding],
    );
    assert_eq!(
        dropped_keywords(&plan),
        ["$INST", "$EXP", "$COM", "$SMNO", "$LOST"]
    );
    // Never a value.
    let listed = serde_json::to_string(&plan.dropped).unwrap();
    assert!(!listed.contains("Admin") && !listed.contains("me"));

    // A keyword the layout needs cannot be dropped.
    let spec = Spec {
        overrides: vec![("$TOT", "<absent>".into())],
        raw_text: b"X//$TOT/3/".to_vec(),
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::MissingLayout);
    let spec = Spec {
        raw_text: b"A///x/".to_vec(),
        padding: b" ".to_vec(),
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::AmbiguousText);
}

#[test]
fn bytes_nothing_declares_are_dropped_only_by_consent() {
    // The Opteon shape: a tail, and keywords with its offsets.
    let spec = Spec {
        padding: b" ".to_vec(),
        keywords: vec![
            ("$CYT", "Model//X"),
            ("VENDOR", "kept"),
            ("#NCBeginSaturatedPops", "600"),
            ("#NCEndSaturatedPops", "700"),
        ],
        tail: vec![0, 0, 7, 0],
        ..Spec::default()
    };
    let plan = expect(
        &spec,
        &[RepairCode::TextTrailingPadding, RepairCode::UndeclaredBytes],
    );
    assert_eq!(
        dropped_keywords(&plan),
        ["#NCBeginSaturatedPops", "#NCEndSaturatedPops"]
    );
    let repaired = repair(&plan, &spec.build());
    assert!(!repaired.windows(3).any(|bytes| bytes == b"#NC"));

    // Blank bytes hold nothing; a CRC is not a tail.
    for tail in [vec![0; 91], b"00000000".to_vec(), b"12345678  ".to_vec()] {
        let spec = Spec {
            padding: b" ".to_vec(),
            tail,
            ..Spec::default()
        };
        expect(&spec, &[RepairCode::TextTrailingPadding]);
    }
    let spec = Spec {
        padding: b" ".to_vec(),
        gap: b"  x  ".to_vec(),
        ..Spec::default()
    };
    expect(
        &spec,
        &[RepairCode::TextTrailingPadding, RepairCode::UndeclaredBytes],
    );
}

#[test]
fn versions_and_mixed_types_are_kept() {
    for version in ["FCS3.0", "FCS3.1", "FCS3.2"] {
        expect(
            &Spec {
                version,
                padding: b" ".to_vec(),
                ..Spec::default()
            },
            &[RepairCode::TextTrailingPadding],
        );
    }
    // `$PnDATATYPE` counts in 3.2 only, as for the reader.
    let spec = Spec {
        version: "FCS3.2",
        padding: b" ".to_vec(),
        overrides: vec![("$P2DATATYPE", "I".into()), ("$TOT", "3".into())],
        ..Spec::default()
    };
    let plan = repairable(&spec.build());
    let repaired = repair(&plan, &spec.build());
    let dataset = FcsReader::new(Cursor::new(&repaired)).read().unwrap();
    assert!(matches!(dataset.data.columns[1], ColumnData::UInt(_)));
}

#[test]
fn files_outside_the_scope_are_refused_by_name() {
    let padded = || Spec {
        padding: b" ".to_vec(),
        ..Spec::default()
    };
    let with = |overrides: Vec<(&'static str, String)>| Spec {
        overrides,
        ..padded()
    };
    let mut other = padded().build();
    other[..10].copy_from_slice(b"FCS2.0    ");
    assert_eq!(refused(&other), RefusalCode::UnsupportedVersion);
    other[..10].copy_from_slice(b"PK\x03\x04      ");
    assert_eq!(refused(&other), RefusalCode::NotFcs);
    assert_eq!(refused(&padded().build()[..40]), RefusalCode::NotFcs);
    assert_eq!(refused(b""), RefusalCode::NotFcs);
    assert_eq!(
        refused(&padded().build()[..100]),
        RefusalCode::TextOutOfFile
    );
    let cases = [
        (
            with(vec![("$NEXTDATA", "900".into())]),
            RefusalCode::MultipleDataSets,
        ),
        (
            with(vec![("$MODE", "H".into())]),
            RefusalCode::UnsupportedLayout,
        ),
        (
            with(vec![("$P1B", "12".into())]),
            RefusalCode::UnsupportedLayout,
        ),
        (
            with(vec![("$PAR", "0".into())]),
            RefusalCode::UnsupportedLayout,
        ),
        (
            with(vec![("$PAR", "many".into())]),
            RefusalCode::MissingLayout,
        ),
        (
            with(vec![("$ENDDATA", "<absent>".into())]),
            RefusalCode::MissingLayout,
        ),
        (
            with(vec![("CYTIQ_ACQUISITION", "x".into())]),
            RefusalCode::DerivedFile,
        ),
        (
            Spec {
                delimiter: b'Z',
                keywords: vec![],
                ..padded()
            },
            RefusalCode::Delimiter,
        ),
        (
            Spec {
                raw_text: b"$cyt/another/".to_vec(),
                ..padded()
            },
            RefusalCode::RepeatedKeyword,
        ),
        (
            Spec {
                raw_text: b"V/\xff/".to_vec(),
                ..padded()
            },
            RefusalCode::StillRefused,
        ),
    ];
    for (spec, code) in cases {
        assert_eq!(refused(&spec.build()), code);
    }
    // HEADER offsets of a further segment.
    let mut bytes = Spec {
        text_start: 256,
        ..padded()
    }
    .build();
    bytes[58..74].copy_from_slice(b"     500     600");
    assert_eq!(refused(&bytes), RefusalCode::OtherSegments);
}

#[test]
fn overlapping_segments_are_refused() {
    let spec = Spec {
        padding: b" ".to_vec(),
        overrides: vec![
            ("$BEGINDATA", "00000100".into()),
            ("$ENDDATA", "00000123".into()),
        ],
        header: [None, Some((100, 123)), None],
        ..Spec::default()
    };
    assert_eq!(refused(&spec.build()), RefusalCode::SegmentsOverlap);
}

/// Every byte of a deviating file, changed or cut: never a panic, and whatever
/// is planned is read strictly and holds the window it names.
#[test]
fn no_change_makes_a_plan_the_reader_refuses() {
    let spec = Spec {
        raw_text: b"$INST//$EXP/a/".to_vec(),
        padding: b" \0".to_vec(),
        supplemental: Some(false),
        header: [None, None, Some((0, -1))],
        tail: b"tail".to_vec(),
        ..Spec::default()
    };
    let source = spec.build();
    let check = |bytes: &[u8]| {
        if let Diagnosis::Repairable { plan, .. } = found(bytes) {
            let repaired = repair(&plan, bytes);
            assert_eq!(found(&repaired), Diagnosis::Conforming);
            let at = usize::try_from(plan.data.offset).unwrap();
            let length = usize::try_from(plan.data.bytes).unwrap();
            let copied = &repaired[repaired.len() - 8 - length..repaired.len() - 8];
            assert_eq!(copied, &bytes[at..at + length]);
        }
    };
    for at in 0..source.len() {
        for value in [0, b' ', b'/', b'0', b'9', 0xff] {
            let mut changed = source.clone();
            changed[at] = value;
            check(&changed);
        }
        check(&source[..at]);
    }
}
