use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};

use crate::error::FcsError;
use crate::keywords::is_engine_keyword;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FcsVersion {
    V3_0,
    V3_1,
    V3_2,
}

impl FcsVersion {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V3_0 => "FCS3.0",
            Self::V3_1 => "FCS3.1",
            Self::V3_2 => "FCS3.2",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    #[must_use]
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// Length of the inclusive range.
    ///
    /// # Panics
    ///
    /// Panics unless `start <= end < u64::MAX`; the reader checks every range
    /// against the file length before asking for its length.
    #[must_use]
    pub const fn len(self) -> u64 {
        assert!(
            self.start <= self.end && self.end < u64::MAX,
            "byte range must be validated before taking its length"
        );
        self.end - self.start + 1
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start > self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub version: FcsVersion,
    pub text: ByteRange,
    pub data: Option<ByteRange>,
    pub analysis: Option<ByteRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextSegment {
    pub delimiter: u8,
    pub pairs: Vec<(String, String)>,
}

impl TextSegment {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Keyword values by upper-case name, and the names that have none because the
/// file repeats them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Keywords {
    values: HashMap<String, String>,
    quarantined: BTreeSet<String>,
}

impl Keywords {
    /// Add a pair. Keywords that differ only by case are the same keyword, so
    /// a second one leaves the value ambiguous: the file is refused when
    /// anything reads the keyword, which otherwise loses its value.
    pub(crate) fn insert(&mut self, key: &str, value: &str) -> Result<(), FcsError> {
        let name = key.to_ascii_uppercase();
        if self.quarantined.contains(&name) {
            return Ok(());
        }
        match self.values.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value.to_owned());
            }
            Entry::Occupied(_) if is_engine_keyword(key) => {
                return Err(FcsError::DuplicateKeyword(key.to_owned()));
            }
            Entry::Occupied(entry) => {
                self.quarantined.insert(entry.remove_entry().0);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FcsText {
    pub primary: TextSegment,
    pub supplemental: Option<TextSegment>,
    keywords: Keywords,
}

impl FcsText {
    pub(crate) fn new(
        primary: TextSegment,
        supplemental: Option<TextSegment>,
        keywords: Keywords,
    ) -> Self {
        Self {
            primary,
            supplemental,
            keywords,
        }
    }

    #[must_use]
    pub const fn primary(&self) -> &TextSegment {
        &self.primary
    }

    /// The value of `key`. Keywords are case-insensitive in every FCS version.
    /// A [quarantined](Self::quarantined) keyword has none.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keywords
            .values
            .get(&key.to_ascii_uppercase())
            .map(String::as_str)
    }

    /// Every keyword that has a value, in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        let quarantined = &self.keywords.quarantined;
        self.pairs().filter(move |(key, _)| {
            quarantined.is_empty() || !quarantined.contains(&key.to_ascii_uppercase())
        })
    }

    /// Every pair as the file holds it, quarantined ones included. To show a
    /// file, never to look a keyword up.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.primary
            .iter()
            .chain(self.supplemental.iter().flat_map(TextSegment::iter))
    }

    /// Upper-case names, sorted, of the keywords the file repeats and nothing
    /// reads. They have no value.
    pub fn quarantined(&self) -> impl Iterator<Item = &str> {
        self.keywords.quarantined.iter().map(String::as_str)
    }

    /// Merge a supplemental TEXT segment; a keyword already present is a
    /// repeated one.
    pub(crate) fn add_supplemental(&mut self, segment: TextSegment) -> Result<(), FcsError> {
        for (key, value) in &segment.pairs {
            self.keywords.insert(key, value)?;
        }
        self.supplemental = Some(segment);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterDataType {
    Ascii,
    Integer,
    Float,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterWidth {
    Bits(u8),
    AsciiChars(u8),
    DelimitedAscii,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    pub index: u16,
    pub name: String,
    pub label: Option<String>,
    pub width: ParameterWidth,
    pub data_type: ParameterDataType,
    pub keywords: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColumnData {
    UInt(Vec<u64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl ColumnData {
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::UInt(values) => values.len(),
            Self::F32(values) => values.len(),
            Self::F64(values) => values.len(),
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventColumns {
    pub columns: Vec<ColumnData>,
}

impl EventColumns {
    #[must_use]
    pub fn column(&self, index: usize) -> Option<&ColumnData> {
        self.columns.get(index)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FcsDataset {
    pub version: FcsVersion,
    pub header: Header,
    pub text: FcsText,
    pub parameters: Vec<Parameter>,
    pub data: EventColumns,
    pub analysis: Option<Vec<u8>>,
}
