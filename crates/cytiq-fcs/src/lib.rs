mod acquisition;
mod channels;
mod compensation;
mod data;
mod error;
mod header;
#[cfg(feature = "inputs")]
pub mod inputs;
mod layout;
mod limits;
mod lineage;
mod linear;
mod margins;
mod signal;
mod stream;
mod text;
mod types;
mod writer;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroUsize;
use std::path::Path;

use log::debug;

use crate::data::{decode_data, decoded_data_length, ensure_data_length, read_raw_segment};
use crate::error::FcsError;
use crate::header::read_header;
use crate::layout::{ResolvedLayout, resolve_layout};
use crate::limits::TEXT_BYTES_MAX;
use crate::text::{build_text, parse_segment_range, read_text_segment};
use crate::types::{ByteRange, FcsText, Header};

pub use crate::acquisition::{ACQUISITION_KEYWORD, acquisition_id, is_sha256, set_acquisition};
pub use crate::channels::{is_scatter_channel, parse_channel_list};
pub use crate::compensation::{
    Spillover, compensate, condition_number_1, find_parameter_index, invert_matrix,
    is_spillover_keyword,
};
pub use crate::error::FcsError as Error;
pub use crate::limits::FcsReadLimits;
pub use crate::lineage::{ORIGINAL_ID_PARAMETER, source_event_ids};
pub use crate::linear::{ensure_linear_scale, linear_values, log_amplified_parameters};
pub use crate::margins::{
    MARGINS_KEYWORD, MarginBounds, SaturationAssessment, SaturationChannelCounts,
    SaturationDiagnostics, assess_detector_saturation, dataset_margin_bounds, encode_margin_bounds,
    margin_channels, raw_margin_pass, recorded_margin_bounds,
};
pub use crate::signal::{
    COMPENSATION_KEYWORD, CompensationState, NORMALIZATION_KEYWORD, NormalizationState,
    SOURCE_SPILLOVER_KEYWORD, SignalState, TRANSFORM_KEYWORD, TransformContract, TransformState,
    UNMIXING_KEYWORD, UnmixingState, compensation_state, normalization_state, transform_state,
    unmixing_state,
};
pub use crate::stream::{EventBatches, FcsMetadata, FcsStream};
pub use crate::types::{
    ColumnData, EventColumns, FcsDataset, FcsText as Text, FcsVersion, Parameter,
    ParameterDataType, ParameterWidth, TextSegment,
};
pub use crate::writer::{
    FcsWriteOptions, FcsWriteParameter, carried_keywords, needs_f64,
    write_fcs3_1_exact_with_options, write_fcs3_1_f32, write_fcs3_1_f32_to_writer,
    write_fcs3_1_f32_to_writer_with_options, write_fcs3_1_f32_with_options,
    write_fcs3_1_f64_to_writer_with_options, write_fcs3_1_f64_with_options,
};

#[derive(Debug)]
pub struct FcsReader<R> {
    reader: R,
}

impl FcsReader<File> {
    /// Open an FCS file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be opened for reading.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FcsError> {
        debug!("opening FCS file {}", path.as_ref().display());
        let reader = File::open(path).map_err(FcsError::Io)?;
        Ok(Self::new(reader))
    }
}

impl<R: Read + Seek> FcsReader<R> {
    pub const fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Read the FCS dataset from the wrapped reader, within the default
    /// [`FcsReadLimits`].
    ///
    /// # Errors
    ///
    /// Returns an error when the file does not conform to the supported FCS
    /// 3.0, 3.1, or 3.2 layouts, when required metadata is missing, when it
    /// declares more than the limits allow, or when the underlying reader
    /// fails.
    pub fn read(self) -> Result<FcsDataset, FcsError> {
        self.read_with_limits(FcsReadLimits::default())
    }

    /// Read the FCS dataset, refusing a file whose decoded DATA or ANALYSIS
    /// exceeds `limits` before allocating for either.
    ///
    /// # Errors
    ///
    /// As [`FcsReader::read`]; [`FcsError::LimitExceeded`] names the limit.
    pub fn read_with_limits(mut self, limits: FcsReadLimits) -> Result<FcsDataset, FcsError> {
        let Prelude {
            header,
            text,
            data_range,
            analysis_range,
            layout: resolved_layout,
        } = read_prelude(&mut self.reader, limits)?;
        if let Some(range) = analysis_range {
            ensure_within_limit("ANALYSIS bytes", range.len(), limits.analysis_bytes_max)?;
        }
        let parameters = resolved_layout.public_parameters();
        let data = decode_data(&mut self.reader, data_range, &resolved_layout)?;
        let analysis = analysis_range
            .map(|range| read_raw_segment(&mut self.reader, range))
            .transpose()?;

        let event_count = data.columns.first().map_or(0, ColumnData::len);
        debug!(
            "parsed FCS dataset: version={:?}, parameters={}, events={}, analysis_bytes={}",
            header.version,
            parameters.len(),
            event_count,
            analysis.as_ref().map_or(0, Vec::len)
        );

        Ok(FcsDataset {
            version: header.version,
            header,
            text,
            parameters,
            data,
            analysis,
        })
    }

    /// Check the file as [`FcsReader::read_with_limits`] does, then return its
    /// metadata and an iterator that decodes DATA `batch_size` events at a
    /// time. Memory holds one batch, not the file. ANALYSIS is never read, so
    /// `limits.analysis_bytes_max` does not apply.
    ///
    /// # Errors
    ///
    /// As [`FcsReader::read_with_limits`], for everything up to DATA; a batch
    /// that fails to decode is an `Err` item, after which the iterator ends.
    pub fn stream(
        mut self,
        batch_size: NonZeroUsize,
        limits: FcsReadLimits,
    ) -> Result<FcsStream<R>, FcsError> {
        let Prelude {
            header,
            text,
            data_range,
            layout,
            ..
        } = read_prelude(&mut self.reader, limits)?;
        let metadata = FcsMetadata {
            version: header.version,
            header,
            text,
            parameters: layout.public_parameters(),
            event_count: layout.row_count(),
        };
        let batches = EventBatches::new(self.reader, data_range, layout, batch_size)?;
        Ok(FcsStream { metadata, batches })
    }
}

/// What a file declares, checked, before anything past TEXT is read.
struct Prelude {
    header: Header,
    text: FcsText,
    data_range: ByteRange,
    analysis_range: Option<ByteRange>,
    layout: ResolvedLayout,
}

/// Read and check HEADER and TEXT, resolve the segments and the layout, and
/// check DATA against the layout and the decoded-size limit.
fn read_prelude<R: Read + Seek>(
    reader: &mut R,
    limits: FcsReadLimits,
) -> Result<Prelude, FcsError> {
    let file_len = file_length(reader)?;
    reader.seek(SeekFrom::Start(0)).map_err(FcsError::Io)?;

    let header = read_header(reader)?;
    ensure_in_file("TEXT", header.text, file_len)?;
    ensure_within_limit("TEXT bytes", header.text.len(), TEXT_BYTES_MAX)?;
    let primary_segment = read_text_segment(reader, header.text, "TEXT")?;
    let mut text = build_text(primary_segment, None)?;
    let supplemental_range = parse_segment_range(
        text.primary(),
        "$BEGINSTEXT",
        "$ENDSTEXT",
        "supplemental TEXT",
        false,
    )?;
    if let Some(range) = supplemental_range {
        ensure_in_file("supplemental TEXT", range, file_len)?;
        ensure_within_limit(
            "TEXT bytes",
            header.text.len().saturating_add(range.len()),
            TEXT_BYTES_MAX,
        )?;
        let segment = read_text_segment(reader, range, "supplemental TEXT")?;
        text.add_supplemental(segment)?;
    }

    let data_range = resolve_required_range(&header, &text, "$BEGINDATA", "$ENDDATA", "DATA")?;
    let analysis_range =
        resolve_optional_range(&header, &text, "$BEGINANALYSIS", "$ENDANALYSIS", "ANALYSIS")?;
    validate_segments(
        file_len,
        header.text,
        supplemental_range,
        data_range,
        analysis_range,
    )?;

    let resolved_layout = resolve_layout(header.version, &text)?;
    ensure_data_length(data_range, &resolved_layout)?;
    ensure_within_limit(
        "decoded DATA bytes",
        decoded_data_length(&resolved_layout),
        limits.decoded_bytes_max,
    )?;
    Ok(Prelude {
        header,
        text,
        data_range,
        analysis_range,
        layout: resolved_layout,
    })
}

fn file_length<R: Read + Seek>(reader: &mut R) -> Result<u64, FcsError> {
    let current = reader.stream_position().map_err(FcsError::Io)?;
    let end = reader.seek(SeekFrom::End(0)).map_err(FcsError::Io)?;
    reader
        .seek(SeekFrom::Start(current))
        .map_err(FcsError::Io)?;
    Ok(end)
}

fn resolve_required_range(
    header: &Header,
    text: &FcsText,
    begin_key: &str,
    end_key: &str,
    name: &'static str,
) -> Result<ByteRange, FcsError> {
    let text_range = parse_segment_range(text.primary(), begin_key, end_key, name, true)?
        .ok_or_else(|| {
            FcsError::MissingKeyword(match name {
                "DATA" => begin_key.to_owned(),
                _ => end_key.to_owned(),
            })
        })?;
    let header_range = match name {
        "DATA" => header.data,
        "ANALYSIS" => header.analysis,
        _ => None,
    };

    if let Some(range) = header_range {
        ensure_matching_ranges(name, range, text_range)?;
    }

    Ok(text_range)
}

fn resolve_optional_range(
    header: &Header,
    text: &FcsText,
    begin_key: &str,
    end_key: &str,
    name: &'static str,
) -> Result<Option<ByteRange>, FcsError> {
    let text_range = parse_segment_range(text.primary(), begin_key, end_key, name, false)?;
    let header_range = match name {
        "DATA" => header.data,
        "ANALYSIS" => header.analysis,
        _ => None,
    };

    match (header_range, text_range) {
        (Some(header_value), Some(text_value)) => {
            ensure_matching_ranges(name, header_value, text_value)?;
            Ok(Some(text_value))
        }
        (Some(header_value), None) => Ok(Some(header_value)),
        (None, value) => Ok(value),
    }
}

fn ensure_matching_ranges(
    name: &'static str,
    header_value: ByteRange,
    text_value: ByteRange,
) -> Result<(), FcsError> {
    if header_value == text_value {
        Ok(())
    } else {
        Err(FcsError::OffsetMismatch {
            name,
            header_start: header_value.start,
            header_end: header_value.end,
            text_start: text_value.start,
            text_end: text_value.end,
        })
    }
}

fn validate_segments(
    file_len: u64,
    text_range: ByteRange,
    supplemental_text_range: Option<ByteRange>,
    data_range: ByteRange,
    analysis_range: Option<ByteRange>,
) -> Result<(), FcsError> {
    let mut segments = vec![
        ("HEADER", ByteRange::new(0, 57)),
        ("TEXT", text_range),
        ("DATA", data_range),
    ];

    if let Some(range) = supplemental_text_range {
        segments.push(("supplemental TEXT", range));
    }
    if let Some(range) = analysis_range {
        segments.push(("ANALYSIS", range));
    }

    for (name, range) in &segments {
        ensure_in_file(name, *range, file_len)?;
    }

    segments.sort_by_key(|(_, range)| range.start);
    for window in segments.windows(2) {
        let (first_name, first_range) = window[0];
        let (second_name, second_range) = window[1];
        if second_range.start <= first_range.end {
            return Err(FcsError::SegmentOverlap {
                first: first_name,
                second: second_name,
            });
        }
    }

    Ok(())
}

fn ensure_in_file(name: &'static str, range: ByteRange, file_len: u64) -> Result<(), FcsError> {
    if range.end >= file_len {
        Err(FcsError::SegmentOutOfBounds {
            name,
            start: range.start,
            end: range.end,
            file_len,
        })
    } else {
        Ok(())
    }
}

fn ensure_within_limit(what: &'static str, value: u64, limit: u64) -> Result<(), FcsError> {
    if value > limit {
        Err(FcsError::LimitExceeded { what, value, limit })
    } else {
        Ok(())
    }
}
