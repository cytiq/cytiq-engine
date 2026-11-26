//! How a file would be repaired: what is kept, changed and dropped.

use std::collections::BTreeSet;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::codes::{Refusal, RefusalCode, RepairCode};
use crate::repaired::{NO_CRC, Repaired};
use crate::scan::{Analysis, HEADER_BYTES, Layout, Scan, Segment, Stop};
use crate::text::{escape, unescape};
use crate::{REPAIR_KEYWORD, REPAIR_VERSION};

/// HEADER offsets are eight digits.
const HEADER_OFFSET_MAX: u64 = 99_999_999;
/// Offsets in TEXT are padded with zeros (FCS 3.1, 3.2.17), so that the length
/// of TEXT does not depend on them.
const OFFSET_DIGITS: usize = 20;
/// Vendor keywords that hold byte offsets of what follows DATA.
const TAIL_KEYWORDS: [&str; 2] = ["#NCBEGINSATURATEDPOPS", "#NCENDSATURATEDPOPS"];

/// A deviation that can be repaired, and what shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: RepairCode,
    pub evidence: String,
}

impl Finding {
    pub(crate) fn new(code: RepairCode, evidence: impl Into<String>) -> Self {
        Self {
            code,
            evidence: evidence.into(),
        }
    }
}

/// Bytes of the source that the repaired file does not hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Dropped {
    /// `text_padding`, `keyword`, `gap` (between segments) or `tail` (after
    /// the last one).
    pub what: &'static str,
    /// The keywords in those bytes, never their values.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The repair that drops them; none for blank space between segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<RepairCode>,
    /// Spaces and NULs only.
    pub blank: bool,
    pub offset: u64,
    pub bytes: u64,
    pub sha256: String,
}

/// An offset, or the CRC, that the repaired file states differently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub what: String,
    pub from: String,
    pub to: String,
}

/// DATA in the source: exactly `$TOT` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DataWindow {
    pub offset: u64,
    pub bytes: u64,
    pub events: u64,
    pub parameters: u64,
}

/// A pair kept as TEXT holds it, escapes included.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    /// Upper-case name.
    name: String,
    key: Vec<u8>,
    value: Vec<u8>,
}

/// A repair that the strict reader accepts the result of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// `FCS3.0`, `FCS3.1` or `FCS3.2`; the repaired file keeps it.
    pub version: String,
    pub findings: Vec<Finding>,
    pub changes: Vec<Change>,
    pub dropped: Vec<Dropped>,
    pub data: DataWindow,
    delimiter: u8,
    kept: Vec<Kept>,
    analysis: Option<Segment>,
}

impl Plan {
    /// The repairs the file needs.
    #[must_use]
    pub fn codes(&self) -> BTreeSet<RepairCode> {
        self.findings.iter().map(|finding| finding.code).collect()
    }

    /// The repaired file of `source`, whose SHA-256 is `acquisition`.
    pub fn repaired<R: Read + Seek>(&self, source: R, acquisition: &str) -> Repaired<R> {
        let data = self.data.offset..self.data.offset + self.data.bytes;
        let ranges = std::iter::once(data)
            .chain(self.analysis.map(Segment::range))
            .collect();
        Repaired::new(source, self.prefix(acquisition), ranges)
    }

    /// Where DATA and ANALYSIS go, behind a TEXT of `text` bytes.
    fn placed(&self, text: u64) -> (Segment, Option<Segment>) {
        let segment = |start, bytes| Segment {
            start,
            end: start + bytes - 1,
        };
        let data = segment(HEADER_BYTES + text, self.data.bytes);
        let analysis = self
            .analysis
            .map(|source| segment(data.end + 1, source.bytes()));
        (data, analysis)
    }

    fn prefix(&self, acquisition: &str) -> Vec<u8> {
        let zero = Segment { start: 0, end: 0 };
        let sized = self.text(acquisition, zero, self.analysis.map(|_| zero));
        let (data, analysis) = self.placed(sized.len() as u64);
        let text = self.text(acquisition, data, analysis);
        debug_assert_eq!(text.len(), sized.len());
        let text_segment = Segment {
            start: HEADER_BYTES,
            end: HEADER_BYTES + text.len() as u64 - 1,
        };
        let mut prefix = format!("{}    ", self.version).into_bytes();
        for segment in [
            Some(text_segment),
            header_entry(Some(data)),
            header_entry(analysis),
        ] {
            let (start, end) = segment.map_or((0, 0), |segment| (segment.start, segment.end));
            prefix.extend(format!("{start:>8}{end:>8}").into_bytes());
        }
        prefix.extend(text);
        prefix
    }

    fn text(&self, acquisition: &str, data: Segment, analysis: Option<Segment>) -> Vec<u8> {
        let mut text = vec![self.delimiter];
        let mut push = |key: &[u8], value: &[u8]| {
            for token in [key, value] {
                text.extend_from_slice(token);
                text.push(self.delimiter);
            }
        };
        for pair in &self.kept {
            match offset_value(&pair.name, data, analysis) {
                Some(value) => push(&pair.key, value.as_bytes()),
                None => push(&pair.key, &pair.value),
            }
        }
        for (key, value) in [
            (cytiq_fcs::ACQUISITION_KEYWORD, acquisition.to_owned()),
            (REPAIR_KEYWORD, self.keyword()),
        ] {
            push(
                &escape(key.as_bytes(), self.delimiter),
                &escape(value.as_bytes(), self.delimiter),
            );
        }
        text
    }

    /// What `CYTIQ_REPAIR` holds.
    fn keyword(&self) -> String {
        let codes: Vec<&str> = self.codes().into_iter().map(RepairCode::as_str).collect();
        serde_json::json!({ "version": REPAIR_VERSION, "codes": codes }).to_string()
    }
}

/// HEADER holds zeros for a segment that does not fit in its eight digits.
fn header_entry(segment: Option<Segment>) -> Option<Segment> {
    segment.filter(|segment| segment.end <= HEADER_OFFSET_MAX)
}

/// The value of a keyword that states an offset of the repaired file.
fn offset_value(name: &str, data: Segment, analysis: Option<Segment>) -> Option<String> {
    let padded = |offset: u64| format!("{offset:0OFFSET_DIGITS$}");
    let analysis = |offset: fn(Segment) -> u64| {
        analysis.map_or_else(|| "0".to_owned(), |segment| padded(offset(segment)))
    };
    match name {
        "$BEGINDATA" => Some(padded(data.start)),
        "$ENDDATA" => Some(padded(data.end)),
        "$BEGINSTEXT" | "$ENDSTEXT" => Some("0".to_owned()),
        "$BEGINANALYSIS" => Some(analysis(|segment| segment.start)),
        "$ENDANALYSIS" => Some(analysis(|segment| segment.end)),
        _ => None,
    }
}

fn refuse<T>(code: RefusalCode, message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::new(code, message))
}

/// Plan the repair of a scanned file.
pub(crate) fn plan<R: Read + Seek>(reader: &mut R, scan: &Scan) -> Result<Plan, Stop> {
    check_delimiter(scan.split.delimiter)?;
    check_single_data_set(scan)?;
    let layout = scan.layout()?;
    let mut findings = scan.findings.clone();
    let data = data_window(scan, layout, &mut findings)?;
    check_other_segments(scan)?;
    check_supplemental(scan, &mut findings)?;
    let analysis = analysis(scan, &mut findings)?;
    let mut declared = vec![scan.text_segment, data];
    declared.extend(analysis);
    check_overlap(&mut declared)?;
    let mut dropped = dropped_text(scan);
    let tail = undeclared(reader, scan, &declared, &mut dropped)?;
    let mut kept = kept(scan);
    if tail.remainder {
        drop_tail_keywords(scan, &mut kept, &mut dropped);
    }
    if dropped
        .iter()
        .any(|entry| entry.code == Some(RepairCode::UndeclaredBytes))
    {
        findings.push(Finding::new(
            RepairCode::UndeclaredBytes,
            "bytes or keywords that no declared segment holds would be lost",
        ));
    }
    let mut plan = Plan {
        version: String::from_utf8_lossy(&scan.version).into_owned(),
        findings,
        changes: Vec::new(),
        dropped,
        data: DataWindow {
            offset: data.start,
            bytes: data.bytes(),
            events: layout.events,
            parameters: layout.parameters,
        },
        delimiter: scan.split.delimiter,
        kept,
        analysis,
    };
    plan.changes = changes(&plan, scan, tail.crc)?;
    Ok(plan)
}

/// The new keywords must not start with the delimiter, and the acquisition is
/// hexadecimal.
fn check_delimiter(delimiter: u8) -> Result<(), Refusal> {
    if delimiter.is_ascii_alphanumeric() || delimiter == b'{' || !delimiter.is_ascii() {
        return refuse(
            RefusalCode::Delimiter,
            format!("TEXT is delimited by `{}`", delimiter.escape_ascii()),
        );
    }
    Ok(())
}

fn check_single_data_set(scan: &Scan) -> Result<(), Refusal> {
    if scan.value("$NEXTDATA").is_some() && scan.number("$NEXTDATA")? != 0 {
        return refuse(
            RefusalCode::MultipleDataSets,
            "`$NEXTDATA` names another data set",
        );
    }
    Ok(())
}

/// The only segment DATA can be.
fn data_window(
    scan: &Scan,
    layout: Layout,
    findings: &mut Vec<Finding>,
) -> Result<Segment, Refusal> {
    let Some((start, end)) = scan.offsets("$BEGINDATA", "$ENDDATA")? else {
        return refuse(RefusalCode::MissingLayout, "TEXT gives no DATA offsets");
    };
    if let Some(header) = scan.header_data
        && (header.start, header.end) != (start, end)
    {
        return refuse(
            RefusalCode::AmbiguousDataOffsets,
            format!(
                "HEADER puts DATA at {}-{}, TEXT at {start}-{end}",
                header.start, header.end
            ),
        );
    }
    let need = layout.data_bytes;
    let declared = end
        .checked_sub(start)
        .and_then(|bytes| bytes.checked_add(1))
        .filter(|_| start >= HEADER_BYTES && need > 0);
    let mismatch = || {
        Refusal::new(
            RefusalCode::DataLengthMismatch,
            format!(
                "DATA is {start}-{end} in a file of {} bytes; `$TOT` and the widths need {need} \
                 bytes",
                scan.file_bytes
            ),
        )
    };
    let declared = declared.ok_or_else(mismatch)?;
    if declared == need && end < scan.file_bytes {
        return Ok(Segment { start, end });
    }
    if declared == need + 1 && end == scan.file_bytes {
        // Starting one byte later would end outside the file.
        findings.push(Finding::new(
            RepairCode::DataEndExclusive,
            format!(
                "DATA is {start}-{end}, one byte more than {need}, and the file ends at {}",
                end - 1
            ),
        ));
        return Ok(Segment {
            start,
            end: end - 1,
        });
    }
    if declared == need + 1 && end < scan.file_bytes {
        return refuse(
            RefusalCode::AmbiguousDataOffsets,
            format!("DATA is {start}-{end}, one byte more than {need}: either end may be wrong"),
        );
    }
    Err(mismatch())
}

/// A rewritten file has nowhere to put what nothing describes.
fn check_other_segments(scan: &Scan) -> Result<(), Refusal> {
    if scan.other_segments {
        return refuse(
            RefusalCode::OtherSegments,
            "HEADER declares segments, or holds bytes, after the ANALYSIS offsets",
        );
    }
    Ok(())
}

fn check_supplemental(scan: &Scan, findings: &mut Vec<Finding>) -> Result<(), Refusal> {
    let Some((start, end)) = scan.offsets("$BEGINSTEXT", "$ENDSTEXT")? else {
        return Ok(());
    };
    let whole = (start, end) == (scan.header_text.start, scan.header_text.end);
    let text = scan.text_segment;
    let delimiter = start == end
        && (text.start..=text.end).contains(&start)
        && usize::try_from(start - text.start)
            .is_ok_and(|at| scan.text[at] == scan.split.delimiter);
    if !(whole || delimiter) {
        return refuse(
            RefusalCode::SupplementalText,
            format!("a supplemental TEXT segment at {start}-{end}"),
        );
    }
    findings.push(Finding::new(
        RepairCode::SupplementalTextInsideText,
        format!(
            "supplemental TEXT is {start}-{end}, {} TEXT",
            if whole {
                "which is"
            } else {
                "one delimiter of"
            }
        ),
    ));
    Ok(())
}

fn analysis(scan: &Scan, findings: &mut Vec<Finding>) -> Result<Option<Segment>, Refusal> {
    let in_text = scan.offsets("$BEGINANALYSIS", "$ENDANALYSIS")?;
    let disagree = |header: &str| {
        refuse(
            RefusalCode::AmbiguousAnalysis,
            format!("HEADER puts ANALYSIS at {header}, TEXT at {in_text:?}"),
        )
    };
    let segment = match (scan.header_analysis, in_text) {
        (Analysis::None, None) => None,
        (Analysis::Negative, None) => {
            findings.push(Finding::new(
                RepairCode::AnalysisOffsetsNegative,
                "HEADER ends ANALYSIS at -1, and TEXT has none",
            ));
            None
        }
        (Analysis::Negative, Some(_)) => return disagree("0--1"),
        (Analysis::At(header), None) => Some(header),
        (Analysis::At(header), Some(text)) => {
            if (header.start, header.end) != text {
                return disagree(&format!("{}-{}", header.start, header.end));
            }
            Some(header)
        }
        (Analysis::None, Some((start, end))) => Some(Segment { start, end }),
    };
    match segment {
        Some(found)
            if found.start > found.end
                || found.start < HEADER_BYTES
                || found.end >= scan.file_bytes =>
        {
            refuse(
                RefusalCode::AmbiguousAnalysis,
                format!(
                    "ANALYSIS at {}-{} is not in the file",
                    found.start, found.end
                ),
            )
        }
        found => Ok(found),
    }
}

fn check_overlap(segments: &mut [Segment]) -> Result<(), Refusal> {
    segments.sort_by_key(|segment| segment.start);
    for pair in segments.windows(2) {
        if pair[1].start <= pair[0].end {
            return refuse(
                RefusalCode::SegmentsOverlap,
                format!(
                    "segments {}-{} and {}-{} share bytes",
                    pair[0].start, pair[0].end, pair[1].start, pair[1].end
                ),
            );
        }
    }
    Ok(())
}

fn hex(hasher: Sha256) -> String {
    use std::fmt::Write as _;
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn is_blank(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| matches!(byte, b' ' | 0))
}

/// What TEXT loses: its padding, and the pairs its readings disagree on.
fn dropped_text(scan: &Scan) -> Vec<Dropped> {
    let entry = |what, keywords, code, range: &Range<usize>| {
        let bytes = &scan.text[range.clone()];
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Dropped {
            what,
            keywords,
            code: Some(code),
            blank: is_blank(bytes),
            offset: scan.text_segment.start + range.start as u64,
            bytes: bytes.len() as u64,
            sha256: hex(hasher),
        }
    };
    let mut dropped: Vec<Dropped> = scan
        .split
        .ambiguous
        .iter()
        .map(|range| {
            let names = ambiguous_names(&scan.text[range.clone()], scan.split.delimiter);
            entry("keyword", names, RepairCode::TextEmptyValues, range)
        })
        .collect();
    if !scan.split.padding.is_empty() {
        dropped.push(entry(
            "text_padding",
            Vec::new(),
            RepairCode::TextTrailingPadding,
            &scan.split.padding,
        ));
    }
    dropped
}

/// The names between doubled delimiters, without a value that follows them.
fn ambiguous_names(region: &[u8], delimiter: u8) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = region;
    loop {
        let end = rest
            .iter()
            .position(|byte| *byte == delimiter)
            .unwrap_or(rest.len());
        if end == 0 {
            return names;
        }
        names.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        if rest.get(end + 1) != Some(&delimiter) {
            return names;
        }
        rest = &rest[end + 2..];
    }
}

/// What follows the last segment.
struct Tail {
    crc: Option<[u8; 8]>,
    /// More than a CRC follows.
    remainder: bool,
}

/// List the bytes no segment holds.
fn undeclared<R: Read + Seek>(
    reader: &mut R,
    scan: &Scan,
    declared: &[Segment],
    dropped: &mut Vec<Dropped>,
) -> io::Result<Tail> {
    let mut at = HEADER_BYTES;
    for segment in declared {
        drop_range(reader, "gap", at..segment.start, dropped)?;
        at = segment.end + 1;
    }
    let mut crc = [0_u8; 8];
    let has_crc = scan.file_bytes - at >= 8 && {
        reader.seek(SeekFrom::Start(at))?;
        reader.read_exact(&mut crc)?;
        crc.iter().all(u8::is_ascii_digit)
    };
    let tail = if has_crc { at + 8 } else { at };
    drop_range(reader, "tail", tail..scan.file_bytes, dropped)?;
    Ok(Tail {
        crc: has_crc.then_some(crc),
        remainder: tail < scan.file_bytes,
    })
}

fn drop_range<R: Read + Seek>(
    reader: &mut R,
    what: &'static str,
    range: Range<u64>,
    dropped: &mut Vec<Dropped>,
) -> io::Result<()> {
    if range.is_empty() {
        return Ok(());
    }
    reader.seek(SeekFrom::Start(range.start))?;
    let mut left = range.end - range.start;
    let mut buffer = vec![0_u8; 1 << 16];
    let mut hasher = Sha256::new();
    let mut blank = true;
    while left > 0 {
        let count = usize::try_from(left).map_or(buffer.len(), |left| left.min(buffer.len()));
        reader.read_exact(&mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        blank &= is_blank(&buffer[..count]);
        left -= count as u64;
    }
    dropped.push(Dropped {
        what,
        keywords: Vec::new(),
        code: (!blank).then_some(RepairCode::UndeclaredBytes),
        blank,
        offset: range.start,
        bytes: range.end - range.start,
        sha256: hex(hasher),
    });
    Ok(())
}

fn kept(scan: &Scan) -> Vec<Kept> {
    scan.split
        .pairs
        .iter()
        .map(|pair| {
            let key = scan.text[pair.key.clone()].to_vec();
            let name = unescape(&key, scan.split.delimiter);
            Kept {
                name: String::from_utf8_lossy(&name).to_ascii_uppercase(),
                key,
                value: scan.text[pair.value.clone()].to_vec(),
            }
        })
        .collect()
}

/// Offsets of bytes the repaired file does not hold would name other bytes.
fn drop_tail_keywords(scan: &Scan, kept: &mut Vec<Kept>, dropped: &mut Vec<Dropped>) {
    let delimiter = scan.split.delimiter;
    for pair in &scan.split.pairs {
        let key = unescape(&scan.text[pair.key.clone()], delimiter);
        let name = String::from_utf8_lossy(&key).into_owned();
        if !TAIL_KEYWORDS.contains(&name.to_ascii_uppercase().as_str()) {
            continue;
        }
        let range = pair.key.start..pair.value.end;
        let mut hasher = Sha256::new();
        hasher.update(&scan.text[range.clone()]);
        dropped.push(Dropped {
            what: "keyword",
            keywords: vec![name],
            code: Some(RepairCode::UndeclaredBytes),
            blank: false,
            offset: scan.text_segment.start + range.start as u64,
            bytes: range.len() as u64,
            sha256: hex(hasher),
        });
    }
    kept.retain(|pair| !TAIL_KEYWORDS.contains(&pair.name.as_str()));
}

/// What the repaired file states differently from the source.
fn changes(plan: &Plan, scan: &Scan, crc: Option<[u8; 8]>) -> Result<Vec<Change>, Refusal> {
    let zero = Segment { start: 0, end: 0 };
    let sized = plan.text(&"0".repeat(64), zero, plan.analysis.map(|_| zero));
    let text = Segment {
        start: HEADER_BYTES,
        end: HEADER_BYTES + sized.len() as u64 - 1,
    };
    let (data, analysis) = plan.placed(sized.len() as u64);
    let names_analysis = plan.kept.iter().any(|pair| pair.name == "$BEGINANALYSIS");
    if analysis.is_some() && header_entry(analysis).is_none() && !names_analysis {
        return refuse(
            RefusalCode::AmbiguousAnalysis,
            "ANALYSIS would move beyond HEADER's eight digits, and TEXT does not name it",
        );
    }
    let show = |segment: Option<Segment>| {
        segment.map_or_else(
            || "0-0".to_owned(),
            |segment| format!("{}-{}", segment.start, segment.end),
        )
    };
    let header_analysis = match scan.header_analysis {
        Analysis::None => "0-0".to_owned(),
        Analysis::Negative => "0--1".to_owned(),
        Analysis::At(segment) => show(Some(segment)),
    };
    let mut changes = vec![
        (
            "HEADER TEXT".to_owned(),
            show(Some(scan.header_text)),
            show(Some(text)),
        ),
        (
            "HEADER DATA".to_owned(),
            show(scan.header_data),
            show(header_entry(Some(data))),
        ),
        (
            "HEADER ANALYSIS".to_owned(),
            header_analysis,
            show(header_entry(analysis)),
        ),
    ];
    for pair in &plan.kept {
        if let Some(to) = offset_value(&pair.name, data, analysis) {
            let from = unescape(&pair.value, plan.delimiter);
            changes.push((
                pair.name.clone(),
                String::from_utf8_lossy(&from).into_owned(),
                to,
            ));
        }
    }
    if let Some(crc) = crc {
        changes.push((
            "CRC".to_owned(),
            String::from_utf8_lossy(&crc).into_owned(),
            String::from_utf8_lossy(NO_CRC).into_owned(),
        ));
    }
    Ok(changes
        .into_iter()
        .filter(|(_, from, to)| from != to)
        .map(|(what, from, to)| Change { what, from, to })
        .collect())
}
