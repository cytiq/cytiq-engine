use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum QcError {
    #[error(transparent)]
    Manifest(#[from] cytiq_manifest::ManifestError),
    #[error("sample `{sample}` is not in manifest `{manifest}`")]
    UnknownSample { sample: String, manifest: PathBuf },
    #[error("failed to read sample FCS `{path}`: {source}")]
    FcsRead {
        path: PathBuf,
        #[source]
        source: cytiq_fcs::Error,
    },
    #[error(transparent)]
    Mask(#[from] cytiq_mask::MaskError),
    #[error("output `{path}` already exists; pass --overwrite to replace it")]
    OutputExists { path: PathBuf },
    #[error("failed to write QC report `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("qc failed: {failed} check(s) failed; see `{report}`")]
    Failed { failed: usize, report: PathBuf },
    #[error("{0}")]
    Publish(#[from] cytiq_publish::PublishError),
    #[error("failed to read artifact truth `{path}`: {source}")]
    ArtifactTruth {
        path: PathBuf,
        #[source]
        source: crate::truth::TruthError,
    },
}
