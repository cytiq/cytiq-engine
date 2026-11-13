//! Compact per-event flags for lineage checks, sized by the raw event count
//! and allocated fallibly so an oversized acquisition fails instead of
//! aborting.

use crate::{PopulationError, Result};

pub(crate) struct Bitmap {
    words: Vec<u64>,
    len: usize,
}

impl Bitmap {
    /// `size` cleared flags.
    ///
    /// # Errors
    /// Returns an error when the flags do not fit in memory.
    pub(crate) fn new(size: usize) -> Result<Self> {
        let mut words = Vec::new();
        let count = size.div_ceil(64);
        words
            .try_reserve_exact(count)
            .map_err(|_| PopulationError::Invalid("lineage bitmap exceeds memory budget".into()))?;
        words.resize(count, 0_u64);
        Ok(Self { words, len: size })
    }

    pub(crate) fn get(&self, index: usize) -> bool {
        self.words[index / 64] & (1_u64 << (index % 64)) != 0
    }

    pub(crate) fn set(&mut self, index: usize, value: bool) {
        let bit = 1_u64 << (index % 64);
        if value {
            self.words[index / 64] |= bit;
        } else {
            self.words[index / 64] &= !bit;
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Whether every flag is set.
    pub(crate) fn is_full(&self) -> bool {
        (0..self.len).all(|index| self.get(index))
    }
}

#[cfg(test)]
mod tests {
    use super::Bitmap;

    #[test]
    fn flags_are_set_and_cleared_independently() {
        let mut flags = Bitmap::new(130).unwrap();
        flags.set(129, true);
        flags.set(64, true);
        flags.set(64, false);
        assert!(flags.get(129) && !flags.get(64) && !flags.get(0));
        assert!(!flags.is_full());
        for index in 0..130 {
            flags.set(index, true);
        }
        assert!(flags.is_full());
        assert_eq!(flags.len(), 130);
    }
}
