//! The 0–4 exit contract shared by every command (see
//! [`OutcomeReason`]): a failure is classified by walking its `source()`
//! chain until a known error type decides it; anything unknown is a runtime
//! error.

mod libraries;

use std::error::Error;

use cytiq_provenance::outcome::{CommandOutcome, OutcomeReason};

pub(crate) fn success(command: &str) -> CommandOutcome {
    CommandOutcome::new(command, true, OutcomeReason::Success, String::new())
}

/// A failed command: only failed checks count as completed, because the
/// command wrote its report before failing.
pub(crate) fn failure(command: &str, error: &(dyn Error + 'static)) -> CommandOutcome {
    let reason = classify(error);
    CommandOutcome::new(
        command,
        reason == OutcomeReason::ChecksFailed,
        reason,
        error.to_string(),
    )
}

pub(crate) fn classify(error: &(dyn Error + 'static)) -> OutcomeReason {
    std::iter::successors(Some(error), |&error| error.source())
        .find_map(libraries::known)
        .unwrap_or(OutcomeReason::RuntimeError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::PathBuf;

    fn code(error: &(dyn Error + 'static)) -> u8 {
        classify(error).exit_code()
    }

    fn io_error(kind: io::ErrorKind) -> io::Error {
        io::Error::new(kind, "test")
    }

    #[test]
    fn fcs_errors_separate_missing_files_corrupt_data_and_transient_io() {
        use cytiq_fcs::Error as Fcs;
        assert_eq!(code(&Fcs::Io(io_error(io::ErrorKind::NotFound))), 4);
        assert_eq!(code(&Fcs::Io(io_error(io::ErrorKind::UnexpectedEof))), 2);
        assert_eq!(code(&Fcs::Io(io_error(io::ErrorKind::PermissionDenied))), 1);
        assert_eq!(code(&Fcs::InvalidHeader("bad")), 2);
        assert_eq!(code(&Fcs::MissingKeyword("$PAR".into())), 2);
        assert_eq!(code(&Fcs::UnsupportedVersion("FCS9.9".into())), 2);
    }

    #[test]
    fn wrapped_fcs_errors_keep_their_classification() {
        let error = cytiq_inspect::stats::StatsError::FcsRead {
            path: PathBuf::from("a.fcs"),
            source: cytiq_fcs::Error::Io(io_error(io::ErrorKind::NotFound)),
        };
        assert_eq!(classify(&error), OutcomeReason::Incomplete);
    }

    #[test]
    fn io_errors_inside_parquet_and_arrow_keep_their_classification() {
        use arrow_schema::ArrowError;
        use cytiq_population::PopulationError as Population;
        use parquet::errors::ParquetError;
        let nested = |kind| ParquetError::External(Box::new(io_error(kind)));
        // A full disk is retryable, not invalid data.
        assert_eq!(
            code(&Population::Parquet(nested(io::ErrorKind::StorageFull))),
            1
        );
        assert_eq!(
            code(&Population::Parquet(nested(
                io::ErrorKind::PermissionDenied
            ))),
            1
        );
        let arrow = ArrowError::IoError("write".into(), io_error(io::ErrorKind::StorageFull));
        assert_eq!(code(&Population::Arrow(arrow)), 1);
        let mask = |kind| cytiq_mask::MaskError::Parquet {
            path: "run.masks.parquet".into(),
            source: nested(kind),
        };
        assert_eq!(code(&mask(io::ErrorKind::StorageFull)), 1);
        assert_eq!(code(&mask(io::ErrorKind::UnexpectedEof)), 2);
        // Corrupt or truncated content stays invalid.
        assert_eq!(
            code(&Population::Parquet(nested(io::ErrorKind::UnexpectedEof))),
            2
        );
        assert_eq!(
            code(&Population::Parquet(ParquetError::General(
                "corrupt".into()
            ))),
            2
        );
    }

    #[test]
    fn failed_checks_are_completed_with_code_three() {
        let error = cytiq_qc::QcError::Failed {
            failed: 2,
            report: PathBuf::from("qc.json"),
        };
        let outcome = failure("cytiq qc", &error);
        assert_eq!(outcome.exit_code, 3);
        assert!(outcome.completed);
        assert_eq!(outcome.reason, OutcomeReason::ChecksFailed);
        let flagged = cytiq_spillqc::SpillQcError::Flagged {
            pairs: 1,
            report: PathBuf::from("report.html"),
        };
        assert_eq!(code(&flagged), 3);
    }

    #[test]
    fn transparent_manifest_errors_are_invalid_input() {
        let error = cytiq_qc::QcError::Manifest(cytiq_manifest::ManifestError::Parse {
            path: PathBuf::from("run.yaml"),
            message: "bad".into(),
        });
        assert_eq!(code(&error), 2);
        let missing = cytiq_qc::QcError::Manifest(cytiq_manifest::ManifestError::Read {
            path: PathBuf::from("run.yaml"),
            source: io_error(io::ErrorKind::NotFound),
        });
        assert_eq!(code(&missing), 4);
    }

    #[test]
    fn csv_reads_distinguish_io_from_malformed_records() {
        let missing = cytiq_autospill::AutospillError::ManifestRead {
            path: PathBuf::from("controls.csv"),
            source: csv::Error::from(io_error(io::ErrorKind::NotFound)),
        };
        assert_eq!(code(&missing), 4);
        let malformed = csv::ReaderBuilder::new()
            .from_reader("a,b\n1\n".as_bytes())
            .records()
            .find_map(Result::err)
            .expect("unequal record lengths");
        let malformed = cytiq_autospill::AutospillError::ManifestRead {
            path: PathBuf::from("controls.csv"),
            source: malformed,
        };
        assert_eq!(code(&malformed), 2);
    }

    #[test]
    fn existing_outputs_conflict_and_invalid_values_are_input_errors() {
        let exists =
            cytiq_transform::TransformError::Publish(cytiq_publish::PublishError::Exists {
                path: PathBuf::from("out.fcs"),
            });
        assert_eq!(classify(&exists), OutcomeReason::OutputConflict);
        assert_eq!(code(&cytiq_inspect::stats::StatsError::InvalidBins(1)), 2);
        assert_eq!(code(&io_error(io::ErrorKind::InvalidInput)), 2);
    }

    #[derive(Debug)]
    struct Wrapper(cytiq_inspect::stats::StatsError);

    impl std::fmt::Display for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("wrapper")
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn unknown_errors_are_runtime_errors_unless_their_source_is_known() {
        let opaque: Box<dyn Error> = "something broke".into();
        assert_eq!(code(opaque.as_ref()), 1);
        assert_eq!(
            code(&Wrapper(cytiq_inspect::stats::StatsError::InvalidBins(0))),
            2
        );
    }

    #[test]
    fn success_is_completed_with_code_zero() {
        let outcome = success("cytiq meta");
        assert!(outcome.completed);
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.artifacts.len(), 0);
    }
}
