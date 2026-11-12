use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use cytiq_publish::{Existing, Publication};
use log::debug;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::error::AutospillError;
use crate::runner::{AutospillResult, AutospreadResult, MatrixKind};

/// Write the requested matrix format to `path`.
///
/// # Errors
///
/// Returns an error if the output format is unsupported or the matrix cannot be
/// written to the requested path.
pub fn export_matrix(
    result: &AutospillResult,
    kind: MatrixKind,
    path: impl AsRef<Path>,
    existing: Existing,
) -> Result<(), AutospillError> {
    let path = path.as_ref();
    let (row_header, row_labels, column_labels) = matrix_labels(result, kind);
    export_named_matrix(
        row_header,
        row_labels,
        column_labels,
        result.matrix(kind),
        path,
        existing,
    )
}

/// Write the requested spread matrix format to `path`.
///
/// # Errors
///
/// Returns an error if the output format is unsupported or the matrix cannot be
/// written to the requested path.
pub fn export_spread_matrix(
    result: &AutospreadResult,
    path: impl AsRef<Path>,
    existing: Existing,
) -> Result<(), AutospillError> {
    export_named_matrix(
        "dye",
        &result.dyes,
        &result.detectors,
        &result.spread_matrix,
        path,
        existing,
    )
}

pub(crate) fn export_named_matrix(
    row_header: &str,
    row_labels: &[String],
    column_labels: &[String],
    matrix: &[Vec<f64>],
    path: impl AsRef<Path>,
    existing: Existing,
) -> Result<(), AutospillError> {
    let path = path.as_ref();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| AutospillError::UnsupportedOutputFormat {
            path: path.to_path_buf(),
        })?;
    debug!(
        "exporting matrix with {} rows and {} columns to {}",
        row_labels.len(),
        column_labels.len(),
        path.display()
    );

    let delimiter = match extension.as_str() {
        "csv" => Some(b','),
        "tsv" => Some(b'\t'),
        "parquet" => None,
        _ => {
            return Err(AutospillError::UnsupportedOutputFormat {
                path: path.to_path_buf(),
            });
        }
    };
    let mut publication = Publication::new(existing);
    publication.stage_path(path, |staged| match delimiter {
        Some(delimiter) => write_delimited_matrix(
            row_header,
            row_labels,
            column_labels,
            matrix,
            staged,
            delimiter,
        ),
        None => write_parquet_matrix(row_header, row_labels, column_labels, matrix, staged),
    })?;
    Ok(publication.commit()?)
}

fn matrix_labels(
    result: &AutospillResult,
    kind: MatrixKind,
) -> (&'static str, &[String], &[String]) {
    if is_legacy_square_result(result) {
        return (
            "channel",
            &result.primary_channels,
            &result.primary_channels,
        );
    }

    match kind {
        MatrixKind::Spillover => ("dye", &result.dyes, &result.detectors),
        MatrixKind::Compensation => ("detector", &result.detectors, &result.dyes),
    }
}

fn is_legacy_square_result(result: &AutospillResult) -> bool {
    result.detectors.len() == result.primary_channels.len()
        && result
            .detectors
            .iter()
            .zip(&result.primary_channels)
            .all(|(detector, primary)| detector == primary)
}

fn write_delimited_matrix(
    row_header: &str,
    row_labels: &[String],
    column_labels: &[String],
    matrix: &[Vec<f64>],
    path: &Path,
    delimiter: u8,
) -> Result<(), AutospillError> {
    debug!(
        "writing delimited matrix to {} with delimiter {:?}",
        path.display(),
        char::from(delimiter)
    );
    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .map_err(|source| AutospillError::MatrixWrite {
            path: path.to_path_buf(),
            source,
        })?;

    let mut header = Vec::with_capacity(column_labels.len() + 1);
    header.push(row_header.to_owned());
    header.extend(column_labels.iter().cloned());
    writer
        .write_record(header)
        .map_err(|source| AutospillError::MatrixWrite {
            path: path.to_path_buf(),
            source,
        })?;

    for (row_name, row) in row_labels.iter().zip(matrix) {
        let mut record = Vec::with_capacity(row.len() + 1);
        record.push(row_name.clone());
        record.extend(row.iter().map(|value| format!("{value:.12}")));
        writer
            .write_record(record)
            .map_err(|source| AutospillError::MatrixWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer
        .flush()
        .map_err(|source| AutospillError::CreateOutputFile {
            path: path.to_path_buf(),
            source,
        })?;
    debug!("finished writing delimited matrix to {}", path.display());
    Ok(())
}

fn write_parquet_matrix(
    row_header: &str,
    row_labels: &[String],
    column_labels: &[String],
    matrix: &[Vec<f64>],
    path: &Path,
) -> Result<(), AutospillError> {
    debug!("writing parquet matrix to {}", path.display());
    let file = File::create(path).map_err(|source| AutospillError::CreateOutputFile {
        path: path.to_path_buf(),
        source,
    })?;
    let schema = Arc::new(Schema::new(
        std::iter::once(Field::new(row_header, DataType::Utf8, false))
            .chain(
                column_labels
                    .iter()
                    .map(|label| Field::new(label, DataType::Float64, false)),
            )
            .collect::<Vec<_>>(),
    ));

    let mut columns = Vec::<ArrayRef>::with_capacity(column_labels.len() + 1);
    columns.push(Arc::new(StringArray::from(row_labels.to_vec())));
    for column_index in 0..column_labels.len() {
        let values = matrix
            .iter()
            .map(|row| row[column_index])
            .collect::<Vec<_>>();
        columns.push(Arc::new(Float64Array::from(values)));
    }
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|error| {
        AutospillError::ParquetBatch {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;

    let properties = WriterProperties::builder().build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(properties)).map_err(|source| {
        AutospillError::ParquetWrite {
            path: path.to_path_buf(),
            source,
        }
    })?;
    writer
        .write(&batch)
        .map_err(|source| AutospillError::ParquetWrite {
            path: path.to_path_buf(),
            source,
        })?;
    writer
        .close()
        .map_err(|source| AutospillError::ParquetWrite {
            path: path.to_path_buf(),
            source,
        })?;

    debug!("finished writing parquet matrix to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::runner::{AutospillResult, ConvergenceSummary, MatrixKind};

    #[test]
    fn writes_square_csv_matrix() {
        let result = sample_square_result();
        let path = temp_path("matrix.csv");
        super::export_matrix(
            &result,
            MatrixKind::Compensation,
            &path,
            cytiq_publish::Existing::Refuse,
        )
        .expect("csv export should succeed");

        let output = fs::read_to_string(&path).expect("csv output should exist");
        assert!(output.starts_with("channel,FL1-A,FL2-A"));
        assert!(output.contains("FL1-A,1.020408163265"));
    }

    #[test]
    fn writes_rectangular_spillover_header() {
        let result = sample_rectangular_result();
        let path = temp_path("spillover.csv");
        super::export_matrix(
            &result,
            MatrixKind::Spillover,
            &path,
            cytiq_publish::Existing::Refuse,
        )
        .expect("rectangular csv export should succeed");

        let output = fs::read_to_string(&path).expect("csv output should exist");
        assert!(output.starts_with("dye,FL1-A,FL2-A,FL3-A"));
        assert!(output.contains("FITC,1.000000000000,0.200000000000,0.050000000000"));
    }

    fn sample_square_result() -> AutospillResult {
        AutospillResult {
            dyes: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
            detectors: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
            channels: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
            primary_channels: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
            spillover_matrix: vec![vec![1.0, 0.2], vec![0.1, 1.0]],
            compensation_matrix: vec![
                vec![1.020_408_163_265_306_1, -0.204_081_632_653_061_2],
                vec![-0.102_040_816_326_530_6, 1.020_408_163_265_306_1],
            ],
            residual_slope_error: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
            residual_skewness: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
            gate_stats: Vec::new(),
            convergence: ConvergenceSummary {
                converged: true,
                iterations: 2,
                final_delta_max: 0.0,
                steps: Vec::new(),
            },
        }
    }

    fn sample_rectangular_result() -> AutospillResult {
        AutospillResult {
            dyes: vec!["FITC".to_owned(), "PE".to_owned()],
            detectors: vec!["FL1-A".to_owned(), "FL2-A".to_owned(), "FL3-A".to_owned()],
            channels: vec!["FITC".to_owned(), "PE".to_owned()],
            primary_channels: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
            spillover_matrix: vec![vec![1.0, 0.2, 0.05], vec![0.1, 1.0, 0.03]],
            compensation_matrix: vec![vec![1.0, -0.2], vec![-0.1, 1.0], vec![0.0, 0.0]],
            residual_slope_error: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
            residual_skewness: vec![vec![0.0, 0.0], vec![0.0, 0.0]],
            gate_stats: Vec::new(),
            convergence: ConvergenceSummary {
                converged: true,
                iterations: 2,
                final_delta_max: 0.0,
                steps: Vec::new(),
            },
        }
    }

    fn temp_path(file_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("cytiq-autospill-{unique}-{file_name}"))
    }
}
