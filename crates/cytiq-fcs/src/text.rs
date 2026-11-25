use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::error::FcsError;
use crate::types::{ByteRange, FcsText, TextSegment};

pub(crate) fn read_text_segment<R: Read + Seek>(
    reader: &mut R,
    range: ByteRange,
    section_name: &'static str,
) -> Result<TextSegment, FcsError> {
    reader
        .seek(SeekFrom::Start(range.start))
        .map_err(FcsError::Io)?;
    let buffer_len =
        usize::try_from(range.len()).map_err(|_| FcsError::InvalidText("segment too large"))?;
    let mut buffer = vec![0_u8; buffer_len];
    reader.read_exact(&mut buffer).map_err(FcsError::Io)?;

    let delimiter = *buffer
        .first()
        .ok_or(FcsError::InvalidText("TEXT segment must not be empty"))?;
    let tokens = split_tokens(&buffer, delimiter)?;
    if tokens.len() % 2 != 0 {
        return Err(FcsError::InvalidText(
            "TEXT tokens must form key/value pairs",
        ));
    }

    let mut pairs = Vec::with_capacity(tokens.len() / 2);
    for [key, value] in tokens.as_chunks::<2>().0 {
        let key = std::str::from_utf8(key).map_err(|_| FcsError::InvalidUtf8 {
            section: section_name,
        })?;
        let value = std::str::from_utf8(value).map_err(|_| FcsError::InvalidUtf8 {
            section: section_name,
        })?;
        pairs.push((key.to_owned(), value.to_owned()));
    }

    Ok(TextSegment { delimiter, pairs })
}

pub(crate) fn build_text(
    primary: TextSegment,
    supplemental: Option<TextSegment>,
) -> Result<FcsText, FcsError> {
    let mut lookup = HashMap::new();
    for (key, value) in &primary.pairs {
        FcsText::insert_lookup(&mut lookup, key, value)?;
    }
    if let Some(segment) = &supplemental {
        for (key, value) in &segment.pairs {
            FcsText::insert_lookup(&mut lookup, key, value)?;
        }
    }

    Ok(FcsText::new(primary, supplemental, lookup))
}

pub(crate) fn parse_segment_range(
    segment: &TextSegment,
    begin_key: &str,
    end_key: &str,
    name: &'static str,
    required: bool,
) -> Result<Option<ByteRange>, FcsError> {
    let begin_value = segment
        .pairs
        .iter()
        .find_map(|(key, value)| (key == begin_key).then_some(value.as_str()));
    let end_value = segment
        .pairs
        .iter()
        .find_map(|(key, value)| (key == end_key).then_some(value.as_str()));

    match (begin_value, end_value) {
        (Some(begin), Some(end)) => {
            let begin_offset = parse_offset_value(begin_key, begin)?;
            let end_offset = parse_offset_value(end_key, end)?;
            if begin_offset == 0 && end_offset == 0 {
                Ok(None)
            } else if begin_offset <= end_offset {
                Ok(Some(ByteRange::new(begin_offset, end_offset)))
            } else {
                Err(FcsError::InvalidText(match name {
                    "DATA" => "DATA segment start is after segment end",
                    "ANALYSIS" => "ANALYSIS segment start is after segment end",
                    _ => "TEXT segment start is after segment end",
                }))
            }
        }
        (None, None) if required => Err(FcsError::MissingKeyword(begin_key.to_owned())),
        (None, None) => Ok(None),
        _ => Err(FcsError::InvalidText(
            "segment offsets must include both begin and end",
        )),
    }
}

fn split_tokens(buffer: &[u8], delimiter: u8) -> Result<Vec<Vec<u8>>, FcsError> {
    if buffer.len() < 2 {
        return Err(FcsError::InvalidText("TEXT segment is too short"));
    }

    let mut tokens = Vec::new();
    let mut token = Vec::new();
    let mut index = 1;
    while index < buffer.len() {
        let byte = buffer[index];
        if byte == delimiter {
            if index + 1 < buffer.len() && buffer[index + 1] == delimiter {
                token.push(delimiter);
                index += 2;
                continue;
            }

            tokens.push(token);
            token = Vec::new();
            index += 1;
            continue;
        }

        token.push(byte);
        index += 1;
    }

    if !token.is_empty() {
        return Err(FcsError::InvalidText(
            "TEXT segment must end with a delimiter",
        ));
    }

    Ok(tokens)
}

pub(crate) fn parse_offset_value(key: &str, value: &str) -> Result<u64, FcsError> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| FcsError::InvalidKeywordValue {
            key: key.to_owned(),
            value: value.to_owned(),
        })
}
