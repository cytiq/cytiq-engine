//! Inputs and the alternative layout for the events benchmark
//! (`performance/events.py`) and the lakehouse interop check
//! (`parity/lakehouse/interop.py`). A development tool, not a cytiq
//! subcommand; the list layout it writes is never published by cytiq.
//!
//! - `generate`: a deterministic FCS 3.2 file of `--events` events, written
//!   row by row (memory does not grow with the event count).
//! - `write-list`: one FCS file as the **list layout**: the three keys, one
//!   fixed-size list column per value type (`values_u64`, `values_f32`,
//!   `values_f64`, nothing widened) and a separate channel dictionary table
//!   (`<output>.channels.parquet`) mapping each channel to its list and
//!   position.
//! - `verify`: `cytiq_population::events::verify_file`, timed on its own.

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    ArrayRef, FixedSizeListArray, Float32Array, Float64Array, RecordBatch, StringArray,
    UInt16Array, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use clap::{Parser, Subcommand, ValueEnum};
use cytiq_fcs::{ColumnData, FcsReadLimits, FcsReader};
use cytiq_population::events;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Generate {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        events: u64,
        #[arg(long, default_value_t = 30)]
        channels: usize,
        #[arg(long, value_enum, default_value_t = Types::Float32)]
        types: Types,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Start every channel with -0.0, NaN payloads, infinities,
        /// subnormals and integers above 2^63 (as the type allows).
        #[arg(long)]
        special: bool,
    },
    WriteList {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = events::DEFAULT_ROW_GROUP_ROWS)]
        row_group_size: usize,
        #[arg(long, default_value_t = events::DEFAULT_ZSTD_LEVEL)]
        zstd_level: i32,
    },
    Verify {
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Types {
    /// Every channel `F` (32-bit float).
    Float32,
    /// Every channel `D` (64-bit float).
    Float64,
    /// `Time` as a 64-bit integer, then `F` channels and three `D` channels.
    Mixed,
}

/// One parameter of a generated file: `$PnN`, `$PnDATATYPE`, `$PnB`.
struct Parameter {
    name: String,
    datatype: char,
    bits: u32,
}

fn parameters(types: Types, channels: usize) -> Vec<Parameter> {
    let parameter = |name: String, datatype: char, bits: u32| Parameter {
        name,
        datatype,
        bits,
    };
    match types {
        Types::Float32 => (1..=channels)
            .map(|i| parameter(format!("C{i}"), 'F', 32))
            .collect(),
        Types::Float64 => (1..=channels)
            .map(|i| parameter(format!("C{i}"), 'D', 64))
            .collect(),
        Types::Mixed => {
            let mut list = vec![parameter("Time".into(), 'I', 64)];
            let doubles = 3.min(channels.saturating_sub(1));
            for i in 2..=channels {
                let datatype = if i > channels - doubles { 'D' } else { 'F' };
                let bits = if datatype == 'D' { 64 } else { 32 };
                list.push(parameter(format!("C{i}"), datatype, bits));
            }
            list
        }
    }
}

/// `SplitMix64`.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A cytometry-like intensity: a few populations, a rare bright one,
    /// negative values after compensation-like spread.
    #[allow(clippy::cast_precision_loss)]
    fn intensity(&mut self, channel: usize) -> f64 {
        let draw = self.next();
        let unit = (draw >> 11) as f64 / (1_u64 << 53) as f64;
        let population = draw % 100;
        let centre = match population {
            0 => 50_000.0,
            1..=40 => 300.0 + 40.0 * channel as f64,
            41..=80 => 2_000.0,
            _ => 9_000.0,
        };
        let spread = centre * 0.25;
        centre + spread * (unit * 2.0 - 1.0) * (unit * 3.0 + 0.5) - 150.0
    }
}

const SPECIAL_F32: [u32; 6] = [
    0x8000_0000, // -0.0
    0x7fc0_1234, // NaN with a payload
    0xff80_0001, // signalling NaN, sign set
    0x7f80_0000, // +inf
    0x0000_0001, // smallest subnormal
    0x7f7f_ffff, // f32::MAX
];
const SPECIAL_F64: [u64; 6] = [
    0x8000_0000_0000_0000,
    0x7ff8_0000_dead_beef,
    0xfff0_0000_0000_0001,
    0xfff0_0000_0000_0000,
    0x0000_0000_0000_0001,
    0x7fef_ffff_ffff_ffff,
];
const SPECIAL_U64: [u64; 6] = [0, 1, (1 << 53) | 1, 1 << 63, u64::MAX - 1, u64::MAX];

fn write_value(
    out: &mut impl Write,
    parameter: &Parameter,
    channel: usize,
    event: u64,
    random: &mut Random,
    special: bool,
) -> std::io::Result<()> {
    let row = usize::try_from(event).unwrap_or(usize::MAX);
    let special = special && row < SPECIAL_F32.len();
    match parameter.datatype {
        'I' => {
            let value = if special { SPECIAL_U64[row] } else { event / 4 };
            out.write_all(&value.to_le_bytes())
        }
        'D' => {
            let value = if special {
                f64::from_bits(SPECIAL_F64[row])
            } else {
                random.intensity(channel)
            };
            out.write_all(&value.to_le_bytes())
        }
        _ => {
            #[allow(clippy::cast_possible_truncation)]
            let value = if special {
                f32::from_bits(SPECIAL_F32[row])
            } else {
                random.intensity(channel) as f32
            };
            out.write_all(&value.to_le_bytes())
        }
    }
}

/// TEXT with 20-digit DATA offsets, so its length does not depend on them.
fn text(parameters: &[Parameter], events: u64, begin: u64, end: u64) -> String {
    let datatype = parameters[0].datatype;
    let mut pairs = vec![
        ("$BEGINANALYSIS".to_owned(), "0".to_owned()),
        ("$ENDANALYSIS".to_owned(), "0".to_owned()),
        ("$BEGINSTEXT".to_owned(), "0".to_owned()),
        ("$ENDSTEXT".to_owned(), "0".to_owned()),
        ("$BEGINDATA".to_owned(), format!("{begin:020}")),
        ("$ENDDATA".to_owned(), format!("{end:020}")),
        ("$BYTEORD".to_owned(), "1,2,3,4".to_owned()),
        ("$DATATYPE".to_owned(), datatype.to_string()),
        ("$MODE".to_owned(), "L".to_owned()),
        ("$NEXTDATA".to_owned(), "0".to_owned()),
        ("$PAR".to_owned(), parameters.len().to_string()),
        ("$TOT".to_owned(), events.to_string()),
    ];
    for (i, parameter) in parameters.iter().enumerate() {
        let n = i + 1;
        pairs.push((format!("$P{n}B"), parameter.bits.to_string()));
        if parameter.datatype != datatype {
            pairs.push((format!("$P{n}DATATYPE"), parameter.datatype.to_string()));
        }
        pairs.push((format!("$P{n}E"), "0,0".to_owned()));
        pairs.push((format!("$P{n}N"), parameter.name.clone()));
        let range = if parameter.datatype == 'I' {
            u64::MAX.to_string()
        } else {
            "262144".to_owned()
        };
        pairs.push((format!("$P{n}R"), range));
    }
    let mut text = String::from("|");
    for (key, value) in pairs {
        text.push_str(&key);
        text.push('|');
        text.push_str(&value);
        text.push('|');
    }
    text
}

fn generate(
    output: &Path,
    events: u64,
    channels: usize,
    types: Types,
    seed: u64,
    special: bool,
) -> Result<()> {
    if events == 0 || channels == 0 {
        return Err("a generated file needs events and channels".into());
    }
    let parameters = parameters(types, channels);
    let row_bytes: u64 = parameters.iter().map(|p| u64::from(p.bits / 8)).sum();
    let data_bytes = events * row_bytes;
    let text_length = text(&parameters, events, 0, 0).len() as u64;
    let begin = 58 + text_length;
    let end = begin + data_bytes - 1;
    let text = text(&parameters, events, begin, end);
    // Offsets past eight digits are recorded in TEXT only.
    let header_data = |value: u64| if end <= 99_999_999 { value } else { 0 };
    let mut out = BufWriter::with_capacity(1 << 20, File::create(output)?);
    write!(
        out,
        "FCS3.2    {:>8}{:>8}{:>8}{:>8}{:>8}{:>8}",
        58,
        57 + text_length,
        header_data(begin),
        header_data(end),
        0,
        0
    )?;
    out.write_all(text.as_bytes())?;
    let mut random = Random(seed);
    for event in 0..events {
        for (channel, parameter) in parameters.iter().enumerate() {
            write_value(&mut out, parameter, channel, event, &mut random, special)?;
        }
    }
    out.flush()?;
    println!(
        "{{\"events\":{events},\"channels\":{},\"data_bytes\":{data_bytes}}}",
        parameters.len()
    );
    Ok(())
}

/// Interleave equal-length columns into one row-major fixed-size list.
fn list<T: Copy>(columns: &[&[T]], rows: usize) -> Vec<T> {
    let mut values = Vec::with_capacity(rows * columns.len());
    for row in 0..rows {
        for column in columns {
            values.push(column[row]);
        }
    }
    values
}

fn fixed_list(field: &str, item: DataType, values: ArrayRef, width: usize) -> Result<ArrayRef> {
    let size = i32::try_from(width)?;
    Ok(Arc::new(FixedSizeListArray::try_new(
        Arc::new(Field::new(field, item, false)),
        size,
        values,
        None,
    )?))
}

/// The keys, then one fixed-size list per value type present.
fn list_schema(widths: [usize; 3]) -> Result<Arc<Schema>> {
    let mut fields = vec![
        Field::new("acquisition_id", DataType::Utf8, false),
        Field::new("layer_id", DataType::Utf8, false),
        Field::new("source_event_id", DataType::UInt64, false),
    ];
    let lists = [
        ("values_u64", DataType::UInt64),
        ("values_f32", DataType::Float32),
        ("values_f64", DataType::Float64),
    ];
    for (kind, (name, item)) in lists.iter().enumerate() {
        if widths[kind] > 0 {
            let size = i32::try_from(widths[kind])?;
            fields.push(Field::new(
                *name,
                DataType::FixedSizeList(Arc::new(Field::new("item", item.clone(), false)), size),
                false,
            ));
        }
    }
    Ok(Arc::new(Schema::new(fields)))
}

fn write_list(input: &Path, output: &Path, row_group_size: usize, zstd_level: i32) -> Result<()> {
    let probe = FcsReader::open(input)?.stream(NonZeroUsize::MIN, FcsReadLimits::default())?;
    let types: Vec<u8> = probe
        .batches
        .empty_batch()
        .columns
        .iter()
        .map(|column| match column {
            ColumnData::UInt(_) => 0,
            ColumnData::F32(_) => 1,
            ColumnData::F64(_) => 2,
        })
        .collect();
    let names: Vec<String> = probe
        .metadata
        .parameters
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let acquisition = cytiq_fcs::acquisition_id(input, &probe.metadata.text)?;
    let mut widths = [0_usize; 3];
    for kind in &types {
        widths[usize::from(*kind)] += 1;
    }
    let row_bytes = widths[0] * 8 + widths[1] * 4 + widths[2] * 8 + 8 + 2 * 68;
    let rows = (events::DEFAULT_BATCH_BYTES / row_bytes).clamp(1, 65_536);

    let schema = list_schema(widths)?;
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(zstd_level)?))
        .set_max_row_group_row_count(Some(row_group_size))
        .set_max_row_group_bytes(Some(events::ROW_GROUP_BYTES_MAX))
        .set_dictionary_enabled(false)
        .set_column_dictionary_enabled(ColumnPath::from("acquisition_id"), true)
        .set_column_dictionary_enabled(ColumnPath::from("layer_id"), true)
        .build();
    let mut writer = ArrowWriter::try_new(File::create(output)?, schema.clone(), Some(properties))?;
    // The list layout has no layer descriptor; a fixed id stands in.
    let layer = "list-layout-benchmark";
    let stream = FcsReader::open(input)?.stream(
        NonZeroUsize::new(rows).unwrap_or(NonZeroUsize::MIN),
        FcsReadLimits::default(),
    )?;
    let mut start = 0_u64;
    for batch in stream.batches {
        let batch = batch?;
        let count = batch.columns.first().map_or(0, ColumnData::len);
        let mut u64s: Vec<&[u64]> = Vec::new();
        let mut f32s: Vec<&[f32]> = Vec::new();
        let mut f64s: Vec<&[f64]> = Vec::new();
        for column in &batch.columns {
            match column {
                ColumnData::UInt(values) => u64s.push(values),
                ColumnData::F32(values) => f32s.push(values),
                ColumnData::F64(values) => f64s.push(values),
            }
        }
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                acquisition.as_str(),
                count,
            ))),
            Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                layer, count,
            ))),
            Arc::new(UInt64Array::from_iter_values(start..start + count as u64)),
        ];
        if !u64s.is_empty() {
            let values = Arc::new(UInt64Array::from(list(&u64s, count)));
            arrays.push(fixed_list("item", DataType::UInt64, values, u64s.len())?);
        }
        if !f32s.is_empty() {
            let values = Arc::new(Float32Array::from(list(&f32s, count)));
            arrays.push(fixed_list("item", DataType::Float32, values, f32s.len())?);
        }
        if !f64s.is_empty() {
            let values = Arc::new(Float64Array::from(list(&f64s, count)));
            arrays.push(fixed_list("item", DataType::Float64, values, f64s.len())?);
        }
        writer.write(&RecordBatch::try_new(schema.clone(), arrays)?)?;
        start += count as u64;
    }
    writer.close()?;
    write_dictionary(output, &names, &types)?;
    Ok(())
}

/// `(index, name, value_type, list, position)` per channel.
fn write_dictionary(output: &Path, names: &[String], types: &[u8]) -> Result<()> {
    let mut positions = [0_u16; 3];
    let mut list_names = Vec::new();
    let mut value_types = Vec::new();
    let mut position = Vec::new();
    for kind in types {
        let kind = usize::from(*kind);
        list_names.push(["values_u64", "values_f32", "values_f64"][kind]);
        value_types.push(["uint64", "float32", "float64"][kind]);
        // DuckDB and SQL lists count from 1.
        positions[kind] += 1;
        position.push(positions[kind]);
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("index", DataType::UInt16, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("value_type", DataType::Utf8, false),
        Field::new("list", DataType::Utf8, false),
        Field::new("position", DataType::UInt16, false),
    ]));
    let index: Vec<u16> = (1..=u16::try_from(names.len())?).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt16Array::from(index)),
            Arc::new(StringArray::from_iter_values(names)),
            Arc::new(StringArray::from(value_types)),
            Arc::new(StringArray::from(list_names)),
            Arc::new(UInt16Array::from(position)),
        ],
    )?;
    let path = PathBuf::from(format!("{}.channels.parquet", output.display()));
    let mut writer = ArrowWriter::try_new(File::create(path)?, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Generate {
            output,
            events,
            channels,
            types,
            seed,
            special,
        } => generate(&output, events, channels, types, seed, special),
        Command::WriteList {
            input,
            output,
            row_group_size,
            zstd_level,
        } => write_list(&input, &output, row_group_size, zstd_level),
        Command::Verify { input } => {
            let mut issues = cytiq_population::issue::Issues::new();
            let summary = events::verify_file(&input, "", &mut issues)
                .ok_or_else(|| format!("{:?}", issues.items()))?;
            println!(
                "{{\"rows\":{},\"values_sha256\":\"{}\"}}",
                summary.rows, summary.values_sha256
            );
            Ok(())
        }
    }
}
