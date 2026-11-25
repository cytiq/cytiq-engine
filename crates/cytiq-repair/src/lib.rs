//! Repair of FCS files that instruments wrote against the standard.
//!
//! The reader of `cytiq-fcs` is strict and stays so: no other command reads a
//! file around a deviation. This crate names the deviations of a file, and
//! rewrites HEADER and TEXT, never DATA, when DATA can be read in exactly one
//! way and the caller allows each repair by its code. Everything dropped is
//! listed with its digest. What it writes is read back by the strict reader.

mod codes;
mod diagnosis;
mod error;
mod plan;
mod repaired;
mod scan;
mod text;

pub use crate::codes::{Refusal, RefusalCode, RepairCode};
pub use crate::diagnosis::{Diagnosis, PLAN_CONTRACT, PlanDocument, PlanStatus, diagnose};
pub use crate::error::RepairError;
pub use crate::plan::{Change, DataWindow, Dropped, Finding, Plan};
pub use crate::repaired::Repaired;

/// Keyword a repaired file records its repairs in.
pub const REPAIR_KEYWORD: &str = "CYTIQ_REPAIR";
/// Version of the repairs and of [`REPAIR_KEYWORD`].
pub const REPAIR_VERSION: u32 = 1;
