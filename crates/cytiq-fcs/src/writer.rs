use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use log::debug;

use crate::error::FcsError;
use crate::types::{FcsText, Parameter};

const HEADER_LEN: u64 = 58;
const TEXT_DELIMITER: u8 = b'|';
const OFFSET_FIELD_LIMIT: u64 = 99_999_999;
/// `$BEGINDATA`/`$ENDDATA` are zero-padded to the widest u64 so the TEXT length
/// does not depend on the offsets it describes. The HEADER repeats the DATA
/// offsets when they fit its 8-digit fields, as FCS 3.1 requires (strict
/// readers such as flowio reject a mismatch), and holds zeros otherwise.
const TEXT_OFFSET_WIDTH: usize = 20;
/// Per-parameter keywords the writer derives from the data itself.
const GENERATED_PARAMETER_SUFFIXES: [&str; 6] = ["N", "S", "B", "E", "R", "DATATYPE"];

#[derive(Clone, Debug, PartialEq)]
pub struct FcsWriteParameter {
    pub name: String,
    pub label: Option<String>,
    /// `$PnR` override; computed from the column maximum when absent.
    pub range: Option<f64>,
    /// Extra `$Pn<suffix>` keywords as `(suffix, value)`, e.g. `("V", "450")`.
    pub keywords: Vec<(String, String)>,
}

impl FcsWriteParameter {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: None,
            range: None,
            keywords: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_label(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            ..Self::new(name)
        }
    }

    /// Describe a source parameter, keeping its name, label, `$PnR` and every
    /// non-generated `$Pn*` keyword (`$PnV`, `$PnG`, `$PnD`, ...).
    #[must_use]
    pub fn from_source(parameter: &Parameter) -> Self {
        let prefix = format!("$P{}", parameter.index);
        let range_key = format!("{prefix}R");
        let range = parameter
            .keywords
            .iter()
            .find(|(key, _)| *key == range_key)
            .and_then(|(_, value)| value.trim().parse::<f64>().ok());
        let keywords = parameter
            .keywords
            .iter()
            .filter_map(|(key, value)| {
                let suffix = key.strip_prefix(&prefix)?;
                (!GENERATED_PARAMETER_SUFFIXES.contains(&suffix))
                    .then(|| (suffix.to_owned(), value.clone()))
            })
            .collect();
        Self {
            name: parameter.name.clone(),
            label: parameter.label.clone(),
            range,
            keywords,
        }
    }

    #[must_use]
    pub const fn with_range(mut self, range: f64) -> Self {
        self.range = Some(range);
        self
    }

    #[must_use]
    pub fn with_keyword(mut self, suffix: impl Into<String>, value: impl Into<String>) -> Self {
        let suffix = suffix.into();
        if let Some((_, existing)) = self.keywords.iter_mut().find(|(key, _)| *key == suffix) {
            *existing = value.into();
        } else {
            self.keywords.push((suffix, value.into()));
        }
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FcsWriteOptions {
    pub text_keywords: Vec<(String, String)>,
}

impl FcsWriteOptions {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text_keywords: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_text_keyword(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.text_keywords.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn with_text_keywords(
        mut self,
        keywords: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.text_keywords.extend(keywords);
        self
    }
}

/// Dataset-level keywords of `text` that a derived file can carry over:
/// everything except segment offsets, layout keywords and `$Pn*` parameter
/// keywords, which the writer regenerates. Callers drop keywords that no
/// longer describe the written values (for example `$SPILLOVER` after a
/// transform).
#[must_use]
pub fn carried_keywords(text: &FcsText) -> Vec<(String, String)> {
    text.iter()
        .filter(|(key, _)| !is_generated_keyword(key.trim()))
        .map(|(key, value)| (key.trim().to_owned(), value.to_owned()))
        .collect()
}

/// Event value encodings the writer supports.
trait DataValue: Copy {
    const DATATYPE: &'static str;
    const BITS: &'static str;
    const WIDTH: u64;

    fn is_finite(self) -> bool;
    fn as_f64(self) -> f64;
    fn write_le<W: Write>(self, writer: &mut W) -> std::io::Result<()>;
}

impl DataValue for f32 {
    const DATATYPE: &'static str = "F";
    const BITS: &'static str = "32";
    const WIDTH: u64 = 4;

    fn is_finite(self) -> bool {
        self.is_finite()
    }

    fn as_f64(self) -> f64 {
        f64::from(self)
    }

    fn write_le<W: Write>(self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&self.to_le_bytes())
    }
}

impl DataValue for f64 {
    const DATATYPE: &'static str = "D";
    const BITS: &'static str = "64";
    const WIDTH: u64 = 8;

    fn is_finite(self) -> bool {
        self.is_finite()
    }

    fn as_f64(self) -> f64 {
        self
    }

    fn write_le<W: Write>(self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&self.to_le_bytes())
    }
}

/// Write an FCS 3.1 file with little-endian 32-bit float event data.
///
/// `columns` must contain one column per parameter and all columns must have the
/// same number of events. The writer intentionally supports only the compact
/// layout needed for generated cytiq outputs.
///
/// # Errors
///
/// Returns an error when parameters/columns are inconsistent, offsets cannot be
/// represented in the fixed header, values are non-finite, or the underlying
/// writer fails.
pub fn write_fcs3_1_f32(
    path: impl AsRef<Path>,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f32>],
) -> Result<(), FcsError> {
    write_fcs3_1_f32_with_options(path, parameters, columns, &FcsWriteOptions::new())
}

/// Write an FCS 3.1 file with little-endian 32-bit float event data and custom
/// TEXT keywords.
///
/// # Errors
///
/// Returns an error when parameters/columns/options are inconsistent, offsets
/// cannot be represented in the fixed header, values are non-finite, or the
/// underlying writer fails.
pub fn write_fcs3_1_f32_with_options(
    path: impl AsRef<Path>,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f32>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    write_to_file(path.as_ref(), parameters, columns, options)
}

/// Write an FCS 3.1 file with little-endian 64-bit float event data and custom
/// TEXT keywords. Use it when values must stay exact beyond f32 precision, such
/// as event identifiers above 2^24.
///
/// # Errors
///
/// Returns an error when parameters/columns/options are inconsistent, offsets
/// cannot be represented in the fixed header, values are non-finite, or the
/// underlying writer fails.
pub fn write_fcs3_1_f64_with_options(
    path: impl AsRef<Path>,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f64>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    write_to_file(path.as_ref(), parameters, columns, options)
}

/// Whether storing any of `values` as f32 would change it.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn needs_f64(values: &[f64]) -> bool {
    values
        .iter()
        .any(|value| f64::from(*value as f32).to_bits() != value.to_bits())
}

/// Write an FCS 3.1 file that stores every value exactly: 32-bit floats when
/// all values fit f32, doubles otherwise (event identifiers above 2^24, 32-bit
/// integer data, f64 data). Computed values that should not force doubles
/// must be rounded through f32 by the caller.
///
/// # Errors
///
/// Returns an error when parameters/columns/options are inconsistent, offsets
/// cannot be represented in the fixed header, values are non-finite, or the
/// underlying writer fails.
#[allow(clippy::cast_possible_truncation)]
pub fn write_fcs3_1_exact_with_options(
    path: impl AsRef<Path>,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f64>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    if columns.iter().any(|column| needs_f64(column)) {
        write_fcs3_1_f64_with_options(path, parameters, columns, options)
    } else {
        let columns = columns
            .iter()
            .map(|column| column.iter().map(|value| *value as f32).collect())
            .collect::<Vec<Vec<f32>>>();
        write_fcs3_1_f32_with_options(path, parameters, &columns, options)
    }
}

/// Buffered file output: the data segment is written value by value, which
/// would otherwise cost one `write` syscall per event and channel.
fn write_to_file<T: DataValue>(
    path: &Path,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<T>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    debug!(
        "writing FCS 3.1 {}-bit float dataset to {} with {} parameters",
        T::BITS,
        path.display(),
        parameters.len()
    );
    let mut writer = BufWriter::new(File::create(path).map_err(FcsError::Io)?);
    write_dataset(&mut writer, parameters, columns, options)?;
    writer.flush().map_err(FcsError::Io)
}

/// Write an FCS 3.1 float32 dataset to any byte writer.
///
/// # Errors
///
/// Returns an error when parameters/columns are inconsistent, offsets cannot be
/// represented in the fixed header, values are non-finite, or the underlying
/// writer fails.
pub fn write_fcs3_1_f32_to_writer<W: Write>(
    writer: &mut W,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f32>],
) -> Result<(), FcsError> {
    write_dataset(writer, parameters, columns, &FcsWriteOptions::new())
}

/// Write an FCS 3.1 float32 dataset to any byte writer with custom TEXT
/// keywords.
///
/// # Errors
///
/// Returns an error when parameters/columns/options are inconsistent, offsets
/// cannot be represented in the fixed header, values are non-finite, or the
/// underlying writer fails.
pub fn write_fcs3_1_f32_to_writer_with_options<W: Write>(
    writer: &mut W,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f32>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    write_dataset(writer, parameters, columns, options)
}

/// Write an FCS 3.1 float64 dataset to any byte writer with custom TEXT
/// keywords.
///
/// # Errors
///
/// Returns an error when parameters/columns/options are inconsistent, offsets
/// cannot be represented in the fixed header, values are non-finite, or the
/// underlying writer fails.
pub fn write_fcs3_1_f64_to_writer_with_options<W: Write>(
    writer: &mut W,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<f64>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    write_dataset(writer, parameters, columns, options)
}

fn write_dataset<W: Write, T: DataValue>(
    writer: &mut W,
    parameters: &[FcsWriteParameter],
    columns: &[Vec<T>],
    options: &FcsWriteOptions,
) -> Result<(), FcsError> {
    validate_input(parameters, columns)?;
    let row_count = columns.first().map_or(0, Vec::len);
    let ranges = columns
        .iter()
        .map(|column| column_range(column))
        .collect::<Vec<_>>();
    let (header, text) = encode_header_and_text::<T>(parameters, &ranges, row_count, options)?;

    writer.write_all(header.as_bytes()).map_err(FcsError::Io)?;
    writer.write_all(&text).map_err(FcsError::Io)?;
    write_data(writer, columns, row_count)?;
    Ok(())
}

/// Build the HEADER and TEXT segments. Separate from the data so offsets for
/// very large datasets can be checked without allocating them.
fn encode_header_and_text<T: DataValue>(
    parameters: &[FcsWriteParameter],
    computed_ranges: &[f64],
    row_count: usize,
    options: &FcsWriteOptions,
) -> Result<(String, Vec<u8>), FcsError> {
    validate_parameters(parameters)?;
    validate_options(options)?;

    let mut pairs = common_text_pairs::<T>(parameters.len(), row_count);
    for (parameter_index, parameter) in parameters.iter().enumerate() {
        let number = parameter_index + 1;
        set_pair(&mut pairs, &format!("$P{number}N"), parameter.name.trim());
        if let Some(label) = parameter
            .label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            set_pair(&mut pairs, &format!("$P{number}S"), label);
        }
        set_pair(&mut pairs, &format!("$P{number}B"), T::BITS);
        set_pair(&mut pairs, &format!("$P{number}E"), "0,0");
        let range = match parameter.range {
            Some(range) => range.to_string(),
            None => format!("{:.6}", computed_ranges[parameter_index]),
        };
        set_pair(&mut pairs, &format!("$P{number}R"), &range);
        for (suffix, value) in &parameter.keywords {
            set_pair(&mut pairs, &format!("$P{number}{}", suffix.trim()), value);
        }
    }
    for (key, value) in &options.text_keywords {
        set_pair(&mut pairs, key.trim(), value);
    }
    validate_text_pairs(&pairs)?;

    let text_len = u64::try_from(encode_text(&pairs).len())
        .map_err(|_| FcsError::InvalidWriterInput("TEXT segment length exceeds supported range"))?;
    let text_start = HEADER_LEN;
    let text_end = text_start + text_len - 1;
    if text_end > OFFSET_FIELD_LIMIT {
        return Err(FcsError::InvalidWriterInput(
            "TEXT offsets exceed fixed FCS HEADER fields",
        ));
    }
    let data_start = text_end + 1;
    let data_end = data_start
        .checked_add(data_length::<T>(parameters.len(), row_count)?)
        .map(|end| end - 1)
        .ok_or(FcsError::InvalidWriterInput(
            "DATA segment length exceeds supported range",
        ))?;

    set_pair(&mut pairs, "$BEGINDATA", &format_text_offset(data_start));
    set_pair(&mut pairs, "$ENDDATA", &format_text_offset(data_end));
    let text = encode_text(&pairs);
    debug_assert_eq!(text.len() as u64, text_len, "fixed-width offsets");

    Ok((
        format_header(text_start, text_end, data_start, data_end),
        text,
    ))
}

fn validate_input<T: DataValue>(
    parameters: &[FcsWriteParameter],
    columns: &[Vec<T>],
) -> Result<(), FcsError> {
    if parameters.len() != columns.len() {
        return Err(FcsError::InvalidWriterInput(
            "parameter count must match column count",
        ));
    }
    let Some(first_column) = columns.first() else {
        return Err(FcsError::InvalidWriterInput(
            "at least one column is required",
        ));
    };
    let row_count = first_column.len();
    if row_count == 0 {
        return Err(FcsError::InvalidWriterInput(
            "at least one event row is required",
        ));
    }
    if columns.iter().any(|column| column.len() != row_count) {
        return Err(FcsError::InvalidWriterInput(
            "all columns must have the same length",
        ));
    }
    if columns
        .iter()
        .flat_map(|column| column.iter())
        .any(|value| !value.is_finite())
    {
        return Err(FcsError::InvalidWriterInput(
            "all FCS event values must be finite",
        ));
    }

    Ok(())
}

fn validate_parameters(parameters: &[FcsWriteParameter]) -> Result<(), FcsError> {
    if parameters.is_empty() {
        return Err(FcsError::InvalidWriterInput(
            "at least one parameter is required",
        ));
    }
    for parameter in parameters {
        if parameter.name.trim().is_empty() {
            return Err(FcsError::InvalidWriterInput(
                "parameter names must not be empty",
            ));
        }
        if parameter
            .range
            .is_some_and(|range| !range.is_finite() || range <= 0.0)
        {
            return Err(FcsError::InvalidWriterInput(
                "parameter ranges must be finite and positive",
            ));
        }
        for (suffix, _) in &parameter.keywords {
            let suffix = suffix.trim();
            if suffix.is_empty() || suffix.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(FcsError::InvalidWriterInput(
                    "parameter keyword suffixes must be non-empty and not start with a digit",
                ));
            }
            if GENERATED_PARAMETER_SUFFIXES.contains(&suffix) {
                return Err(FcsError::InvalidWriterInput(
                    "parameter keywords must not override generated FCS keywords",
                ));
            }
        }
    }

    Ok(())
}

/// The delimiter is escaped by doubling it, so a key or value that is empty or
/// starts with the delimiter reads back as an escape instead. FCS 3.1 forbids
/// both; refuse them rather than write a file that reads back differently.
fn validate_text_pairs(pairs: &[(String, String)]) -> Result<(), FcsError> {
    let unrepresentable = pairs
        .iter()
        .flat_map(|(key, value)| [key, value])
        .any(|token| {
            token
                .as_bytes()
                .first()
                .is_none_or(|first| *first == TEXT_DELIMITER)
        });
    if unrepresentable {
        return Err(FcsError::InvalidWriterInput(
            "TEXT keywords and values must be non-empty and must not start with `|`",
        ));
    }
    Ok(())
}

fn validate_options(options: &FcsWriteOptions) -> Result<(), FcsError> {
    for (key, _) in &options.text_keywords {
        let key = key.trim();
        if key.is_empty() {
            return Err(FcsError::InvalidWriterInput(
                "custom TEXT keyword names must not be empty",
            ));
        }
        if is_generated_keyword(key) {
            return Err(FcsError::InvalidWriterInput(
                "custom TEXT keywords must not override generated FCS keywords",
            ));
        }
    }

    Ok(())
}

/// Keywords are case-insensitive, so `$tot` would repeat the generated `$TOT`.
fn is_generated_keyword(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    matches!(
        key.as_str(),
        "$BEGINSTEXT"
            | "$ENDSTEXT"
            | "$BEGINDATA"
            | "$ENDDATA"
            | "$BEGINANALYSIS"
            | "$ENDANALYSIS"
            | "$BYTEORD"
            | "$DATATYPE"
            | "$MODE"
            | "$NEXTDATA"
            | "$PAR"
            | "$TOT"
    ) || is_parameter_keyword(&key)
}

/// `$Pn*` keywords; dataset keywords such as `$PROJ` or `$PLATEID` are not.
fn is_parameter_keyword(key: &str) -> bool {
    key.strip_prefix("$P")
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
}

fn common_text_pairs<T: DataValue>(
    parameter_count: usize,
    row_count: usize,
) -> Vec<(String, String)> {
    let zero = format_text_offset(0);
    vec![
        ("$BEGINSTEXT".to_owned(), zero.clone()),
        ("$ENDSTEXT".to_owned(), zero.clone()),
        ("$BEGINDATA".to_owned(), zero.clone()),
        ("$ENDDATA".to_owned(), zero.clone()),
        ("$BEGINANALYSIS".to_owned(), zero.clone()),
        ("$ENDANALYSIS".to_owned(), zero.clone()),
        // FCS 3.1 only allows "1,2,3,4" or "4,3,2,1"; it names endianness for
        // doubles as well.
        ("$BYTEORD".to_owned(), "1,2,3,4".to_owned()),
        ("$DATATYPE".to_owned(), T::DATATYPE.to_owned()),
        ("$MODE".to_owned(), "L".to_owned()),
        ("$NEXTDATA".to_owned(), zero),
        ("$PAR".to_owned(), parameter_count.to_string()),
        ("$TOT".to_owned(), row_count.to_string()),
    ]
}

fn format_text_offset(offset: u64) -> String {
    format!("{offset:0TEXT_OFFSET_WIDTH$}")
}

fn column_range<T: DataValue>(column: &[T]) -> f64 {
    let maximum = column
        .iter()
        .map(|value| value.as_f64())
        .fold(0.0_f64, f64::max);
    maximum.ceil().max(1.0)
}

fn data_length<T: DataValue>(parameter_count: usize, row_count: usize) -> Result<u64, FcsError> {
    let parameter_count = u64::try_from(parameter_count)
        .map_err(|_| FcsError::InvalidWriterInput("parameter count exceeds supported range"))?;
    let row_count = u64::try_from(row_count)
        .map_err(|_| FcsError::InvalidWriterInput("row count exceeds supported range"))?;
    parameter_count
        .checked_mul(row_count)
        .and_then(|values| values.checked_mul(T::WIDTH))
        .ok_or(FcsError::InvalidWriterInput(
            "DATA segment length exceeds supported range",
        ))
}

fn format_header(text_start: u64, text_end: u64, data_start: u64, data_end: u64) -> String {
    let (data_start, data_end) = if data_end <= OFFSET_FIELD_LIMIT {
        (data_start, data_end)
    } else {
        (0, 0)
    };
    format!(
        "FCS3.1    {text_start:>8}{text_end:>8}{data_start:>8}{data_end:>8}{analysis_start:>8}{analysis_end:>8}",
        analysis_start = 0,
        analysis_end = 0
    )
}

fn write_data<W: Write, T: DataValue>(
    writer: &mut W,
    columns: &[Vec<T>],
    row_count: usize,
) -> Result<(), FcsError> {
    for row_index in 0..row_count {
        for column in columns {
            column[row_index].write_le(writer).map_err(FcsError::Io)?;
        }
    }

    Ok(())
}

fn encode_text(pairs: &[(String, String)]) -> Vec<u8> {
    let mut bytes = vec![TEXT_DELIMITER];
    for (key, value) in pairs {
        push_escaped(&mut bytes, key);
        bytes.push(TEXT_DELIMITER);
        push_escaped(&mut bytes, value);
        bytes.push(TEXT_DELIMITER);
    }

    bytes
}

fn push_escaped(bytes: &mut Vec<u8>, value: &str) {
    for byte in value.as_bytes() {
        bytes.push(*byte);
        if *byte == TEXT_DELIMITER {
            bytes.push(TEXT_DELIMITER);
        }
    }
}

fn set_pair(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, existing)) = pairs.iter_mut().find(|(current_key, _)| current_key == key) {
        value.clone_into(existing);
    } else {
        pairs.push((key.to_owned(), value.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_value<'a>(text: &'a [u8], key: &str) -> Option<&'a str> {
        let text = std::str::from_utf8(text).ok()?;
        let mut tokens = text.split('|').skip(1);
        while let Some(current) = tokens.next() {
            let value = tokens.next()?;
            if current == key {
                return Some(value);
            }
        }
        None
    }

    #[test]
    fn writes_offsets_for_data_over_100mb() {
        // 50M events x 1 channel x 4 bytes = 200 MB of DATA, never allocated.
        let parameters = [FcsWriteParameter::new("FL1-A")];
        let (header, text) =
            encode_header_and_text::<f32>(&parameters, &[1.0], 50_000_000, &FcsWriteOptions::new())
                .expect("large offsets should encode");

        let text_end = HEADER_LEN + text.len() as u64 - 1;
        let begin = text_value(&text, "$BEGINDATA").unwrap();
        let end = text_value(&text, "$ENDDATA").unwrap();
        assert_eq!(begin.parse::<u64>().unwrap(), text_end + 1);
        assert_eq!(end.parse::<u64>().unwrap(), text_end + 200_000_000);
        assert_eq!(&header[26..58], &format!("{:>8}", 0).repeat(4));
    }

    #[test]
    fn header_repeats_data_offsets_that_fit() {
        let (header, text) = encode_header_and_text::<f32>(
            &[FcsWriteParameter::new("FL1-A")],
            &[1.0],
            3,
            &FcsWriteOptions::new(),
        )
        .expect("small offsets should encode");
        let begin = text_value(&text, "$BEGINDATA")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let end = text_value(&text, "$ENDDATA")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(header[26..34].trim().parse::<u64>().unwrap(), begin);
        assert_eq!(header[34..42].trim().parse::<u64>().unwrap(), end);
        assert_eq!(end - begin + 1, 12);
    }

    #[test]
    fn keeps_dataset_keywords_starting_with_p() {
        let options = FcsWriteOptions::new().with_text_keyword("$PROJ", "study");
        let (_, text) =
            encode_header_and_text::<f32>(&[FcsWriteParameter::new("FL1-A")], &[1.0], 1, &options)
                .expect("$PROJ is a dataset keyword");
        assert_eq!(text_value(&text, "$PROJ"), Some("study"));
    }

    #[test]
    fn rejects_generated_parameter_keyword_suffix() {
        let parameters = [FcsWriteParameter::new("FL1-A").with_keyword("B", "16")];
        let error = encode_header_and_text::<f32>(&parameters, &[1.0], 1, &FcsWriteOptions::new())
            .expect_err("$PnB is generated");
        assert!(error.to_string().contains("generated"));
    }
}
