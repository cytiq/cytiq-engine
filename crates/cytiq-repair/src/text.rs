//! Split TEXT into pairs, marking what can be split in several ways.
//!
//! The delimiter inside a keyword or a value is doubled, and neither may be
//! empty or start with it (FCS 3.1, 3.2.7 and 3.2.9). A doubled delimiter in a
//! value is therefore always an escape: read as an empty value it would leave
//! an empty keyword behind. Where a keyword name is expected, it is either an
//! escape in the name or the end of an empty value, and every such reading ends
//! at the same byte: the readings differ in those pairs only.

use std::ops::Range;

use crate::codes::{Refusal, RefusalCode};

/// A pair as TEXT holds it, escapes included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pair {
    pub key: Range<usize>,
    pub value: Range<usize>,
}

/// TEXT, split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Split {
    pub delimiter: u8,
    pub pairs: Vec<Pair>,
    /// Pairs the possible readings disagree on, each up to the next pair.
    pub ambiguous: Vec<Range<usize>>,
    /// Spaces and NULs after the last delimiter.
    pub padding: Range<usize>,
}

pub(crate) fn split(text: &[u8]) -> Result<Split, Refusal> {
    let Some((&delimiter, rest)) = text.split_first() else {
        return Err(unterminated("TEXT is empty"));
    };
    if rest.is_empty() {
        return Err(unterminated("TEXT holds only its delimiter"));
    }
    let mut split = Split {
        delimiter,
        pairs: Vec::new(),
        ambiguous: Vec::new(),
        padding: text.len()..text.len(),
    };
    let mut at = 1;
    while at < text.len() {
        if is_padding(&text[at..], delimiter) {
            split.padding = at..text.len();
            break;
        }
        at = pair(text, at, &mut split)?;
    }
    Ok(split)
}

/// Read the pair, or the ambiguous pairs, at `start`; where the next begins.
fn pair(text: &[u8], start: usize, split: &mut Split) -> Result<usize, Refusal> {
    let delimiter = split.delimiter;
    let mut ambiguous = false;
    let mut at = start;
    let key = loop {
        let (end, run) = next_run(text, at, delimiter)
            .ok_or_else(|| unterminated("the last keyword has no value"))?;
        if end == at || run > 2 {
            return Err(Refusal::new(
                RefusalCode::AmbiguousText,
                format!(
                    "{run} delimiters in a row where a keyword is expected, at byte {end} of TEXT"
                ),
            ));
        }
        at = end + run;
        if run == 1 {
            break start..end;
        }
        ambiguous = true;
        if is_padding(&text[at..], delimiter) {
            // Only the reading that ends with an empty value ends TEXT.
            split.ambiguous.push(start..at);
            return Ok(at);
        }
    };
    let value = at;
    loop {
        let (end, run) = next_run(text, at, delimiter)
            .ok_or_else(|| unterminated("TEXT ends inside a value"))?;
        at = end + run;
        if run % 2 == 1 {
            break;
        }
    }
    if ambiguous {
        split.ambiguous.push(start..at);
    } else {
        split.pairs.push(Pair {
            key,
            value: value..at - 1,
        });
    }
    Ok(at)
}

/// The first delimiter at or after `at`, and how many follow one another.
fn next_run(text: &[u8], at: usize, delimiter: u8) -> Option<(usize, usize)> {
    let start = at + text[at..].iter().position(|byte| *byte == delimiter)?;
    let run = text[start..]
        .iter()
        .take_while(|byte| **byte == delimiter)
        .count();
    Some((start, run))
}

/// Spaces and NULs only (or nothing), none of them the delimiter.
fn is_padding(bytes: &[u8], delimiter: u8) -> bool {
    bytes
        .iter()
        .all(|byte| matches!(byte, b' ' | 0) && *byte != delimiter)
}

fn unterminated(message: &str) -> Refusal {
    Refusal::new(RefusalCode::TextUnterminated, message)
}

/// A token with its escapes removed.
pub(crate) fn unescape(raw: &[u8], delimiter: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter();
    while let Some(&byte) = bytes.next() {
        if byte == delimiter {
            bytes.next();
        }
        out.push(byte);
    }
    out
}

/// A token with its delimiters doubled.
pub(crate) fn escape(raw: &[u8], delimiter: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for &byte in raw {
        out.push(byte);
        if byte == delimiter {
            out.push(byte);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &[u8]) -> (Vec<(String, String)>, Vec<String>, usize) {
        let split = split(text).unwrap();
        let token = |range: &Range<usize>| {
            String::from_utf8(unescape(&text[range.clone()], split.delimiter)).unwrap()
        };
        (
            split
                .pairs
                .iter()
                .map(|pair| (token(&pair.key), token(&pair.value)))
                .collect(),
            split
                .ambiguous
                .iter()
                .map(|range| String::from_utf8(text[range.clone()].to_vec()).unwrap())
                .collect(),
            split.padding.len(),
        )
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn pairs_escapes_and_padding_are_told_apart() {
        let found = tokens(b"/$P3F/488//10/$P3L/488/   \0");
        assert_eq!(found.0, pairs(&[("$P3F", "488/10"), ("$P3L", "488")]));
        assert_eq!(found.1, Vec::<String>::new());
        assert_eq!(found.2, 4);
        // Three delimiters end a value that itself ends with one.
        assert_eq!(tokens(b"/A/x///B/y/").0, pairs(&[("A", "x/"), ("B", "y")]));
        // Four are two escapes.
        assert_eq!(tokens(b"/A/x////y/").0, pairs(&[("A", "x//y")]));
    }

    #[test]
    fn pairs_the_readings_disagree_on_are_marked_and_the_rest_is_kept() {
        // The shape of an Attune file: empty values, or escapes in names.
        let found = tokens(b"/$FIL/a/$INST//$EXP/Admin/$OP/b/$COM//$SMNO//$LOST//    ");
        assert_eq!(found.0, pairs(&[("$FIL", "a"), ("$OP", "b")]));
        assert_eq!(found.1, ["$INST//$EXP/Admin/", "$COM//$SMNO//$LOST//"]);
        assert_eq!(found.2, 4);
        assert_eq!(tokens(b"/A//").1, ["A//"]);
    }

    #[test]
    fn text_that_does_not_end_is_refused() {
        for text in [
            &b""[..],
            b"/",
            b"/A",
            b"/A/",
            b"/A/x",
            b"/A/x/B",
            b"/A/x/B/",
            b"/A/x//",
            b"/A/x//  ",
            b"/A/x/B//C",
        ] {
            let refusal = split(text).unwrap_err();
            assert_eq!(refusal.code, RefusalCode::TextUnterminated, "{text:?}");
        }
    }

    #[test]
    fn runs_where_a_keyword_is_expected_are_ambiguous() {
        for text in [&b"//A/x/"[..], b"/A///x/", b"/A////x/", b"/A/x/B///"] {
            let refusal = split(text).unwrap_err();
            assert_eq!(refusal.code, RefusalCode::AmbiguousText, "{text:?}");
        }
    }

    #[test]
    fn escapes_round_trip() {
        for token in [&b"plain"[..], b"a/b", b"a//b/", b""] {
            assert_eq!(unescape(&escape(token, b'/'), b'/'), token);
        }
    }

    #[test]
    fn the_delimiter_is_never_padding() {
        // Delimited by spaces: the run at the end is an escape and the end.
        let found = tokens(b" A x   ");
        assert_eq!(found.0, pairs(&[("A", "x ")]));
        assert_eq!(found.2, 0);
    }
}
