//! Physical settings of an events export, and the plan they give for one
//! layer: stored channels, batch, row group and part sizes. The plan is
//! checked against what the bounded verifier reads before anything is
//! written, so an export never publishes a table it could not verify.

use std::num::{NonZeroU64, NonZeroUsize};

use cytiq_mask::parquet::ParquetReadLimits;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::DEFAULT_DATA_PAGE_ROW_COUNT_LIMIT;

use super::{LayerDescriptor, PARTS_MAX};
use crate::{PopulationError, Result, require};

/// Decoded bytes of a batch (values, `source_event_id` and both keys) when
/// no batch size is given.
pub const DEFAULT_BATCH_BYTES: usize = 16 << 20;
/// Largest batch accepted, whatever the batch size asked for.
pub const BATCH_BYTES_MAX: usize = 256 << 20;
/// Most events per batch when batches are sized by bytes.
const BATCH_ROWS_MAX: usize = 65_536;
/// Events per row group unless set otherwise. Chosen by
/// `performance/events.py`: within 10% of the fastest `DuckDB` projection and
/// scan at 1M and 10M events, with the lowest peak memory.
pub const DEFAULT_ROW_GROUP_ROWS: usize = 131_072;
/// Encoded bytes a row group may buffer before it is flushed, whatever its
/// row count: the encoder's memory bound.
pub const ROW_GROUP_BYTES_MAX: usize = 64 << 20;
/// Zstandard level unless set otherwise. Higher levels gained under 1% in
/// size on cytometry floats and need larger per-column contexts.
pub const DEFAULT_ZSTD_LEVEL: i32 = 1;
/// Decoded bytes of both key strings per row: 64 characters and an offset.
pub(crate) const KEY_BYTES: usize = 2 * (64 + 4);
/// Upper bound of the footer bytes per column chunk (offsets, sizes,
/// encodings, statistics of at most 64-byte keys).
const FOOTER_BYTES_PER_CHUNK: u64 = 256;

/// Compression of every column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Zstandard at a level from 1 to 22.
    Zstd(i32),
    Snappy,
}

impl Default for Codec {
    fn default() -> Self {
        Self::Zstd(DEFAULT_ZSTD_LEVEL)
    }
}

impl Codec {
    pub(crate) fn parquet(self) -> Result<Compression> {
        match self {
            Self::Zstd(level) => (1..=22)
                .contains(&level)
                .then(|| ZstdLevel::try_new(level).ok())
                .flatten()
                .map(Compression::ZSTD)
                .ok_or_else(|| invalid(format!("zstd level {level} is not between 1 and 22"))),
            Self::Snappy => Ok(Compression::SNAPPY),
        }
    }
}

/// How an export is written. None of these settings changes a value: the
/// values digest is the same whatever they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteSettings {
    /// Events decoded per batch; `None` sizes batches to
    /// [`DEFAULT_BATCH_BYTES`]. A batch may not exceed [`BATCH_BYTES_MAX`].
    pub batch_rows: Option<NonZeroUsize>,
    /// Most events per row group; a row group is also flushed at
    /// [`ROW_GROUP_BYTES_MAX`] encoded bytes.
    pub row_group_rows: NonZeroUsize,
    pub codec: Codec,
    /// Split the layer into part tables holding at most this many bytes of
    /// uncompressed values (`source_event_id` and channels) each. Part
    /// boundaries depend only on the channel types, not on the codec.
    pub max_file_bytes: Option<NonZeroU64>,
    /// Store only these channels, in `$Pn` order whatever their order here.
    pub channels: Option<Vec<String>>,
}

impl Default for WriteSettings {
    fn default() -> Self {
        Self {
            batch_rows: None,
            row_group_rows: NonZeroUsize::new(DEFAULT_ROW_GROUP_ROWS).unwrap_or(NonZeroUsize::MIN),
            codec: Codec::default(),
            max_file_bytes: None,
            channels: None,
        }
    }
}

/// The sizes of one export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub batch_rows: NonZeroUsize,
    pub row_group_rows: NonZeroUsize,
    /// Events per part when the layer is split.
    pub part_rows: Option<NonZeroU64>,
    pub compression: Compression,
}

fn invalid(message: impl Into<String>) -> PopulationError {
    PopulationError::Invalid(message.into())
}

impl WriteSettings {
    /// Positions in `descriptor.channels` of the stored channels, in `$Pn`
    /// order.
    ///
    /// # Errors
    /// Refuses an empty selection, an unknown channel and a channel selected
    /// twice.
    pub(crate) fn select(&self, descriptor: &LayerDescriptor) -> Result<Vec<usize>> {
        let Some(names) = &self.channels else {
            return Ok((0..descriptor.channels.len()).collect());
        };
        require(!names.is_empty(), "the channel selection is empty")?;
        let mut stored = Vec::with_capacity(names.len());
        for name in names {
            let position = descriptor
                .channels
                .iter()
                .position(|channel| channel.name == *name)
                .ok_or_else(|| invalid(format!("channel `{name}` is not a channel of the file")))?;
            require(
                !stored.contains(&position),
                format!("channel `{name}` is selected twice"),
            )?;
            stored.push(position);
        }
        stored.sort_unstable();
        Ok(stored)
    }

    /// The sizes for `events` events of the `stored` channels, checked
    /// against `limits`; `metadata_bytes` estimates the footer's key-value
    /// metadata.
    ///
    /// # Errors
    /// Refuses a batch over [`BATCH_BYTES_MAX`], an invalid codec, parts too
    /// small for one event, and any table the bounded reader would refuse
    /// within `limits`: too many rows, decoded bytes, file bytes, pages or
    /// footer bytes.
    pub(crate) fn plan(
        &self,
        descriptor: &LayerDescriptor,
        stored: &[usize],
        events: u64,
        metadata_bytes: u64,
        limits: &ParquetReadLimits,
    ) -> Result<Plan> {
        let value_bytes = stored
            .iter()
            .map(|&position| descriptor.channels[position].value_type.bytes())
            .sum::<usize>()
            + 8;
        let decoded_row = value_bytes + KEY_BYTES;
        let batch_rows = match self.batch_rows {
            Some(rows) => {
                let bytes = rows.get().saturating_mul(decoded_row);
                require(
                    bytes <= BATCH_BYTES_MAX,
                    format!(
                        "a batch of {rows} events decodes to {bytes} bytes, more than the \
                         {BATCH_BYTES_MAX} allowed"
                    ),
                )?;
                rows
            }
            None => NonZeroUsize::new((DEFAULT_BATCH_BYTES / decoded_row).min(BATCH_ROWS_MAX))
                .unwrap_or(NonZeroUsize::MIN),
        };
        let part_rows = match self.max_file_bytes {
            Some(bytes) => Some(
                NonZeroU64::new(bytes.get() / value_bytes as u64).ok_or_else(|| {
                    invalid(format!(
                        "{bytes} bytes per file hold no event of {value_bytes} bytes"
                    ))
                })?,
            ),
            None => None,
        };
        if let Some(rows) = part_rows {
            let parts = events.div_ceil(rows.get());
            require(
                parts <= PARTS_MAX,
                format!(
                    "{events} events in parts of {rows} make {parts} parts, more than the \
                     {PARTS_MAX} a layer holds; raise `--max-file-bytes`"
                ),
            )?;
        }
        let table_rows = part_rows.map_or(events, |rows| rows.get().min(events));
        check_table(
            TableShape {
                rows: table_rows,
                columns: stored.len() as u64 + 3,
                value_bytes: value_bytes as u64,
                decoded_row: decoded_row as u64,
                row_group_rows: self.row_group_rows.get() as u64,
                metadata_bytes,
            },
            limits,
            part_rows.is_some(),
        )?;
        Ok(Plan {
            batch_rows,
            row_group_rows: self.row_group_rows,
            part_rows,
            compression: self.codec.parquet()?,
        })
    }
}

/// The largest table of an export.
#[derive(Clone, Copy, Debug)]
struct TableShape {
    rows: u64,
    columns: u64,
    value_bytes: u64,
    decoded_row: u64,
    row_group_rows: u64,
    metadata_bytes: u64,
}

impl TableShape {
    /// Fewest rows a row group holds: its row limit, or what fits under the
    /// byte limit when every value is stored uncompressed.
    fn group_rows(self) -> u64 {
        let by_bytes = (ROW_GROUP_BYTES_MAX as u64 / (self.value_bytes + 8)).max(1);
        self.row_group_rows.min(by_bytes).min(self.rows).max(1)
    }

    fn groups(self) -> u64 {
        self.rows.div_ceil(self.group_rows()).max(1)
    }

    fn footer_bytes(self) -> u64 {
        self.groups()
            .saturating_mul(self.columns)
            .saturating_mul(FOOTER_BYTES_PER_CHUNK)
            .saturating_add(self.metadata_bytes)
    }

    /// Data pages per column chunk follow the writer's row limit per page,
    /// plus one for a page cut short; both keys add a dictionary page.
    fn pages(self) -> u64 {
        let per_chunk = self
            .group_rows()
            .div_ceil(DEFAULT_DATA_PAGE_ROW_COUNT_LIMIT as u64)
            + 1;
        self.groups()
            .saturating_mul(self.columns.saturating_mul(per_chunk) + 2)
    }
}

fn check_table(shape: TableShape, limits: &ParquetReadLimits, split: bool) -> Result<()> {
    let split_hint = if split {
        "lower `--max-file-bytes`"
    } else {
        "split the layer with `--max-file-bytes`"
    };
    let rows = shape.rows;
    require(
        rows <= limits.rows,
        format!(
            "a table of {rows} events exceeds the {} rows the verifier reads; {split_hint}",
            limits.rows
        ),
    )?;
    // Decoded arrays carry allocation slack beyond the raw bytes.
    let decoded = rows.saturating_mul(shape.decoded_row);
    let decoded_limit = limits.decoded_bytes - limits.decoded_bytes / 8;
    require(
        decoded <= decoded_limit,
        format!(
            "a table of {rows} events decodes to about {decoded} bytes, more than the \
             {decoded_limit} the verifier reads; {split_hint}"
        ),
    )?;
    let file = rows
        .saturating_mul(shape.value_bytes)
        .saturating_add(shape.footer_bytes());
    require(
        file <= limits.file_bytes,
        format!(
            "a table of {rows} events may take {file} bytes, more than the {} the verifier \
             reads; {split_hint}",
            limits.file_bytes
        ),
    )?;
    let footer = shape.footer_bytes();
    require(
        footer <= limits.metadata_bytes,
        format!(
            "{} row groups of {} columns need about {footer} footer bytes, more than the {} \
             the verifier reads; raise `--row-group-size` or {split_hint}",
            shape.groups(),
            shape.columns,
            limits.metadata_bytes
        ),
    )?;
    let pages = shape.pages();
    require(
        pages <= limits.pages,
        format!(
            "a table of {rows} events in {} columns needs about {pages} pages, more than the {} \
             the verifier reads; {split_hint}",
            shape.columns, limits.pages
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ChannelSpec, LAYER_CONTRACT, LayerState, ValueType};

    fn descriptor(types: &[ValueType]) -> LayerDescriptor {
        LayerDescriptor {
            contract: LAYER_CONTRACT.into(),
            acquisition_id: "a".repeat(64),
            state: LayerState::Raw,
            steps: Vec::new(),
            channels: types
                .iter()
                .enumerate()
                .map(|(i, value_type)| ChannelSpec {
                    index: u16::try_from(i + 1).unwrap(),
                    name: format!("C{}", i + 1),
                    value_type: *value_type,
                })
                .collect(),
        }
    }

    fn plan(settings: &WriteSettings, events: u64, limits: &ParquetReadLimits) -> Result<Plan> {
        let layer = descriptor(&[ValueType::Float32, ValueType::Uint64, ValueType::Float64]);
        let stored = settings.select(&layer)?;
        settings.plan(&layer, &stored, events, 4096, limits)
    }

    #[test]
    fn selections_are_stored_in_pn_order() {
        let layer = descriptor(&[ValueType::Float32; 4]);
        let settings = WriteSettings {
            channels: Some(vec!["C4".into(), "C2".into()]),
            ..WriteSettings::default()
        };
        assert_eq!(settings.select(&layer).unwrap(), [1, 3]);
        for names in [vec![], vec!["C9".into()], vec!["C1".into(), "C1".into()]] {
            let settings = WriteSettings {
                channels: Some(names),
                ..WriteSettings::default()
            };
            assert!(settings.select(&layer).is_err());
        }
    }

    #[test]
    fn parts_hold_whole_events_of_uncompressed_values() {
        // 8 (id) + 4 + 8 + 8 = 28 bytes a row.
        let settings = WriteSettings {
            max_file_bytes: NonZeroU64::new(280),
            ..WriteSettings::default()
        };
        let plan = plan(&settings, 1000, &ParquetReadLimits::default()).unwrap();
        assert_eq!(plan.part_rows, NonZeroU64::new(10));
        let settings = WriteSettings {
            max_file_bytes: NonZeroU64::new(27),
            ..WriteSettings::default()
        };
        assert!(plan_err(&settings, 1000).contains("hold no event"));
    }

    fn plan_err(settings: &WriteSettings, events: u64) -> String {
        plan(settings, events, &ParquetReadLimits::default())
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn batches_are_bounded_by_bytes() {
        let plan_default =
            plan(&WriteSettings::default(), 10, &ParquetReadLimits::default()).unwrap();
        assert_eq!(plan_default.batch_rows.get(), BATCH_ROWS_MAX);
        let settings = WriteSettings {
            batch_rows: NonZeroUsize::new(BATCH_BYTES_MAX),
            ..WriteSettings::default()
        };
        assert!(plan_err(&settings, 10).contains("more than the"));
    }

    #[test]
    fn tables_the_verifier_would_refuse_are_refused_first() {
        let tight = ParquetReadLimits {
            rows: 100,
            ..ParquetReadLimits::default()
        };
        let settings = WriteSettings::default();
        assert!(plan(&settings, 100, &tight).is_ok());
        let error = plan(&settings, 101, &tight).unwrap_err().to_string();
        assert!(error.contains("--max-file-bytes"), "{error}");
        let split = WriteSettings {
            max_file_bytes: NonZeroU64::new(28 * 100),
            ..WriteSettings::default()
        };
        assert!(plan(&split, 1_000_000, &tight).is_ok());

        let decoded = ParquetReadLimits {
            decoded_bytes: 8 * 164 * 1000,
            ..ParquetReadLimits::default()
        };
        // 28 value bytes and 136 key bytes a row, within 7/8 of the limit.
        assert!(plan(&settings, 7000, &decoded).is_ok());
        assert!(plan(&settings, 7001, &decoded).is_err());

        let footer = ParquetReadLimits {
            metadata_bytes: 4096 + 6 * 256,
            ..ParquetReadLimits::default()
        };
        let small_groups = WriteSettings {
            row_group_rows: NonZeroUsize::new(10).unwrap(),
            ..WriteSettings::default()
        };
        assert!(plan(&small_groups, 10, &footer).is_ok());
        let error = plan(&small_groups, 11, &footer).unwrap_err().to_string();
        assert!(error.contains("--row-group-size"), "{error}");

        let pages = ParquetReadLimits {
            pages: 8,
            ..ParquetReadLimits::default()
        };
        // One group: 6 columns of 2 pages each, plus 2 dictionary pages.
        assert!(plan(&settings, 10, &pages).is_err());
    }

    #[test]
    fn codecs_are_checked() {
        for (codec, ok) in [
            (Codec::Zstd(1), true),
            (Codec::Zstd(22), true),
            (Codec::Zstd(0), false),
            (Codec::Zstd(23), false),
            (Codec::Snappy, true),
        ] {
            assert_eq!(codec.parquet().is_ok(), ok, "{codec:?}");
        }
    }
}
