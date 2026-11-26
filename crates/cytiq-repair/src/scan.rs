//! What a file declares, read leniently: only to find out how it deviates.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

use crate::codes::{Refusal, RefusalCode, RepairCode};
use crate::plan::Finding;
use crate::text::{self, Split};

pub(crate) const HEADER_BYTES: u64 = 58;
const HEADER_LENGTH: usize = 58;
/// The reader's own bound on TEXT.
const TEXT_BYTES_MAX: u64 = 16 << 20;
/// `$PAR`: parameter indexes are `u16`.
const PARAMETERS_MAX: u64 = 65_535;

/// An inclusive segment of the file, as FCS writes offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub start: u64,
    pub end: u64,
}

impl Segment {
    pub const fn bytes(self) -> u64 {
        self.end - self.start + 1
    }

    pub const fn range(self) -> Range<u64> {
        self.start..self.end + 1
    }
}

/// A HEADER entry for ANALYSIS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Analysis {
    None,
    /// `0` and `-1`.
    Negative,
    At(Segment),
}

/// HEADER and TEXT of a file, with the findings that concern them.
pub(crate) struct Scan {
    pub file_bytes: u64,
    /// `FCS3.0`, `FCS3.1` or `FCS3.2`.
    pub version: [u8; 6],
    pub header_text: Segment,
    pub header_data: Option<Segment>,
    pub header_analysis: Analysis,
    /// HEADER holds more than blank space after the ANALYSIS offsets.
    pub other_segments: bool,
    /// TEXT without a last byte that belongs to DATA.
    pub text: Vec<u8>,
    pub text_segment: Segment,
    pub split: Split,
    values: HashMap<String, Vec<u8>>,
    pub findings: Vec<Finding>,
}

/// Why a scan stopped: the file, or reading it.
pub(crate) enum Stop {
    Refused(Refusal),
    Io(io::Error),
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn refuse<T>(code: RefusalCode, message: impl Into<String>) -> Result<T, Stop> {
    Err(Stop::Refused(Refusal::new(code, message)))
}

pub(crate) fn scan<R: Read + Seek>(reader: &mut R) -> Result<Scan, Stop> {
    let file_bytes = reader.seek(SeekFrom::End(0))?;
    if file_bytes < HEADER_BYTES {
        return refuse(RefusalCode::NotFcs, "the file is shorter than HEADER");
    }
    let mut header = [0_u8; HEADER_LENGTH];
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut header)?;
    let version = version(&header)?;
    let header_text = match offsets(&header, 10, "TEXT")? {
        Offsets::At(segment) if segment.start >= HEADER_BYTES => segment,
        _ => {
            return refuse(
                RefusalCode::HeaderUnreadable,
                "HEADER names no TEXT segment",
            );
        }
    };
    if header_text.end >= file_bytes || header_text.bytes() > TEXT_BYTES_MAX {
        return refuse(
            RefusalCode::TextOutOfFile,
            format!(
                "TEXT is {}-{} in a file of {file_bytes} bytes",
                header_text.start, header_text.end
            ),
        );
    }
    let header_data = match offsets(&header, 26, "DATA")? {
        Offsets::Absent => None,
        Offsets::At(segment) => Some(segment),
        Offsets::Negative => {
            return refuse(RefusalCode::HeaderUnreadable, "a DATA offset is negative");
        }
    };
    let header_analysis = match offsets(&header, 42, "ANALYSIS")? {
        Offsets::Absent => Analysis::None,
        Offsets::At(segment) => Analysis::At(segment),
        Offsets::Negative => Analysis::Negative,
    };
    let other_segments = other_segments(reader, header_text.start)?;
    let mut text = read(reader, header_text)?;
    let mut text_segment = header_text;
    let mut findings = Vec::new();
    if header_data.is_some_and(|data| data.start == header_text.end) && text.len() > 1 {
        text.pop();
        text_segment.end -= 1;
        findings.push(Finding::new(
            RepairCode::TextOverlapsData,
            format!("TEXT ends at byte {}, where DATA begins", header_text.end),
        ));
    }
    let split = text::split(&text)?;
    if !split.padding.is_empty() {
        findings.push(Finding::new(
            RepairCode::TextTrailingPadding,
            format!(
                "{} spaces or NULs after the last delimiter",
                split.padding.len()
            ),
        ));
    }
    if !split.ambiguous.is_empty() {
        findings.push(Finding::new(
            RepairCode::TextEmptyValues,
            format!(
                "{} runs of pairs can be read in several ways",
                split.ambiguous.len()
            ),
        ));
    }
    let values = values(&text, &split)?;
    Ok(Scan {
        file_bytes,
        version,
        header_text,
        header_data,
        header_analysis,
        other_segments,
        text,
        text_segment,
        split,
        values,
        findings,
    })
}

fn version(header: &[u8]) -> Result<[u8; 6], Refusal> {
    let version: [u8; 6] = header[..6].try_into().expect("six bytes");
    if &version[..3] != b"FCS" {
        return Err(Refusal::new(RefusalCode::NotFcs, "not an FCS file"));
    }
    if header[6..10] != [b' '; 4] || ![&b"FCS3.0"[..], b"FCS3.1", b"FCS3.2"].contains(&&version[..])
    {
        return Err(Refusal::new(
            RefusalCode::UnsupportedVersion,
            format!("version `{}`", String::from_utf8_lossy(&header[..10])),
        ));
    }
    Ok(version)
}

enum Offsets {
    Absent,
    Negative,
    At(Segment),
}

/// A pair of HEADER offsets. Blank and zero pairs are absent.
fn offsets(header: &[u8], at: usize, name: &str) -> Result<Offsets, Refusal> {
    let unreadable = || {
        Refusal::new(
            RefusalCode::HeaderUnreadable,
            format!(
                "HEADER offsets of {name} are `{}`",
                String::from_utf8_lossy(&header[at..at + 16])
            ),
        )
    };
    let offset = |at: usize| -> Result<Option<i64>, Refusal> {
        let field = std::str::from_utf8(&header[at..at + 8]).map_err(|_| unreadable())?;
        match field.trim() {
            "" => Ok(None),
            number => number.parse().map(Some).map_err(|_| unreadable()),
        }
    };
    match (offset(at)?, offset(at + 8)?) {
        (None, None) | (Some(0), Some(0)) => Ok(Offsets::Absent),
        (Some(0), Some(-1)) => Ok(Offsets::Negative),
        (Some(start), Some(end)) => match (u64::try_from(start), u64::try_from(end)) {
            (Ok(start), Ok(end)) if start <= end => Ok(Offsets::At(Segment { start, end })),
            _ => Err(unreadable()),
        },
        _ => Err(unreadable()),
    }
}

/// HEADER may declare further segments after the ANALYSIS offsets.
fn other_segments<R: Read + Seek>(reader: &mut R, text_start: u64) -> Result<bool, Stop> {
    if text_start == HEADER_BYTES {
        return Ok(false);
    }
    let bytes = read(
        reader,
        Segment {
            start: HEADER_BYTES,
            end: text_start - 1,
        },
    )?;
    Ok(bytes.iter().any(|byte| !matches!(byte, b' ' | 0)))
}

fn read<R: Read + Seek>(reader: &mut R, segment: Segment) -> Result<Vec<u8>, Stop> {
    let bytes = segment.bytes();
    if bytes > TEXT_BYTES_MAX {
        return refuse(RefusalCode::TextOutOfFile, "more than 16 MiB before DATA");
    }
    reader.seek(SeekFrom::Start(segment.start))?;
    let mut buffer = vec![0; usize::try_from(bytes).expect("at most 16 MiB")];
    reader.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Values by upper-case keyword. A standard keyword has one value or the file
/// is refused; another that repeats has none.
fn values(text: &[u8], split: &Split) -> Result<HashMap<String, Vec<u8>>, Refusal> {
    let mut values: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    for pair in &split.pairs {
        let key = text::unescape(&text[pair.key.clone()], split.delimiter);
        let key = String::from_utf8_lossy(&key).to_ascii_uppercase();
        let value = text::unescape(&text[pair.value.clone()], split.delimiter);
        if key.starts_with("CYTIQ_") {
            return Err(Refusal::new(
                RefusalCode::DerivedFile,
                format!("the file records `{key}`"),
            ));
        }
        match values.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(Some(value));
            }
            Entry::Occupied(entry) if entry.key().starts_with('$') => {
                return Err(Refusal::new(
                    RefusalCode::RepeatedKeyword,
                    format!("`{}` appears more than once", entry.key()),
                ));
            }
            Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    Ok(values
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect())
}

impl Scan {
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::trim)
    }

    pub fn required(&self, key: &str) -> Result<&str, Refusal> {
        self.value(key)
            .ok_or_else(|| Refusal::new(RefusalCode::MissingLayout, format!("`{key}` is missing")))
    }

    pub fn number(&self, key: &str) -> Result<u64, Refusal> {
        let value = self.required(key)?;
        value.parse().map_err(|_| {
            Refusal::new(
                RefusalCode::MissingLayout,
                format!("`{key}` is `{value}`, not a number"),
            )
        })
    }

    /// A pair of offsets TEXT gives; `None` when both are 0 or absent.
    pub fn offsets(&self, begin: &str, end: &str) -> Result<Option<(u64, u64)>, Refusal> {
        match (self.value(begin), self.value(end)) {
            (None, None) => Ok(None),
            _ => match (self.number(begin)?, self.number(end)?) {
                (0, 0) => Ok(None),
                pair => Ok(Some(pair)),
            },
        }
    }

    /// Events, parameters and bytes of one event.
    pub fn layout(&self) -> Result<Layout, Refusal> {
        let unsupported = |message: String| Refusal::new(RefusalCode::UnsupportedLayout, message);
        let mode = self.required("$MODE")?;
        if mode != "L" {
            return Err(unsupported(format!("`$MODE` is `{mode}`")));
        }
        let parameters = self.number("$PAR")?;
        if parameters == 0 || parameters > PARAMETERS_MAX {
            return Err(unsupported(format!("`$PAR` is {parameters}")));
        }
        let datatype = self.required("$DATATYPE")?;
        let mut event_bytes = 0_u64;
        for index in 1..=parameters {
            let own = format!("$P{index}DATATYPE");
            let kind = match &self.version {
                b"FCS3.2" => self.value(&own).unwrap_or(datatype),
                _ => datatype,
            };
            let key = format!("$P{index}B");
            let width = self.required(&key)?;
            event_bytes += value_bytes(kind, width)
                .ok_or_else(|| unsupported(format!("`{key}` is `{width}` for type `{kind}`")))?;
        }
        let events = self.number("$TOT")?;
        let data_bytes = events
            .checked_mul(event_bytes)
            .ok_or_else(|| unsupported(format!("`$TOT` is {events}")))?;
        Ok(Layout {
            events,
            parameters,
            data_bytes,
        })
    }
}

/// Bytes one value takes, for the widths the reader supports.
fn value_bytes(kind: &str, width: &str) -> Option<u64> {
    let width: u64 = width.parse().ok()?;
    match kind {
        "A" if (1..=20).contains(&width) => Some(width),
        "I" if width.is_multiple_of(8) && (8..=64).contains(&width) => Some(width / 8),
        "F" if width == 32 => Some(4),
        "D" if width == 64 => Some(8),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub events: u64,
    pub parameters: u64,
    /// `$TOT` times the bytes of one event.
    pub data_bytes: u64,
}
