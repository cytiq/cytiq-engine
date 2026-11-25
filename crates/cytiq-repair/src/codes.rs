//! What can be repaired, and why a file is not. Both are stable identifiers.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One repair. Each is applied only when the caller allows it by name, and
/// only when DATA can be read in exactly one way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairCode {
    /// Spaces or NULs after the last delimiter of TEXT.
    TextTrailingPadding,
    /// The last byte of TEXT is the first byte of DATA.
    TextOverlapsData,
    /// A doubled delimiter where a keyword name is expected: an empty value or
    /// an escaped delimiter. The pairs the readings disagree on are dropped.
    TextEmptyValues,
    /// The end of DATA is one past its last byte, which is the last of the file.
    DataEndExclusive,
    /// The supplemental TEXT offsets name TEXT itself, or its delimiter.
    SupplementalTextInsideText,
    /// HEADER says ANALYSIS ends at -1, and TEXT that there is none.
    AnalysisOffsetsNegative,
    /// Bytes no segment declares hold something. They are dropped, with the
    /// vendor keywords that point at them.
    UndeclaredBytes,
}

impl RepairCode {
    pub const ALL: [Self; 7] = [
        Self::TextTrailingPadding,
        Self::TextOverlapsData,
        Self::TextEmptyValues,
        Self::DataEndExclusive,
        Self::SupplementalTextInsideText,
        Self::AnalysisOffsetsNegative,
        Self::UndeclaredBytes,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextTrailingPadding => "text_trailing_padding",
            Self::TextOverlapsData => "text_overlaps_data",
            Self::TextEmptyValues => "text_empty_values",
            Self::DataEndExclusive => "data_end_exclusive",
            Self::SupplementalTextInsideText => "supplemental_text_inside_text",
            Self::AnalysisOffsetsNegative => "analysis_offsets_negative",
            Self::UndeclaredBytes => "undeclared_bytes",
        }
    }
}

impl fmt::Display for RepairCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a file is not repaired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// Not an FCS file, or cut off inside HEADER.
    NotFcs,
    /// Not FCS 3.0, 3.1 or 3.2.
    UnsupportedVersion,
    /// A HEADER offset is not a number, or a segment ends before it starts.
    HeaderUnreadable,
    /// TEXT is outside the file, or larger than the reader accepts.
    TextOutOfFile,
    /// TEXT ends inside a value, or with a keyword that has none.
    TextUnterminated,
    /// TEXT can be split into keywords in several ways.
    AmbiguousText,
    /// TEXT is delimited by a character the new keywords need.
    Delimiter,
    /// A standard keyword appears more than once.
    RepeatedKeyword,
    /// A keyword that describes DATA is missing or unreadable.
    MissingLayout,
    /// A layout the reader does not support.
    UnsupportedLayout,
    /// `$NEXTDATA` names another data set.
    MultipleDataSets,
    /// A supplemental TEXT segment of its own.
    SupplementalText,
    /// HEADER declares segments after ANALYSIS.
    OtherSegments,
    /// HEADER and TEXT, or the offsets and `$TOT`, allow several DATA segments.
    AmbiguousDataOffsets,
    /// DATA does not hold `$TOT` events.
    DataLengthMismatch,
    /// HEADER and TEXT disagree about ANALYSIS.
    AmbiguousAnalysis,
    /// Two segments share bytes.
    SegmentsOverlap,
    /// A file cytiq already wrote or repaired.
    DerivedFile,
    /// The strict reader would still refuse the repaired file.
    StillRefused,
}

impl RefusalCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFcs => "not_fcs",
            Self::UnsupportedVersion => "unsupported_version",
            Self::HeaderUnreadable => "header_unreadable",
            Self::TextOutOfFile => "text_out_of_file",
            Self::TextUnterminated => "text_unterminated",
            Self::AmbiguousText => "ambiguous_text",
            Self::Delimiter => "delimiter",
            Self::RepeatedKeyword => "repeated_keyword",
            Self::MissingLayout => "missing_layout",
            Self::UnsupportedLayout => "unsupported_layout",
            Self::MultipleDataSets => "multiple_data_sets",
            Self::SupplementalText => "supplemental_text",
            Self::OtherSegments => "other_segments",
            Self::AmbiguousDataOffsets => "ambiguous_data_offsets",
            Self::DataLengthMismatch => "data_length_mismatch",
            Self::AmbiguousAnalysis => "ambiguous_analysis",
            Self::SegmentsOverlap => "segments_overlap",
            Self::DerivedFile => "derived_file",
            Self::StillRefused => "still_refused",
        }
    }
}

impl fmt::Display for RefusalCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a file is not repaired, with what was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Refusal {
    pub code: RefusalCode,
    pub message: String,
}

impl Refusal {
    pub(crate) fn new(code: RefusalCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_the_names_they_serialize_to() {
        for code in RepairCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
        }
        for code in [RefusalCode::NotFcs, RefusalCode::StillRefused] {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
        }
    }
}
