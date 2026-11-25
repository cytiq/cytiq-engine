//! A repaired file, read without writing it: the new HEADER and TEXT, then the
//! source's own DATA and ANALYSIS, then the CRC.

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

/// CRC of a data set whose CRC is not computed (FCS 3.1, 3.5).
pub(crate) const NO_CRC: &[u8; 8] = b"00000000";

/// Reads the pieces of a repaired file as one.
pub struct Repaired<R> {
    source: R,
    prefix: Vec<u8>,
    /// Ranges of the source that follow the prefix.
    ranges: Vec<Range<u64>>,
    position: u64,
}

impl<R> Repaired<R> {
    pub(crate) fn new(source: R, prefix: Vec<u8>, ranges: Vec<Range<u64>>) -> Self {
        Self {
            source,
            prefix,
            ranges,
            position: 0,
        }
    }

    /// Bytes of the whole repaired file.
    #[must_use]
    pub fn length(&self) -> u64 {
        let carried: u64 = self
            .ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum();
        self.prefix.len() as u64 + carried + NO_CRC.len() as u64
    }
}

impl<R: Read + Seek> Read for Repaired<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut at = self.position;
        let mut from_memory = |bytes: &[u8], at: u64| -> Option<usize> {
            let at = usize::try_from(at).ok().filter(|at| *at < bytes.len())?;
            let count = buffer.len().min(bytes.len() - at);
            buffer[..count].copy_from_slice(&bytes[at..at + count]);
            Some(count)
        };
        if let Some(count) = from_memory(&self.prefix, at) {
            self.position += count as u64;
            return Ok(count);
        }
        at -= self.prefix.len() as u64;
        for range in &self.ranges {
            let length = range.end - range.start;
            if at < length {
                self.source.seek(SeekFrom::Start(range.start + at))?;
                let left = usize::try_from(length - at).unwrap_or(usize::MAX);
                let count = buffer.len().min(left);
                let count = self.source.read(&mut buffer[..count])?;
                if count == 0 && !buffer.is_empty() {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                self.position += count as u64;
                return Ok(count);
            }
            at -= length;
        }
        let count = from_memory(NO_CRC, at).unwrap_or(0);
        self.position += count as u64;
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for Repaired<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::End(offset) => self.length().checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        };
        self.position = position.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn pieces_read_as_one_file_from_any_position() {
        let source = Cursor::new(b"0123456789".to_vec());
        let mut repaired = Repaired::new(source, b"head".to_vec(), vec![2..5, 7..8]);
        let mut all = Vec::new();
        repaired.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"head234700000000");
        assert_eq!(repaired.length(), all.len() as u64);
        for start in 0..=all.len() {
            let mut tail = Vec::new();
            repaired.seek(SeekFrom::Start(start as u64)).unwrap();
            repaired.read_to_end(&mut tail).unwrap();
            assert_eq!(tail, &all[start..]);
        }
        assert_eq!(
            repaired.seek(SeekFrom::End(-2)).unwrap(),
            all.len() as u64 - 2
        );
        assert!(repaired.seek(SeekFrom::Current(-1000)).is_err());
        repaired.seek(SeekFrom::Start(1000)).unwrap();
        assert_eq!(repaired.read(&mut [0; 4]).unwrap(), 0);
    }

    #[test]
    fn a_source_that_shrank_is_an_error_not_a_short_file() {
        let source = Cursor::new(b"01".to_vec());
        let mut repaired = Repaired::new(source, Vec::new(), std::iter::once(0..5).collect());
        assert!(repaired.read_to_end(&mut Vec::new()).is_err());
    }
}
