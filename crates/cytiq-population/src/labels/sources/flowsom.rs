//! `FlowSOM` assignments: the CSV `cytiq flowsom` writes, one record per
//! assigned event, `event_index,source_event_id,flowsom_node,
//! flowsom_metacluster`. `event_index` is the event's row in the input FCS,
//! nodes and metaclusters are numbered from 1. Masked events have no
//! record, and the file names no acquisition.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use super::Rows;
use crate::labels::LabelState;
use crate::{PopulationError, Result, require};

/// The native format, as a label descriptor names it.
pub(crate) const CONTRACT: &str = "cytiq.flowsom.assignments.csv";
const HEADER: &str = "event_index,source_event_id,flowsom_node,flowsom_metacluster";
/// Longest record: four 20-digit numbers, three commas and a CRLF.
const LINE_MAX: u64 = 4 * 20 + 3 + 2;

/// Stream the assignments at `path` into `rows`: every record is a
/// classified event with its metacluster and node.
pub(crate) fn stream(path: &Path, rows: &mut Rows<'_>) -> Result<()> {
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut line = Vec::new();
    require(
        next_line(&mut reader, &mut line)? && line == HEADER.as_bytes(),
        format!("FlowSOM assignments must start with the header `{HEADER}`"),
    )?;
    let mut row = 0_u64;
    let mut previous: Option<u64> = None;
    while next_line(&mut reader, &mut line)? {
        let fields = record(&line).ok_or_else(|| {
            PopulationError::Invalid(format!(
                "FlowSOM assignment {row} is not four unsigned integers"
            ))
        })?;
        let [index, id, node, metacluster] = fields;
        require(
            previous.is_none_or(|last| index > last),
            format!("FlowSOM assignment {row} is not in input row order"),
        )?;
        previous = Some(index);
        row += 1;
        rows.at(id)?;
        rows.assessed(id)?;
        rows.push(LabelState::Classified, |builders| {
            builders[0].uint(Some(metacluster));
            builders[1].uint(Some(node));
        })?;
    }
    Ok(())
}

/// Read the next line into `line` without its terminator; `false` at the
/// end. A line longer than any record is refused before it is buffered.
fn next_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool> {
    line.clear();
    let read = reader.take(LINE_MAX + 1).read_until(b'\n', line)?;
    if read == 0 {
        return Ok(false);
    }
    require(
        line.last() == Some(&b'\n') || read as u64 <= LINE_MAX,
        "a FlowSOM assignment line is longer than any record",
    )?;
    require(
        line.last() == Some(&b'\n'),
        "FlowSOM assignments must end every line with a newline",
    )?;
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(true)
}

/// Four comma-separated decimal integers, without signs, spaces or leading
/// zeros.
fn record(line: &[u8]) -> Option<[u64; 4]> {
    let mut fields = line.split(|byte| *byte == b',');
    let mut values = [0_u64; 4];
    for value in &mut values {
        let field = fields.next()?;
        let canonical = !field.is_empty()
            && field.iter().all(u8::is_ascii_digit)
            && (field.len() == 1 || field[0] != b'0');
        if !canonical {
            return None;
        }
        *value = std::str::from_utf8(field).ok()?.parse().ok()?;
    }
    fields.next().is_none().then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_four_canonical_integers() {
        assert_eq!(record(b"0,12,3,1"), Some([0, 12, 3, 1]));
        for invalid in [
            &b"0,12,3"[..],
            b"0,12,3,1,5",
            b"0,-1,3,1",
            b"0, 12,3,1",
            b"0,012,3,1",
            b"0,,3,1",
            b"0,99999999999999999999,3,1",
        ] {
            assert_eq!(
                record(invalid),
                None,
                "{}",
                String::from_utf8_lossy(invalid)
            );
        }
    }

    #[test]
    fn overlong_and_unterminated_lines_are_refused() {
        let mut line = Vec::new();
        let long = [b'1'; 200];
        assert!(next_line(&mut &long[..], &mut line).is_err());
        assert!(next_line(&mut &b"0,1,2,3"[..], &mut line).is_err());
        assert!(next_line(&mut &b"0,1,2,3\r\n"[..], &mut line).unwrap());
        assert_eq!(line, b"0,1,2,3");
    }
}
