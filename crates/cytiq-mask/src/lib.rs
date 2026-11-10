//! Per-event masks shared by cytiq stages.
//!
//! A mask is a Parquet file with one row per event of the file it was computed
//! on:
//!
//! | column            | type            | meaning                                   |
//! | ----------------- | --------------- | ----------------------------------------- |
//! | `event_id`        | `UInt64`        | 0-based row in that file                  |
//! | `source_event_id` | `UInt64`        | 0-based row in the raw file               |
//! | filter columns    | nullable `Bool` | pass/fail; null means "not assessed"      |
//! | `keep`            | `Bool`          | whether later stages should use the event |
//!
//! `source_event_id` lets a mask computed on one file apply to any file derived
//! from the same raw acquisition. Parquet key-value metadata ties the mask to
//! that acquisition:
//!
//! | key                         | value                                          |
//! | --------------------------- | ---------------------------------------------- |
//! | `cytiq.mask.schema_version` | `2`                                            |
//! | `cytiq.acquisition_sha256`  | SHA-256 of the raw FCS (`CYTIQ_ACQUISITION`)   |
//! | `cytiq.event_id_base`       | `0`                                            |
//!
//! Combining masks, or applying one to a file, from different acquisitions is
//! an error, and masks without this metadata are rejected.

mod error;
mod mask;
pub mod parquet;
mod runner;

pub use crate::error::MaskError;
pub use crate::mask::{
    ACQUISITION_KEY, EVENT_ID_BASE_KEY, EVENT_ID_COLUMN, EventMask, KEEP_COLUMN, SCHEMA_VERSION,
    SCHEMA_VERSION_KEY, SOURCE_EVENT_ID_COLUMN, SourceKeep, read_combined,
};
pub use crate::runner::{MaskCombineConfig, MaskCombineRunner};
