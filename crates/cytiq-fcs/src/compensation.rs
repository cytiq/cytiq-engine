//! Spillover metadata parsing and fluorescence compensation helpers.

use crate::error::FcsError;
use crate::types::{ColumnData, FcsDataset, Parameter};

const SPILLOVER_KEYWORDS: [&str; 4] = ["$SPILL", "SPILL", "$SPILLOVER", "SPILLOVER"];

/// Whether `key` holds a spillover matrix (`$SPILL`, `SPILL`, `$SPILLOVER`,
/// `SPILLOVER`, any case).
#[must_use]
pub fn is_spillover_keyword(key: &str) -> bool {
    SPILLOVER_KEYWORDS
        .iter()
        .any(|candidate| key.eq_ignore_ascii_case(candidate))
}

/// Square spillover matrix with the detector channels it applies to.
///
/// `matrix` is stored row-major: entry `(row, column)` is the fraction of the
/// `row` fluorochrome signal observed in the `column` detector.
#[derive(Clone, Debug, PartialEq)]
pub struct Spillover {
    channels: Vec<String>,
    matrix: Vec<f64>,
}

impl Spillover {
    /// Build a spillover matrix from channel names and row-major values.
    ///
    /// # Errors
    ///
    /// Returns an error when there are no channels, the value count is not
    /// `channels.len()²`, or any value is non-finite.
    pub fn from_parts(channels: Vec<String>, matrix: Vec<f64>) -> Result<Self, FcsError> {
        if channels.is_empty() {
            return Err(FcsError::InvalidSpillover(
                "spillover channel count must be greater than zero".to_owned(),
            ));
        }
        let size = channels.len();
        if matrix.len() != size * size {
            return Err(FcsError::InvalidSpillover(format!(
                "expected {} spillover values for {size} channels, found {}",
                size * size,
                matrix.len()
            )));
        }
        if matrix.iter().any(|value| !value.is_finite()) {
            return Err(FcsError::InvalidSpillover(
                "spillover matrix contains non-finite values".to_owned(),
            ));
        }
        Ok(Self { channels, matrix })
    }

    /// Parse the `$SPILL`/`SPILL`/`$SPILLOVER`/`SPILLOVER` keyword of a dataset.
    ///
    /// Returns `Ok(None)` when none of the keywords is present.
    ///
    /// # Errors
    ///
    /// Returns an error when the keyword value is malformed.
    pub fn from_dataset(dataset: &FcsDataset) -> Result<Option<Self>, FcsError> {
        dataset
            .text
            .iter()
            .find_map(|(key, value)| is_spillover_keyword(key).then_some(value))
            .map(Self::parse_keyword)
            .transpose()
    }

    /// Parse a spillover keyword value (`n,ch1,...,chn,v11,...,vnn`).
    ///
    /// # Errors
    ///
    /// Returns an error when the value is malformed.
    pub fn parse_keyword(raw: &str) -> Result<Self, FcsError> {
        let tokens = raw
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let Some(first) = tokens.first() else {
            return Err(FcsError::InvalidSpillover(
                "spillover value is empty".to_owned(),
            ));
        };
        let size = first.parse::<usize>().map_err(|error| {
            FcsError::InvalidSpillover(format!(
                "invalid spillover channel count `{first}`: {error}"
            ))
        })?;
        if size == 0 {
            return Err(FcsError::InvalidSpillover(
                "spillover channel count must be greater than zero".to_owned(),
            ));
        }
        let expected_len = size
            .checked_mul(size)
            .and_then(|values| values.checked_add(size))
            .and_then(|values| values.checked_add(1));
        if expected_len != Some(tokens.len()) {
            return Err(FcsError::InvalidSpillover(match expected_len {
                Some(expected_len) => format!(
                    "expected {expected_len} comma-separated values for {size} channels, found {}",
                    tokens.len()
                ),
                None => format!("spillover channel count {size} is too large"),
            }));
        }
        let channels = tokens[1..=size]
            .iter()
            .map(|channel| (*channel).to_owned())
            .collect::<Vec<_>>();
        let values = tokens[(1 + size)..]
            .iter()
            .map(|token| {
                token.parse::<f64>().map_err(|error| {
                    FcsError::InvalidSpillover(format!(
                        "invalid spillover value `{token}`: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_parts(channels, values)
    }

    #[must_use]
    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    /// Row-major spillover values.
    #[must_use]
    pub fn matrix(&self) -> &[f64] {
        &self.matrix
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.channels.len()
    }

    /// Row-major compensation matrix, the inverse of the spillover matrix.
    ///
    /// # Errors
    ///
    /// Returns [`FcsError::SingularMatrix`] when the matrix cannot be inverted.
    pub fn compensation_matrix(&self) -> Result<Vec<f64>, FcsError> {
        invert_matrix(&self.matrix, self.size())
    }
}

/// Invert a row-major `size × size` matrix with Gauss-Jordan elimination and
/// partial pivoting.
///
/// # Errors
///
/// Returns [`FcsError::SingularMatrix`] when the matrix is singular, or
/// [`FcsError::InvalidSpillover`] when `matrix` does not hold `size²` values.
#[allow(clippy::cast_precision_loss)]
pub fn invert_matrix(matrix: &[f64], size: usize) -> Result<Vec<f64>, FcsError> {
    match size.checked_mul(size) {
        Some(expected) if expected == matrix.len() => {}
        Some(expected) => {
            return Err(FcsError::InvalidSpillover(format!(
                "expected {expected} matrix values, found {}",
                matrix.len()
            )));
        }
        None => {
            return Err(FcsError::InvalidSpillover(format!(
                "matrix size {size} is too large"
            )));
        }
    }
    let scale = matrix
        .iter()
        .fold(0.0_f64, |max, value| max.max(value.abs()));
    let tolerance = f64::EPSILON * scale.max(1.0) * size as f64;
    let mut work = matrix.to_vec();
    let mut inverse = vec![0.0; size * size];
    for index in 0..size {
        inverse[index * size + index] = 1.0;
    }

    for column in 0..size {
        let pivot_row = (column..size)
            .max_by(|left, right| {
                work[left * size + column]
                    .abs()
                    .total_cmp(&work[right * size + column].abs())
            })
            .unwrap_or(column);
        let pivot = work[pivot_row * size + column];
        if pivot.abs() <= tolerance {
            return Err(FcsError::SingularMatrix);
        }
        if pivot_row != column {
            for offset in 0..size {
                work.swap(pivot_row * size + offset, column * size + offset);
                inverse.swap(pivot_row * size + offset, column * size + offset);
            }
        }
        for offset in 0..size {
            work[column * size + offset] /= pivot;
            inverse[column * size + offset] /= pivot;
        }
        for row in 0..size {
            if row == column {
                continue;
            }
            let factor = work[row * size + column];
            if factor == 0.0 {
                continue;
            }
            for offset in 0..size {
                work[row * size + offset] -= factor * work[column * size + offset];
                inverse[row * size + offset] -= factor * inverse[column * size + offset];
            }
        }
    }

    Ok(inverse)
}

/// 1-norm condition number `‖A‖₁·‖A⁻¹‖₁` of a row-major `size × size`
/// matrix: how much relative errors in raw values can grow through
/// compensation.
///
/// # Errors
///
/// Returns [`FcsError::SingularMatrix`] when the matrix cannot be inverted.
pub fn condition_number_1(matrix: &[f64], size: usize) -> Result<f64, FcsError> {
    let inverse = invert_matrix(matrix, size)?;
    let norm_1 = |values: &[f64]| {
        (0..size)
            .map(|column| {
                (0..size)
                    .map(|row| values[row * size + column].abs())
                    .sum::<f64>()
            })
            .fold(0.0_f64, f64::max)
    };
    Ok(norm_1(matrix) * norm_1(&inverse))
}

/// Multiply event rows by a row-major compensation matrix.
///
/// `columns[i][event]` is the raw value of channel `i`; the result has the same
/// shape and holds `Σ_i columns[i][event] * compensation[i, j]` for output
/// channel `j`.
///
/// # Panics
///
/// Panics when `compensation` does not hold `columns.len()²` values or the
/// columns differ in length.
#[must_use]
pub fn compensate(columns: &[Vec<f64>], compensation: &[f64]) -> Vec<Vec<f64>> {
    let size = columns.len();
    assert_eq!(
        compensation.len(),
        size * size,
        "compensation matrix must be square over the input columns"
    );
    let event_count = columns.first().map_or(0, Vec::len);
    assert!(
        columns.iter().all(|column| column.len() == event_count),
        "compensation input columns must have equal length"
    );

    let mut output = vec![vec![0.0; event_count]; size];
    for event_index in 0..event_count {
        for (output_index, output_column) in output.iter_mut().enumerate() {
            output_column[event_index] = (0..size)
                .map(|input_index| {
                    columns[input_index][event_index]
                        * compensation[input_index * size + output_index]
                })
                .sum();
        }
    }
    output
}

impl ColumnData {
    /// Read one value with the same conversion as [`Self::to_f64_vec`], without
    /// allocating a converted column. Integer values above 2^53 have the same
    /// f64 rounding as the full conversion. Out-of-range rows return `None`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn get_f64(&self, row: usize) -> Option<f64> {
        match self {
            Self::UInt(values) => values.get(row).map(|&value| value as f64),
            Self::F32(values) => values.get(row).map(|&value| f64::from(value)),
            Self::F64(values) => values.get(row).copied(),
        }
    }

    /// Convert the column to `f64` values.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_f64_vec(&self) -> Vec<f64> {
        match self {
            Self::UInt(values) => values.iter().map(|value| *value as f64).collect(),
            Self::F32(values) => values.iter().map(|value| f64::from(*value)).collect(),
            Self::F64(values) => values.clone(),
        }
    }
}

impl FcsDataset {
    /// Number of events in the dataset.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.data.columns.first().map_or(0, ColumnData::len)
    }

    /// Find a parameter index by `$PnN`, then `$PnS`, then `"$PnN [$PnS]"`.
    #[must_use]
    pub fn find_parameter(&self, requested: &str) -> Option<usize> {
        find_parameter_index(&self.parameters, requested)
    }
}

/// Find a parameter index by `$PnN`, then `$PnS`, then `"$PnN [$PnS]"`.
#[must_use]
pub fn find_parameter_index(parameters: &[Parameter], requested: &str) -> Option<usize> {
    let requested = requested.trim();
    parameters
        .iter()
        .position(|parameter| parameter.name == requested)
        .or_else(|| {
            parameters
                .iter()
                .position(|parameter| parameter.label.as_deref() == Some(requested))
        })
        .or_else(|| {
            parameters
                .iter()
                .position(|parameter| parameter.display_identifier() == requested)
        })
}

impl Parameter {
    /// `"$PnN [$PnS]"` when a label is present, otherwise `$PnN`.
    #[must_use]
    pub fn display_identifier(&self) -> String {
        self.label.as_ref().map_or_else(
            || self.name.clone(),
            |label| format!("{} [{}]", self.name, label),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scalar_access_matches_full_conversion_including_precision_and_signed_zero() {
        for column in [
            super::ColumnData::UInt(vec![0, 16_777_217, (1_u64 << 53) + 1, u64::MAX]),
            super::ColumnData::F32(vec![-0.0, 1.25, f32::INFINITY]),
            super::ColumnData::F64(vec![-0.0, 1.25, f64::MIN_POSITIVE]),
        ] {
            let expected = column.to_f64_vec();
            for (i, value) in expected.iter().enumerate() {
                assert_eq!(column.get_f64(i).unwrap().to_bits(), value.to_bits());
            }
            assert_eq!(column.get_f64(column.len()), None);
        }
    }
    use super::*;

    fn assert_close(left: &[f64], right: &[f64]) {
        assert_eq!(left.len(), right.len());
        for (a, b) in left.iter().zip(right) {
            assert!((a - b).abs() < 1e-12, "{a} != {b}");
        }
    }

    #[test]
    fn condition_number_of_identity_is_one_and_grows_with_spillover() {
        let identity = [1.0, 0.0, 0.0, 1.0];
        assert!((super::condition_number_1(&identity, 2).unwrap() - 1.0).abs() < 1e-12);
        // [[1, 0.9], [0.9, 1]]: ‖A‖₁ = 1.9, ‖A⁻¹‖₁ = 1.9 / 0.19 = 10.
        let spill = [1.0, 0.9, 0.9, 1.0];
        assert!((super::condition_number_1(&spill, 2).unwrap() - 19.0).abs() < 1e-9);
        assert!(super::condition_number_1(&[1.0, 1.0, 1.0, 1.0], 2).is_err());
    }

    #[test]
    fn parses_spillover_keyword() {
        let spillover = Spillover::parse_keyword("2,FL1-A,FL2-A,1,0.1,0.2,1").unwrap();
        assert_eq!(spillover.channels(), ["FL1-A", "FL2-A"]);
        assert_eq!(spillover.matrix(), [1.0, 0.1, 0.2, 1.0]);
    }

    #[test]
    fn rejects_spillover_with_wrong_value_count() {
        let error = Spillover::parse_keyword("2,FL1-A,FL2-A,1,0.1,0.2").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected 7 comma-separated values")
        );
    }

    #[test]
    fn rejects_zero_channel_spillover() {
        assert!(Spillover::parse_keyword("0").is_err());
    }

    #[test]
    fn inverse_round_trips_to_identity() {
        let matrix = [1.0, 0.1, 0.05, 0.2, 1.0, 0.3, 0.0, 0.15, 1.0];
        let inverse = invert_matrix(&matrix, 3).unwrap();
        let mut product = vec![0.0; 9];
        for row in 0..3 {
            for column in 0..3 {
                product[row * 3 + column] = (0..3)
                    .map(|k| matrix[row * 3 + k] * inverse[k * 3 + column])
                    .sum();
            }
        }
        assert_close(&product, &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn inverse_handles_zero_leading_pivot() {
        let inverse = invert_matrix(&[0.0, 1.0, 1.0, 0.0], 2).unwrap();
        assert_close(&inverse, &[0.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn fails_when_matrix_is_singular() {
        let error = invert_matrix(&[1.0, 2.0, 2.0, 4.0], 2).unwrap_err();
        assert!(matches!(error, FcsError::SingularMatrix));
    }

    #[test]
    fn compensation_undoes_spillover() {
        let spillover = Spillover::parse_keyword("2,A,B,1,0.25,0.1,1").unwrap();
        let true_signal = [vec![100.0, 0.0], vec![0.0, 50.0]];
        // Observed = true · S.
        let observed = compensate(&true_signal, spillover.matrix());
        let recovered = compensate(&observed, &spillover.compensation_matrix().unwrap());
        assert_close(&recovered[0], &true_signal[0]);
        assert_close(&recovered[1], &true_signal[1]);
    }
}
