use std::collections::HashMap;

use crate::error::FcsError;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FcsText {
    pub primary: TextSegment,
    pub supplemental: Option<TextSegment>,
    lookup: HashMap<String, String>,
}

impl FcsText {
    pub(crate) fn new(
        primary: TextSegment,
        supplemental: Option<TextSegment>,
        lookup: HashMap<String, String>,
    ) -> Self {
        Self {
            primary,
            supplemental,
            lookup,
        }
    }

    #[must_use]
    pub const fn primary(&self) -> &TextSegment {
        &self.primary
    }

    /// The value of `key`. Keywords are case-insensitive in every FCS version.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lookup
            .get(&key.to_ascii_uppercase())
            .map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.primary
            .iter()
            .chain(self.supplemental.iter().flat_map(TextSegment::iter))
    }

    /// Merge a supplemental TEXT segment, rejecting keywords already present.
    pub(crate) fn add_supplemental(&mut self, segment: TextSegment) -> Result<(), FcsError> {
        for (key, value) in &segment.pairs {
            Self::insert_lookup(&mut self.lookup, key, value)?;
        }
        self.supplemental = Some(segment);
        Ok(())
    }

    pub(crate) fn insert_lookup(
        lookup: &mut HashMap<String, String>,
        key: &str,
        value: &str,
    ) -> Result<(), FcsError> {
        // Keywords that differ only by case are the same keyword, so a second
        // one would leave the value ambiguous.
        if lookup
            .insert(key.to_ascii_uppercase(), value.to_owned())
            .is_some()
        {
            Err(FcsError::DuplicateKeyword(key.to_owned()))
        } else {
            Ok(())
        }
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
