use crate::error::FcsError;
use crate::limits::PARAMETERS_MAX;
use crate::types::{FcsText, FcsVersion, Parameter, ParameterDataType, ParameterWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ByteOrder {
    None,
    Ordered([usize; 8], usize),
    LittleEndian,
    BigEndian,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParameterEncoding {
    AsciiFixed { width: usize },
    Integer { width_bytes: usize },
    Float32,
    Float64,
}

impl ParameterEncoding {
    /// Bytes one value takes in DATA.
    pub const fn stored_bytes(self) -> usize {
        match self {
            Self::AsciiFixed { width } => width,
            Self::Integer { width_bytes } => width_bytes,
            Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// Bytes one value takes once decoded: `A` and `I` widen to `u64`.
    pub const fn decoded_bytes(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::AsciiFixed { .. } | Self::Integer { .. } | Self::Float64 => 8,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedParameter {
    pub public: Parameter,
    pub encoding: ParameterEncoding,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedLayout {
    byte_order: ByteOrder,
    row_count: usize,
    parameters: Vec<ResolvedParameter>,
}

impl ResolvedLayout {
    #[must_use]
    pub const fn byte_order(&self) -> ByteOrder {
        self.byte_order
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    #[must_use]
    pub fn parameters(&self) -> &[ResolvedParameter] {
        &self.parameters
    }

    #[must_use]
    pub fn public_parameters(&self) -> Vec<Parameter> {
        self.parameters
            .iter()
            .map(|parameter| parameter.public.clone())
            .collect()
    }
}

pub(crate) fn resolve_layout(
    version: FcsVersion,
    text: &FcsText,
) -> Result<ResolvedLayout, FcsError> {
    let mode = trimmed_required_value(text, "$MODE")?;
    if mode != "L" {
        return Err(FcsError::UnsupportedMode(mode.to_owned()));
    }

    let row_count = parse_required_usize(text, "$TOT")?;
    let parameter_count = parse_parameter_count(text)?;
    let dataset_type = parse_data_type(required_value(text, "$DATATYPE")?)?;
    let byte_order_raw = required_value(text, "$BYTEORD")?;

    let mut keywords_by_parameter = group_parameter_keywords(text, parameter_count);
    let mut parameters = Vec::with_capacity(parameter_count);
    let mut max_numeric_width = 0_usize;
    let mut has_numeric = false;
    let mut has_delimited_ascii = false;
    let mut fcs30_numeric_width = None;

    for index in 1..=parameter_count {
        let keywords = std::mem::take(&mut keywords_by_parameter[index - 1]);
        let parameter = resolve_parameter(version, text, index, dataset_type, keywords)?;
        if !matches!(parameter.encoding, ParameterEncoding::AsciiFixed { .. }) {
            let width_bytes = parameter.encoding.stored_bytes();
            has_numeric = true;
            max_numeric_width = max_numeric_width.max(width_bytes);
            if version == FcsVersion::V3_0 {
                ensure_consistent_fcs30_width(&mut fcs30_numeric_width, width_bytes)?;
            }
        }
        if parameter.public.width == ParameterWidth::DelimitedAscii {
            has_delimited_ascii = true;
        }
        parameters.push(parameter);
    }

    if has_delimited_ascii {
        return Err(FcsError::UnsupportedLayout(
            "delimited ASCII DATA is not supported yet",
        ));
    }

    let byte_order = if has_numeric {
        parse_byte_order(version, byte_order_raw, max_numeric_width)?
    } else {
        ByteOrder::None
    };

    Ok(ResolvedLayout {
        byte_order,
        row_count,
        parameters,
    })
}

fn resolve_parameter(
    version: FcsVersion,
    text: &FcsText,
    index: usize,
    dataset_type: ParameterDataType,
    keywords: Vec<(String, String)>,
) -> Result<ResolvedParameter, FcsError> {
    let index_u16 = u16::try_from(index)
        .map_err(|_| FcsError::UnsupportedLayout("parameter count exceeds u16"))?;
    let name = required_parameter_value(text, index, "N")?.to_owned();
    let label = optional_parameter_value(text, index, "S").map(str::to_owned);
    let width_value = required_parameter_value(text, index, "B")?;
    let _range_value = required_parameter_value(text, index, "R")?;
    let _scale_value = required_parameter_value(text, index, "E")?;

    let data_type = if version == FcsVersion::V3_2 {
        optional_parameter_value(text, index, "DATATYPE")
            .map(parse_data_type)
            .transpose()?
            .unwrap_or(dataset_type)
    } else {
        dataset_type
    };

    let (width, encoding) = parse_width(index_u16, width_value, data_type)?;
    Ok(ResolvedParameter {
        public: Parameter {
            index: index_u16,
            name,
            label,
            width,
            data_type,
            keywords,
        },
        encoding,
    })
}

fn parse_data_type(value: &str) -> Result<ParameterDataType, FcsError> {
    match value.trim() {
        "A" => Ok(ParameterDataType::Ascii),
        "I" => Ok(ParameterDataType::Integer),
        "F" => Ok(ParameterDataType::Float),
        "D" => Ok(ParameterDataType::Double),
        _ => Err(FcsError::InvalidKeywordValue {
            key: "$DATATYPE".to_owned(),
            value: value.to_owned(),
        }),
    }
}

fn parse_width(
    parameter: u16,
    width_value: &str,
    data_type: ParameterDataType,
) -> Result<(ParameterWidth, ParameterEncoding), FcsError> {
    let trimmed_width = width_value.trim();
    if trimmed_width == "*" {
        return if data_type == ParameterDataType::Ascii {
            Ok((
                ParameterWidth::DelimitedAscii,
                ParameterEncoding::AsciiFixed { width: 0 },
            ))
        } else {
            Err(FcsError::UnsupportedWidth {
                parameter,
                value: width_value.to_owned(),
            })
        };
    }

    let width_number = trimmed_width
        .parse::<u8>()
        .map_err(|_| FcsError::UnsupportedWidth {
            parameter,
            value: width_value.to_owned(),
        })?;

    match data_type {
        ParameterDataType::Ascii => {
            if (1..=20).contains(&width_number) {
                Ok((
                    ParameterWidth::AsciiChars(width_number),
                    ParameterEncoding::AsciiFixed {
                        width: usize::from(width_number),
                    },
                ))
            } else {
                Err(FcsError::UnsupportedWidth {
                    parameter,
                    value: width_value.to_owned(),
                })
            }
        }
        ParameterDataType::Integer => {
            if width_number % 8 == 0 && (8..=64).contains(&width_number) {
                Ok((
                    ParameterWidth::Bits(width_number),
                    ParameterEncoding::Integer {
                        width_bytes: usize::from(width_number / 8),
                    },
                ))
            } else {
                Err(FcsError::UnsupportedWidth {
                    parameter,
                    value: width_value.to_owned(),
                })
            }
        }
        ParameterDataType::Float => {
            if width_number == 32 {
                Ok((
                    ParameterWidth::Bits(width_number),
                    ParameterEncoding::Float32,
                ))
            } else {
                Err(FcsError::UnsupportedWidth {
                    parameter,
                    value: width_value.to_owned(),
                })
            }
        }
        ParameterDataType::Double => {
            if width_number == 64 {
                Ok((
                    ParameterWidth::Bits(width_number),
                    ParameterEncoding::Float64,
                ))
            } else {
                Err(FcsError::UnsupportedWidth {
                    parameter,
                    value: width_value.to_owned(),
                })
            }
        }
    }
}

fn parse_byte_order(
    version: FcsVersion,
    raw_value: &str,
    numeric_width: usize,
) -> Result<ByteOrder, FcsError> {
    let trimmed_value = raw_value.trim();
    match version {
        FcsVersion::V3_0 => parse_ordered_byte_order(trimmed_value, numeric_width),
        FcsVersion::V3_1 | FcsVersion::V3_2 => match trimmed_value {
            "1,2,3,4" => Ok(ByteOrder::LittleEndian),
            "4,3,2,1" => Ok(ByteOrder::BigEndian),
            _ => Err(FcsError::UnsupportedByteOrder(raw_value.to_owned())),
        },
    }
}

fn required_value<'a>(text: &'a FcsText, key: &str) -> Result<&'a str, FcsError> {
    text.get(key)
        .ok_or_else(|| FcsError::MissingKeyword(key.to_owned()))
}

fn trimmed_required_value<'a>(text: &'a FcsText, key: &str) -> Result<&'a str, FcsError> {
    Ok(required_value(text, key)?.trim())
}

fn parse_required_usize(text: &FcsText, key: &str) -> Result<usize, FcsError> {
    let value = required_value(text, key)?;
    value
        .trim()
        .parse::<usize>()
        .map_err(|_| FcsError::InvalidKeywordValue {
            key: key.to_owned(),
            value: value.to_owned(),
        })
}

fn required_parameter_value<'a>(
    text: &'a FcsText,
    index: usize,
    suffix: &str,
) -> Result<&'a str, FcsError> {
    let key = format!("$P{index}{suffix}");
    text.get(&key).ok_or(FcsError::MissingKeyword(key))
}

fn optional_parameter_value<'a>(text: &'a FcsText, index: usize, suffix: &str) -> Option<&'a str> {
    let key = format!("$P{index}{suffix}");
    text.get(&key)
}

fn parse_parameter_count(text: &FcsText) -> Result<usize, FcsError> {
    let count = parse_required_usize(text, "$PAR")?;
    if count == 0 {
        return Err(FcsError::InvalidKeywordValue {
            key: "$PAR".to_owned(),
            value: required_value(text, "$PAR")?.to_owned(),
        });
    }
    if count > PARAMETERS_MAX {
        return Err(FcsError::LimitExceeded {
            what: "$PAR",
            value: u64::try_from(count).unwrap_or(u64::MAX),
            limit: u64::try_from(PARAMETERS_MAX).unwrap_or(u64::MAX),
        });
    }
    Ok(count)
}

/// Each parameter's `$Pn*` keywords in TEXT order, in one pass over TEXT.
/// Keywords are case-insensitive: their names are stored in upper case, the
/// form every lookup uses, and their values as written.
fn group_parameter_keywords(text: &FcsText, parameter_count: usize) -> Vec<Vec<(String, String)>> {
    let mut groups = vec![Vec::new(); parameter_count];
    for (key, value) in text.iter() {
        let key = key.to_ascii_uppercase();
        if let Some(index) = parameter_keyword_index(&key)
            && (1..=parameter_count).contains(&index)
        {
            groups[index - 1].push((key, value.to_owned()));
        }
    }
    groups
}

/// The `n` of a `$Pn*` keyword. The digits must be followed by more
/// characters, and `n` has no leading zero, so `$P1` alone and `$P01N` belong
/// to no parameter, and `$P1` never picks up `$P10*`.
fn parameter_keyword_index(key: &str) -> Option<usize> {
    let rest = key.strip_prefix("$P")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (number, suffix) = rest.split_at(digits);
    if number.is_empty() || number.starts_with('0') || suffix.is_empty() {
        return None;
    }
    number.parse().ok()
}

fn ensure_consistent_fcs30_width(
    expected_width: &mut Option<usize>,
    width_bytes: usize,
) -> Result<(), FcsError> {
    match expected_width {
        Some(existing) if *existing != width_bytes => Err(FcsError::UnsupportedLayout(
            "FCS 3.0 numeric parameters must share a single width",
        )),
        Some(_) => Ok(()),
        None => {
            *expected_width = Some(width_bytes);
            Ok(())
        }
    }
}

fn parse_ordered_byte_order(raw_value: &str, numeric_width: usize) -> Result<ByteOrder, FcsError> {
    if !(1..=8).contains(&numeric_width) {
        return Err(FcsError::UnsupportedByteOrder(raw_value.to_owned()));
    }

    let components = raw_value
        .split(',')
        .map(str::trim)
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| FcsError::UnsupportedByteOrder(raw_value.to_owned()))?;
    if components.len() != numeric_width {
        return Err(FcsError::UnsupportedByteOrder(raw_value.to_owned()));
    }

    let mut order = [0_usize; 8];
    let mut seen = [false; 8];
    for (index, component) in components.into_iter().enumerate() {
        if component == 0 || component > numeric_width {
            return Err(FcsError::UnsupportedByteOrder(raw_value.to_owned()));
        }

        let position = component - 1;
        if seen[position] {
            return Err(FcsError::UnsupportedByteOrder(raw_value.to_owned()));
        }

        seen[position] = true;
        order[index] = position;
    }

    Ok(ByteOrder::Ordered(order, numeric_width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TextSegment;

    fn text(overrides: &[(&str, &str)]) -> FcsText {
        let mut pairs = [
            ("$MODE", "L"),
            ("$TOT", "7"),
            ("$PAR", "2"),
            ("$DATATYPE", "I"),
            ("$BYTEORD", "1,2,3,4"),
            ("$P1N", "A"),
            ("$P1B", "16"),
            ("$P1R", "65536"),
            ("$P1E", "0,0"),
            ("$P1S", "marker A"),
            ("$P2N", "B"),
            ("$P2B", "32"),
            ("$P2R", "65536"),
            ("$P2E", "0,0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<Vec<_>>();
        for (key, value) in overrides {
            if let Some(pair) = pairs.iter_mut().find(|(k, _)| k == key) {
                pair.1 = (*value).into();
            } else {
                pairs.push(((*key).into(), (*value).into()));
            }
        }
        let segment = TextSegment {
            delimiter: b'|',
            pairs,
        };
        crate::text::build_text(segment, None).unwrap()
    }

    #[test]
    fn layout_preserves_rows_parameters_labels_and_keywords() {
        let layout = resolve_layout(FcsVersion::V3_1, &text(&[])).unwrap();
        assert_eq!(layout.row_count(), 7);
        assert_eq!(layout.byte_order(), ByteOrder::LittleEndian);
        let parameters = layout.public_parameters();
        assert_eq!(parameters.len(), 2);
        assert_eq!((parameters[0].index, parameters[1].index), (1, 2));
        assert_eq!(
            (&parameters[0].name, &parameters[1].name),
            (&"A".to_owned(), &"B".to_owned())
        );
        assert_eq!(parameters[0].label.as_deref(), Some("marker A"));
        assert_eq!(parameters[1].label, None);
        assert_eq!(parameters[0].width, ParameterWidth::Bits(16));
        assert_eq!(parameters[1].width, ParameterWidth::Bits(32));
        assert_eq!(parameters[0].data_type, ParameterDataType::Integer);
        assert!(
            parameters[0]
                .keywords
                .contains(&("$P1S".into(), "marker A".into()))
        );
        assert!(
            parameters[0]
                .keywords
                .iter()
                .all(|(k, _)| k.starts_with("$P1"))
        );
        assert!(
            matches!(resolve_layout(FcsVersion::V3_1, &text(&[("$MODE", "C")])), Err(FcsError::UnsupportedMode(mode)) if mode == "C")
        );
    }

    #[test]
    fn layout_honors_version_specific_types_and_widths() {
        assert!(matches!(
            resolve_layout(FcsVersion::V3_0, &text(&[("$BYTEORD", "1,2")])),
            Err(FcsError::UnsupportedLayout(_))
        ));
        let fixed = text(&[("$P2B", "16"), ("$BYTEORD", "2,1")]);
        assert_eq!(
            resolve_layout(FcsVersion::V3_0, &fixed)
                .unwrap()
                .byte_order(),
            ByteOrder::Ordered([1, 0, 0, 0, 0, 0, 0, 0], 2)
        );
        let mixed = text(&[("$P2DATATYPE", "F")]);
        assert_eq!(
            resolve_layout(FcsVersion::V3_2, &mixed)
                .unwrap()
                .parameters()[1]
                .encoding,
            ParameterEncoding::Float32
        );
        assert_eq!(
            resolve_layout(FcsVersion::V3_1, &mixed)
                .unwrap()
                .parameters()[1]
                .encoding,
            ParameterEncoding::Integer { width_bytes: 4 }
        );
        let ascii = text(&[
            ("$DATATYPE", "A"),
            ("$P1B", "5"),
            ("$P2B", "10"),
            ("$BYTEORD", "ignored"),
        ]);
        assert_eq!(
            resolve_layout(FcsVersion::V3_2, &ascii)
                .unwrap()
                .byte_order(),
            ByteOrder::None
        );
        assert!(matches!(
            resolve_layout(
                FcsVersion::V3_2,
                &text(&[("$DATATYPE", "A"), ("$P1B", "*"), ("$P2B", "10")])
            ),
            Err(FcsError::UnsupportedLayout(_))
        ));
        assert!(matches!(
            resolve_layout(FcsVersion::V3_2, &text(&[("$P2DATATYPE", "bad")])),
            Err(FcsError::InvalidKeywordValue { .. })
        ));
    }
}
