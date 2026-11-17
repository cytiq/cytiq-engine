//! The values digest of a label table: SHA-256 over [`VALUES_DOMAIN`], the
//! `annotation_id` and every row in order. A row is its `source_event_id`
//! (u64, little-endian), its `state` (u32 little-endian length, then UTF-8),
//! then each column after `state`: a tag byte, 0 for null and 1 otherwise,
//! followed for a value by one byte 0/1 (Boolean), eight little-endian bytes
//! (`UInt64`), the four little-endian bytes of its bit pattern (`Float32`), or
//! a u32 little-endian length and the UTF-8 bytes (Utf8). It does not depend
//! on the Parquet encoding, row groups or batches.

use arrow_array::{Array, ArrayRef, BooleanArray, Float32Array, StringArray, UInt64Array};
use sha2::{Digest, Sha256};

use super::VALUES_DOMAIN;

/// Bytes of rows buffered before they are hashed.
const CHUNK_BYTES: usize = 64 << 10;

pub(crate) struct LabelsHasher {
    hasher: Sha256,
    chunk: Vec<u8>,
}

/// A column of one batch, downcast once.
enum Cells<'a> {
    Boolean(&'a BooleanArray),
    UInt64(&'a UInt64Array),
    Float32(&'a Float32Array),
    Utf8(&'a StringArray),
}

impl<'a> Cells<'a> {
    fn of(array: &'a ArrayRef) -> Option<Self> {
        let any = array.as_any();
        any.downcast_ref()
            .map(Cells::Boolean)
            .or_else(|| any.downcast_ref().map(Cells::UInt64))
            .or_else(|| any.downcast_ref().map(Cells::Float32))
            .or_else(|| any.downcast_ref().map(Cells::Utf8))
    }

    fn push(&self, i: usize, chunk: &mut Vec<u8>) {
        let array: &dyn Array = match self {
            Self::Boolean(array) => *array,
            Self::UInt64(array) => *array,
            Self::Float32(array) => *array,
            Self::Utf8(array) => *array,
        };
        if array.is_null(i) {
            chunk.push(0);
            return;
        }
        chunk.push(1);
        match self {
            Self::Boolean(array) => chunk.push(u8::from(array.value(i))),
            Self::UInt64(array) => chunk.extend_from_slice(&array.value(i).to_le_bytes()),
            Self::Float32(array) => {
                chunk.extend_from_slice(&array.value(i).to_bits().to_le_bytes());
            }
            Self::Utf8(array) => push_text(chunk, array.value(i)),
        }
    }
}

fn push_text(chunk: &mut Vec<u8>, text: &str) {
    let length = u32::try_from(text.len()).expect("Arrow string offsets are 32-bit");
    chunk.extend_from_slice(&length.to_le_bytes());
    chunk.extend_from_slice(text.as_bytes());
}

impl LabelsHasher {
    pub(crate) fn new(annotation_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(VALUES_DOMAIN.as_bytes());
        hasher.update(b"\n");
        hasher.update(annotation_id.as_bytes());
        hasher.update(b"\n");
        Self {
            hasher,
            chunk: Vec::with_capacity(CHUNK_BYTES + 1024),
        }
    }

    /// Hash one batch: `ids[i]`, `states[i]` and `columns[c][i]` form row
    /// `i`. `None` when a column has a type a label table cannot hold or a
    /// key is null.
    pub(crate) fn update(
        &mut self,
        ids: &UInt64Array,
        states: &StringArray,
        columns: &[ArrayRef],
    ) -> Option<()> {
        if ids.null_count() != 0 || states.null_count() != 0 || ids.len() != states.len() {
            return None;
        }
        let cells = columns.iter().map(Cells::of).collect::<Option<Vec<_>>>()?;
        for i in 0..ids.len() {
            self.chunk.extend_from_slice(&ids.value(i).to_le_bytes());
            push_text(&mut self.chunk, states.value(i));
            for column in &cells {
                column.push(i, &mut self.chunk);
            }
            if self.chunk.len() >= CHUNK_BYTES {
                self.hasher.update(&self.chunk);
                self.chunk.clear();
            }
        }
        Some(())
    }

    pub(crate) fn finish(mut self) -> String {
        self.hasher.update(&self.chunk);
        crate::events::hex(&self.hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn digest(batches: &[(Vec<u64>, Vec<&str>, Vec<ArrayRef>)]) -> String {
        let mut hasher = LabelsHasher::new("annotation");
        for (ids, states, columns) in batches {
            hasher
                .update(
                    &UInt64Array::from(ids.clone()),
                    &StringArray::from(states.clone()),
                    columns,
                )
                .unwrap();
        }
        hasher.finish()
    }

    #[test]
    fn digest_does_not_depend_on_batches() {
        let gate: ArrayRef = Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)]));
        let score: ArrayRef = Arc::new(Float32Array::from(vec![Some(-0.0), None, Some(f32::NAN)]));
        let whole = digest(&[(
            vec![0, 1, 2],
            vec!["assessed", "qc_rejected", "assessed"],
            vec![Arc::clone(&gate), Arc::clone(&score)],
        )]);
        let split = digest(&[
            (
                vec![0],
                vec!["assessed"],
                vec![gate.slice(0, 1), score.slice(0, 1)],
            ),
            (
                vec![1, 2],
                vec!["qc_rejected", "assessed"],
                vec![gate.slice(1, 2), score.slice(1, 2)],
            ),
        ]);
        assert_eq!(whole, split);
    }

    // Column types are fixed by the descriptor, so only values of one type
    // need distinct encodings.
    #[test]
    fn null_false_and_signed_zero_are_distinct() {
        let row = |cell: ArrayRef| digest(&[(vec![0], vec!["assessed"], vec![cell])]);
        let distinct = |values: &[String]| {
            for (i, a) in values.iter().enumerate() {
                assert!(values[i + 1..].iter().all(|b| a != b));
            }
        };
        distinct(&[
            row(Arc::new(BooleanArray::from(vec![None::<bool>]))),
            row(Arc::new(BooleanArray::from(vec![Some(false)]))),
            row(Arc::new(BooleanArray::from(vec![Some(true)]))),
        ]);
        distinct(&[
            row(Arc::new(Float32Array::from(vec![None::<f32>]))),
            row(Arc::new(Float32Array::from(vec![Some(0.0_f32)]))),
            row(Arc::new(Float32Array::from(vec![Some(-0.0_f32)]))),
        ]);
        distinct(&[
            row(Arc::new(StringArray::from(vec![None::<&str>]))),
            row(Arc::new(StringArray::from(vec![Some("")]))),
            row(Arc::new(StringArray::from(vec![Some("a")]))),
        ]);
    }
}
