//! One exhaustive match per library error: a new variant does not compile
//! until it is classified here. Variants are grouped by reason.
//!
//! Rules of thumb: a missing input file, glob or directory is incomplete
//! input (4); an existing output is a conflict (4); malformed data,
//! unsupported input and deterministic algorithm failures on that input are
//! invalid input (2); failed checks after the report was written are 3;
//! internal faults, thread pools and writes that fail for no input-related
//! reason are runtime errors (1). I/O follows its kind.

use std::error::Error;

use cytiq_provenance::ProvenanceError;
use cytiq_provenance::outcome::OutcomeReason;
use cytiq_publish::PublishError;

const INVALID: OutcomeReason = OutcomeReason::InvalidConfiguration;
const MISSING: OutcomeReason = OutcomeReason::Incomplete;
const CONFLICT: OutcomeReason = OutcomeReason::OutputConflict;
const RUNTIME: OutcomeReason = OutcomeReason::RuntimeError;
const CHECKS: OutcomeReason = OutcomeReason::ChecksFailed;

/// The classification of `error` itself, or `None` when its type is unknown
/// (the caller then tries its source).
pub(super) fn known(error: &(dyn Error + 'static)) -> Option<OutcomeReason> {
    macro_rules! classify_as {
        ($($type:ty => $rule:expr),+ $(,)?) => {
            $(if let Some(error) = error.downcast_ref::<$type>() {
                return Some($rule(error));
            })+
        };
    }
    classify_as! {
        std::io::Error => OutcomeReason::of_io,
        cytiq_fcs::Error => fcs,
        PublishError => OutcomeReason::of_publish,
        ProvenanceError => ProvenanceError::outcome_reason,
        cytiq_manifest::ManifestError => manifest,
        cytiq_mask::MaskError => mask,
        csv::Error => |error| csv(error, INVALID),
        serde_json::Error => |error| json(error, RUNTIME),
        cytiq_autospectral::AutoSpectralError => autospectral,
        cytiq_autospill::AutospillError => autospill,
        cytiq_classify::ClassifyError => classify,
        cytiq_clean::CleanError => clean,
        cytiq_compensate::CompensateError => compensate,
        cytiq_cytonorm::CytoNormError => cytonorm,
        cytiq_cytoscan::CytoScanError => cytoscan,
        cytiq_flowsom::FlowSomError => flowsom,
        cytiq_gating::GatingError => gating,
        cytiq_inspect::meta::MetaError => meta,
        cytiq_peacoqc::PeacoQcError => peacoqc,
        cytiq_plot::PlotError => plot,
        cytiq_qc::preflight::PreflightError => preflight,
        cytiq_qc::QcError => qc,
        cytiq_spillqc::SpillQcError => spillqc,
        cytiq_inspect::stats::StatsError => stats,
        cytiq_transform::TransformError => transform,
    }
    None
}

fn io(error: &std::io::Error) -> OutcomeReason {
    OutcomeReason::of_io(error)
}

/// Every FCS error but I/O describes the file's content. A truncated HEADER
/// surfaces as `UnexpectedEof`, which `of_io` already treats as invalid.
fn fcs(error: &cytiq_fcs::Error) -> OutcomeReason {
    use cytiq_fcs::Error as E;
    match error {
        E::Io(source) => io(source),
        E::UnsupportedVersion { .. }
        | E::InvalidHeader { .. }
        | E::InvalidUtf8 { .. }
        | E::InvalidText { .. }
        | E::MissingKeyword { .. }
        | E::DuplicateKeyword { .. }
        | E::InvalidKeywordValue { .. }
        | E::UnsupportedMode { .. }
        | E::UnsupportedByteOrder { .. }
        | E::UnsupportedWidth { .. }
        | E::UnsupportedLayout { .. }
        | E::OffsetMismatch { .. }
        | E::SegmentOutOfBounds { .. }
        | E::SegmentOverlap { .. }
        | E::LimitExceeded { .. }
        | E::DataLengthMismatch { .. }
        | E::EventValueParse { .. }
        | E::InvalidSpillover { .. }
        | E::SingularMatrix
        | E::InvalidWriterInput { .. }
        | E::LogAmplified { .. } => INVALID,
    }
}

/// `decode` applies to errors other than I/O: invalid input when reading,
/// a runtime error when writing.
fn csv(error: &csv::Error, decode: OutcomeReason) -> OutcomeReason {
    match error.kind() {
        csv::ErrorKind::Io(source) => io(source),
        _ => decode,
    }
}

fn json(error: &serde_json::Error, decode: OutcomeReason) -> OutcomeReason {
    error
        .io_error_kind()
        .map_or(decode, OutcomeReason::of_io_kind)
}

fn manifest(error: &cytiq_manifest::ManifestError) -> OutcomeReason {
    use cytiq_manifest::ManifestError as E;
    match error {
        E::Read { source, .. } => io(source),
        E::UnsupportedFormat { .. } | E::Parse { .. } | E::Invalid { .. } => INVALID,
    }
}

fn mask(error: &cytiq_mask::MaskError) -> OutcomeReason {
    use cytiq_mask::MaskError as E;
    match error {
        E::Io { source, .. } => io(source),
        E::Lineage(source) => fcs(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::OutputExists { .. } => CONFLICT,
        E::Parquet { .. }
        | E::Invalid { .. }
        | E::LengthMismatch { .. }
        | E::Empty
        | E::MissingEvent { .. }
        | E::MissingAcquisition { .. }
        | E::InvalidAcquisition { .. }
        | E::AcquisitionMismatch { .. } => INVALID,
    }
}

fn qc(error: &cytiq_qc::QcError) -> OutcomeReason {
    use cytiq_qc::QcError as E;
    match error {
        E::Manifest(source) => manifest(source),
        E::FcsRead { source, .. } => fcs(source),
        E::Mask(source) => mask(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::Write { source, .. } => io(source),
        E::OutputExists { .. } => CONFLICT,
        E::UnknownSample { .. } => INVALID,
        E::Failed { .. } => CHECKS,
    }
}

fn preflight(error: &cytiq_qc::preflight::PreflightError) -> OutcomeReason {
    use cytiq_qc::preflight::PreflightError as E;
    match error {
        E::Manifest(source) => manifest(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::Write { source, .. } => io(source),
        E::OutputExists { .. } => CONFLICT,
        E::Failed { .. } => CHECKS,
    }
}

fn spillqc(error: &cytiq_spillqc::SpillQcError) -> OutcomeReason {
    use cytiq_spillqc::SpillQcError as E;
    match error {
        E::MissingInput { .. } | E::EmptyDirectory { .. } | E::EmptyGlob { .. } => MISSING,
        E::OutputExists { .. } | E::OutputNotDirectory { .. } => CONFLICT,
        E::DirectoryRead { source, .. }
        | E::InputCanonicalize { source, .. }
        | E::Write { source, .. } => io(source),
        E::FcsRead { source, .. } => fcs(source),
        E::Matrix(source) => compensate(source),
        E::Csv { source, .. } => csv(source, RUNTIME),
        E::Json { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::Report { .. } => RUNTIME,
        E::Flagged { .. } => CHECKS,
        E::EmptyInputs
        | E::NonFcsInput { .. }
        | E::UnsupportedGlob { .. }
        | E::DuplicateInput { .. }
        | E::DuplicateStem { .. }
        | E::InvalidParameter { .. }
        | E::AlreadyCompensated { .. }
        | E::UnmixedInput { .. }
        | E::TransformedInput { .. } => INVALID,
    }
}

fn stats(error: &cytiq_inspect::stats::StatsError) -> OutcomeReason {
    use cytiq_inspect::stats::StatsError as E;
    match error {
        E::FcsRead { source, .. } => fcs(source),
        E::Hash { source, .. } | E::OutputWrite { source, .. } => io(source),
        E::OutputExists { .. } => CONFLICT,
        E::InvalidBins { .. } => INVALID,
        E::Serialize(source) => json(source, RUNTIME),
    }
}

fn meta(error: &cytiq_inspect::meta::MetaError) -> OutcomeReason {
    use cytiq_inspect::meta::MetaError as E;
    match error {
        E::FcsRead { source, .. } => fcs(source),
        E::Hash { source, .. } | E::OutputWrite { source, .. } => io(source),
        E::OutputExists { .. } => CONFLICT,
        E::Serialize(source) => json(source, RUNTIME),
    }
}

fn transform(error: &cytiq_transform::TransformError) -> OutcomeReason {
    use cytiq_transform::TransformError as E;
    match error {
        E::MissingInput { .. } | E::EmptyGlob { .. } | E::EmptyDirectory { .. } => MISSING,
        E::OutputExists { .. }
        | E::MetadataExists { .. }
        | E::SameInputOutput { .. }
        | E::MultipleInputsOutputFile { .. } => CONFLICT,
        E::DirectoryRead { source, .. }
        | E::InputCanonicalize { source, .. }
        | E::MetadataDirectoryCreate { source, .. }
        | E::MetadataWrite { source, .. }
        | E::DebugDirectoryCreate { source, .. }
        | E::CsvFlush { source, .. }
        | E::DebugJsonWrite { source, .. }
        | E::HtmlWrite { source, .. } => io(source),
        E::FcsRead { source, .. } | E::FcsWrite { source, .. } => fcs(source),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::MetadataSerialize { source, .. } | E::DebugJsonSerialize { source, .. } => {
            json(source, RUNTIME)
        }
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::NonFiniteOutputValues { .. } => RUNTIME,
        E::EmptyInputs
        | E::NonFcsInput { .. }
        | E::UnsupportedGlob { .. }
        | E::DuplicateInput { .. }
        | E::UnsupportedFormat { .. }
        | E::InvalidPositiveParameter { .. }
        | E::InvalidNonNegativeParameter { .. }
        | E::InvalidFiniteParameter { .. }
        | E::InvalidTransformContract { .. }
        | E::UnsupportedTransformContract { .. }
        | E::InvalidScaleParameters { .. }
        | E::AlreadyTransformed { .. }
        | E::UncompensatedInput { .. }
        | E::EmptyFcs { .. }
        | E::EmptyChannelList
        | E::DuplicateChannel { .. }
        | E::MissingChannel { .. }
        | E::NoTransformableChannels { .. }
        | E::ReservedChannel { .. }
        | E::NonFiniteChannel { .. }
        | E::DuplicateOutput { .. }
        | E::NonFcsOutput { .. } => INVALID,
    }
}

fn compensate(error: &cytiq_compensate::CompensateError) -> OutcomeReason {
    use cytiq_compensate::CompensateError as E;
    match error {
        E::MissingInput { .. } => MISSING,
        E::OutputExists { .. } => CONFLICT,
        E::FcsRead { source, .. } | E::FcsWrite { source, .. } | E::Spillover { source, .. } => {
            fcs(source)
        }
        E::MatrixRead { source, .. } => csv(source, INVALID),
        E::Mask(source) => mask(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::UnsupportedOutputFormat { .. }
        | E::EmptyInput { .. }
        | E::MissingSpillover { .. }
        | E::AlreadyCompensated { .. }
        | E::TransformedInput { .. }
        | E::UnmixedInput { .. }
        | E::MissingChannel { .. }
        | E::InvalidMatrix { .. } => INVALID,
    }
}

fn gating(error: &cytiq_gating::GatingError) -> OutcomeReason {
    use cytiq_gating::GatingError as E;
    match error {
        E::FcsRead { source, .. } | E::Compensation { source, .. } | E::SignalState(source) => {
            fcs(source)
        }
        E::GatingMlRead { source, .. } | E::OutputWrite { source, .. } => io(source),
        E::Mask(source) => mask(source),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::JsonWrite { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::Xml { .. }
        | E::InvalidGatingMl { .. }
        | E::UnsupportedElement { .. }
        | E::DuplicateGate { .. }
        | E::UnknownGateRef { .. }
        | E::UnknownTransform { .. }
        | E::UnknownCompensation { .. }
        | E::CyclicHierarchy { .. }
        | E::UnknownGate { .. }
        | E::MissingDimension { .. }
        | E::InvalidTransform { .. }
        | E::InvalidGate { .. }
        | E::MaskLength { .. }
        | E::ReservedGateId { .. }
        | E::AlreadyProcessed { .. } => INVALID,
    }
}

fn clean(error: &cytiq_clean::CleanError) -> OutcomeReason {
    use cytiq_clean::CleanError as E;
    match error {
        E::FcsRead { source, .. } => fcs(source),
        E::Mask(source) => mask(source),
        E::Gating(source) => gating(source),
        E::GatingMlRead { source, .. } | E::OutputWrite { source, .. } => io(source),
        E::JsonWrite { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::ParquetWrite { .. } => RUNTIME,
        E::MissingChannel { .. }
        | E::AmbiguousViabilityChannel { .. }
        | E::RequiredFilterNotApplied { .. }
        | E::TransformedViability { .. }
        | E::TransformedScatter { .. }
        | E::InvalidConfig { .. } => INVALID,
    }
}

fn peacoqc(error: &cytiq_peacoqc::PeacoQcError) -> OutcomeReason {
    use cytiq_peacoqc::PeacoQcError as E;
    match error {
        E::MissingInputFile { .. } => MISSING,
        E::FcsRead { source, .. } | E::FcsWrite { source, .. } => fcs(source),
        E::DebugDirectoryCreate { source, .. }
        | E::CsvFlush { source, .. }
        | E::JsonWrite { source, .. }
        | E::HtmlWrite { source, .. } => io(source),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::JsonSerialize { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::NonFiniteOutputValues { .. } | E::ParquetWrite { .. } => RUNTIME,
        E::EmptyInput { .. }
        | E::EmptyChannels
        | E::DuplicateChannel { .. }
        | E::MissingChannel { .. }
        | E::NonFiniteChannel { .. }
        | E::AllZeroChannel { .. }
        | E::UnknownMargins { .. }
        | E::AllEventsRemoved { .. }
        | E::NoPeakFeatures { .. }
        | E::UnsupportedOutputFormat { .. } => INVALID,
    }
}

fn flowsom(error: &cytiq_flowsom::FlowSomError) -> OutcomeReason {
    use cytiq_flowsom::FlowSomError as E;
    match error {
        E::MissingInputFile { .. } => MISSING,
        E::Io(source)
        | E::DebugDirectoryCreate { source, .. }
        | E::CsvFlush { source, .. }
        | E::JsonWrite { source, .. }
        | E::HtmlWrite { source, .. } => io(source),
        E::ModelJson(source) => json(source, INVALID),
        E::FcsRead { source, .. } => fcs(source),
        E::Mask(source) => mask(source),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::JsonSerialize { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::InvalidConfig { .. }
        | E::EmptyInput { .. }
        | E::NoMaskedEvents { .. }
        | E::EmptyChannels
        | E::DuplicateChannel { .. }
        | E::MissingChannel { .. }
        | E::NonFiniteChannel { .. } => INVALID,
    }
}

fn cytoscan(error: &cytiq_cytoscan::CytoScanError) -> OutcomeReason {
    use cytiq_cytoscan::CytoScanError as E;
    match error {
        E::MissingInput { .. }
        | E::EmptyGlob { .. }
        | E::EmptyDirectory { .. }
        | E::EmptyReference => MISSING,
        E::DirectoryRead { source, .. }
        | E::InputCanonicalize { source, .. }
        | E::OutputDirectoryCreate { source, .. }
        | E::DebugDirectoryCreate { source, .. }
        | E::CsvFlush { source, .. }
        | E::JsonWrite { source, .. }
        | E::HtmlWrite { source, .. } => io(source),
        E::FcsRead { source, .. } => fcs(source),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::JsonSerialize { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::EmptyInputs
        | E::MissingSpillover { .. }
        | E::ChannelNotCompensated { .. }
        | E::ProcessedInput { .. }
        | E::NonFcsInput { .. }
        | E::UnsupportedGlob { .. }
        | E::DuplicateInput { .. }
        | E::EmptyChannels
        | E::EmptyChannelList
        | E::DuplicateChannel { .. }
        | E::MissingChannel { .. }
        | E::NonFiniteChannel { .. }
        | E::EmptyFcs { .. }
        | E::MissingReference { .. }
        | E::InvalidSpillover { .. }
        | E::SingularSpillover { .. }
        | E::EmptyFeatureMatrix
        | E::EmptyAggregate { .. }
        | E::FeatureShapeMismatch
        | E::FingerprintBinTooSmall { .. }
        | E::NoveltyFit { .. }
        | E::IncompatiblePreprocessing { .. } => INVALID,
    }
}

fn autospill(error: &cytiq_autospill::AutospillError) -> OutcomeReason {
    use cytiq_autospill::AutospillError as E;
    match error {
        E::MissingControlFile { .. } => MISSING,
        E::ManifestRead { source, .. }
        | E::DetectorRead { source, .. }
        | E::TransformRead { source, .. } => csv(source, INVALID),
        E::MatrixWrite { source, .. } => csv(source, RUNTIME),
        E::FcsRead { source, .. } => fcs(source),
        E::DebugOutputCreate { source, .. }
        | E::DebugWrite { source, .. }
        | E::CreateOutputFile { source, .. } => io(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::ParquetWrite { .. } | E::ParquetBatch { .. } => RUNTIME,
        E::EmptyManifest
        | E::EmptyDetectorsManifest
        | E::EmptyManifestField { .. }
        | E::EmptyDetectorField { .. }
        | E::EmptyTransformField { .. }
        | E::AbsoluteManifestFilename { .. }
        | E::DuplicateDye { .. }
        | E::DuplicatePrimaryChannel { .. }
        | E::DuplicateDetectorChannel { .. }
        | E::DuplicateTransformChannel { .. }
        | E::EmptyControl { .. }
        | E::ProcessedControl { .. }
        | E::PrimaryChannelNotFound { .. }
        | E::AmbiguousPrimaryChannel { .. }
        | E::AutofluorescenceRequiresDetectors { .. }
        | E::AutofluorescenceDetectorResolution { .. }
        | E::MissingChannel { .. }
        | E::MissingDetector { .. }
        | E::MissingTransform { .. }
        | E::DegenerateGate { .. }
        | E::RegressionFailed { .. }
        | E::SingularMatrix
        | E::IllConditionedPseudoInverse
        | E::NonConvergent { .. }
        | E::AutospreadFailed
        | E::UnsupportedOutputFormat { .. } => INVALID,
    }
}

fn autospectral(error: &cytiq_autospectral::AutoSpectralError) -> OutcomeReason {
    use cytiq_autospectral::AutoSpectralError as E;
    match error {
        E::MissingFcsFile { .. } => MISSING,
        E::ControlsManifestRead { source, .. }
        | E::SamplesManifestRead { source, .. }
        | E::DetectorsManifestRead { source, .. } => csv(source, INVALID),
        E::CsvWrite { source, .. } => csv(source, RUNTIME),
        E::FcsRead { source, .. } | E::FcsWrite { source, .. } => fcs(source),
        E::Mask(source) => mask(source),
        E::OutputDirectoryCreate { source, .. }
        | E::CsvFlush { source, .. }
        | E::JsonWrite { source, .. } => io(source),
        E::JsonSerialize { source, .. } => json(source, RUNTIME),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::NonFiniteDiagnostic { .. } => RUNTIME,
        E::EmptyControlsManifest
        | E::EmptySamplesManifest
        | E::EmptyDetectorsManifest
        | E::EmptyManifestField { .. }
        | E::AbsoluteManifestFilename { .. }
        | E::DuplicateDye { .. }
        | E::DuplicateDetectorChannel { .. }
        | E::ProcessedInput { .. }
        | E::ReservedDetectorChannel { .. }
        | E::MissingAutofluorescenceControl
        | E::DuplicateAutofluorescenceControl
        | E::NoSingleStainControls
        | E::DegenerateScatterGate { .. }
        | E::EmptyFcs { .. }
        | E::PrimaryChannelNotFound { .. }
        | E::AmbiguousPrimaryChannel { .. }
        | E::MissingChannel { .. }
        | E::MissingDetectorChannel { .. }
        | E::DetectorInferenceFailed
        | E::DetectorMismatch { .. }
        | E::TooFewDetectors { .. }
        | E::TooFewRetainedEvents { .. }
        | E::TooFewPositiveEvents { .. }
        | E::ZeroSpectrum { .. }
        | E::SingularReferenceMatrix
        | E::IllConditionedReferenceMatrix { .. }
        | E::NonFiniteSampleValues { .. } => INVALID,
    }
}

fn plot(error: &cytiq_plot::PlotError) -> OutcomeReason {
    use cytiq_plot::PlotError as E;
    match error {
        E::FcsRead { source, .. } | E::Fcs { source, .. } => fcs(source),
        E::Mask(source) => mask(source),
        E::Gating(source) => gating(source),
        E::Transform { source, .. } => transform(source),
        E::GatingMlRead { source, .. } | E::Write { source, .. } => io(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::InvalidSvg { .. } | E::Serialize { .. } => RUNTIME,
        E::MissingChannel { .. }
        | E::MissingAxis { .. }
        | E::UnknownGate { .. }
        | E::MissingGatingMl { .. }
        | E::GateAxisMismatch { .. }
        | E::UnsupportedGateGeometry { .. }
        | E::ScaleOnTransformedData { .. }
        | E::InvalidOption { .. }
        | E::Classes { .. }
        | E::ClassesAcquisition { .. }
        | E::ColorByWithoutClasses { .. }
        | E::ColorByNeedsScatter { .. }
        | E::Batch { .. } => INVALID,
    }
}

fn classify(error: &cytiq_classify::ClassifyError) -> OutcomeReason {
    use cytiq_classify::ClassifyError as E;
    match error {
        E::Io(source) => io(source),
        E::Json(source) => json(source, INVALID),
        E::Fcs(source) => fcs(source),
        E::Mask(source) => mask(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        E::Invalid { .. } => INVALID,
        E::Arrow { .. } | E::Parquet { .. } | E::Ort { .. } => RUNTIME,
    }
}

fn cytonorm(error: &cytiq_cytonorm::CytoNormError) -> OutcomeReason {
    use cytiq_cytonorm::CytoNormError as E;
    match error {
        E::Io(source) => io(source),
        E::Json(source) => json(source, INVALID),
        E::Fcs(source) => fcs(source),
        E::FlowSom(source) => flowsom(source),
        E::Mask(source) => mask(source),
        E::Publish(source) => OutcomeReason::of_publish(source),
        // Parquet and Arrow decode the model bundle.
        E::Invalid { .. } | E::Parquet { .. } | E::Arrow { .. } => INVALID,
    }
}
