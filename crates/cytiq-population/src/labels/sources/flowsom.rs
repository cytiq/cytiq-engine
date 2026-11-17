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

/// One `FlowSOM` assignment record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// The event's row in the input FCS.
    pub event_index: u64,
    pub source_event_id: u64,
    pub node: u64,
    pub metacluster: u64,
}

/// The records of `FlowSOM` assignments read from `reader`: the exact
/// header, then four canonical unsigned integers per line, nodes and
/// metaclusters from 1, input rows strictly increasing. Each line is bounded
/// before it is buffered; the first malformed line ends the records with an
/// error.
pub fn read_flowsom_assignments<R: Read>(reader: R) -> Assignments<R> {
    Assignments {
        reader: BufReader::new(reader),
        line: Vec::new(),
        row: 0,
        previous: None,
        state: Some(false),
    }
}

/// Iterator of [`read_flowsom_assignments`].
pub struct Assignments<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
    row: u64,
    previous: Option<u64>,
    /// Whether the header was read; `None` once the records ended.
    state: Option<bool>,
}

impl<R: Read> Assignments<R> {
    fn next_record(&mut self) -> Result<Option<Assignment>> {
        if self.state == Some(false) {
            self.state = Some(true);
            require(
                next_line(&mut self.reader, &mut self.line)? && self.line == HEADER.as_bytes(),
                format!("FlowSOM assignments must start with the header `{HEADER}`"),
            )?;
        }
        if !next_line(&mut self.reader, &mut self.line)? {
            return Ok(None);
        }
        let row = self.row;
        let [event_index, source_event_id, node, metacluster] =
            record(&self.line).ok_or_else(|| {
                PopulationError::Invalid(format!(
                    "FlowSOM assignment {row} is not four unsigned integers"
                ))
            })?;
        require(
            self.previous.is_none_or(|last| event_index > last),
            format!("FlowSOM assignment {row} is not in input row order"),
        )?;
        require(
            node > 0 && metacluster > 0,
            format!("FlowSOM assignment {row} numbers nodes or metaclusters from 0"),
        )?;
        self.previous = Some(event_index);
        self.row += 1;
        Ok(Some(Assignment {
            event_index,
            source_event_id,
            node,
            metacluster,
        }))
    }
}

impl<R: Read> Iterator for Assignments<R> {
    type Item = Result<Assignment>;

    fn next(&mut self) -> Option<Self::Item> {
        self.state?;
        let record = self.next_record().transpose();
        if !matches!(record, Some(Ok(_))) {
            self.state = None;
        }
        record
    }
}

/// Stream the assignments at `path` into `rows`: every record is a
/// classified event with its metacluster and node.
pub(crate) fn stream(path: &Path, rows: &mut Rows<'_>) -> Result<()> {
    for assignment in read_flowsom_assignments(std::fs::File::open(path)?) {
        let assignment = assignment?;
        let id = assignment.source_event_id;
        rows.at(id)?;
        rows.assessed(id)?;
        rows.push(LabelState::Classified, |builders| {
            builders[0].uint(Some(assignment.metacluster));
            builders[1].uint(Some(assignment.node));
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
