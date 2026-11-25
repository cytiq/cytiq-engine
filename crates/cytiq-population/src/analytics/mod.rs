//! The analytics product of a dataset (`cytiq dataset export`): path-free
//! Parquet tables for SQL, `DuckDB` views and a `product.json` that lists
//! them, published as one directory.
//!
//! - Dimensions: subjects, specimens, samples, design factors, acquisitions,
//!   panels, channels and signal layers.
//! - Labels: a catalog of the dataset's `cytiq.events.labels.v1` tables,
//!   their columns and classes, and exact counts per state and value, from
//!   the verified tables.
//! - Observations: every listed `cytiq.population.observations.v1` row,
//!   flattened, with the sources and bindings of the listed annotation
//!   descriptors (without their paths).
//! - Parity: each observation whose native output has a label table must
//!   agree with its exact counts, or nothing is published.
//!
//! Inferential statistics are left to diffcyt and CATALYST: the views give
//! them their inputs. Cohort statistics count subjects, never events.

mod counts;
mod dimensions;
mod inputs;
mod observations;
mod parity;
mod tables;
mod views;

use std::path::{Path, PathBuf};

use cytiq_provenance::record_path_for_dir;
use cytiq_publish::{Existing, Prepared, Publication, PublishError};
use serde::{Deserialize, Serialize};

pub use observations::MAX_ROWS as MAX_OBSERVATIONS;
pub use parity::{ParityEntry, ParityLevel};
pub use tables::TableEntry;
pub use views::CONTRACT as VIEWS_CONTRACT;

use crate::{PopulationError, Result, serialization};

/// Contract identifier of `product.json`.
pub const CONTRACT: &str = "cytiq.analytics.product.v1";
pub const PRODUCT_FILE: &str = "product.json";
pub const VIEWS_FILE: &str = "views.sql";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportDatasetConfig {
    pub manifest: PathBuf,
    /// Where object keys resolve; the manifest's directory when `None`.
    pub root: Option<PathBuf>,
    /// The product directory.
    pub output: PathBuf,
    pub overwrite: bool,
}

impl ExportDatasetConfig {
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| {
            self.manifest
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        })
    }

    /// Where the provenance record of the export goes.
    #[must_use]
    pub fn record_path(&self) -> PathBuf {
        record_path_for_dir(&self.output)
    }
}

/// `product.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Product {
    pub contract: String,
    /// The dataset the product summarizes.
    pub dataset_id: String,
    pub tables: Vec<TableEntry>,
    pub views: ViewsEntry,
    /// The label tables observations were checked against.
    pub parity: Vec<ParityEntry>,
    pub cohort: Cohort,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewsEntry {
    pub contract: String,
    pub location: String,
    pub sha256: String,
    pub names: Vec<String>,
}

/// Whether `views.sql` holds the cohort views, and why not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cohort {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub struct AnalyticsRunner;

impl AnalyticsRunner {
    /// Files the export reads, by provenance role: the manifest and every
    /// label table, observations table and annotation descriptor it lists.
    ///
    /// # Errors
    /// Returns a missing or unparsable manifest.
    pub fn input_files(config: &ExportDatasetConfig) -> Result<Vec<(&'static str, PathBuf)>> {
        inputs::files(&config.manifest, &config.root())
    }

    /// Every file of the product, `product.json` first.
    #[must_use]
    pub fn outputs(config: &ExportDatasetConfig, product: &Product) -> Vec<PathBuf> {
        std::iter::once(config.output.join(PRODUCT_FILE))
            .chain(std::iter::once(config.output.join(&product.views.location)))
            .chain(
                product
                    .tables
                    .iter()
                    .map(|t| config.output.join(&t.location)),
            )
            .collect()
    }

    /// [`Self::prepare`], then publish the product atomically, without a
    /// provenance record.
    ///
    /// # Errors
    /// As [`Self::prepare`], and what publishing returns.
    pub fn run(config: &ExportDatasetConfig) -> Result<Product> {
        Ok(Self::prepare(config)?.commit()?)
    }

    /// Build the product in a staged directory. Nothing is published.
    ///
    /// # Errors
    /// Returns [`PublishError::Exists`] for an existing output without
    /// `overwrite`, a dataset that does not validate, objects that do not
    /// verify, observations outside the dataset or disagreeing with their
    /// label tables, and I/O errors.
    pub fn prepare(config: &ExportDatasetConfig) -> Result<Prepared<Product>> {
        if config.output.exists() && !config.overwrite {
            return Err(PublishError::Exists {
                path: config.output.clone(),
            }
            .into());
        }
        let inputs = inputs::read(&config.manifest, &config.root())?;
        let parity = parity::check(&inputs)?;
        let mut publication = Publication::new(Existing::overwrite(config.overwrite));
        let staged = publication.stage_dir(&config.output)?;
        let summary = write(&staged, &inputs, parity)?;
        Ok(Prepared {
            summary,
            publication,
        })
    }
}

/// Every sample's specimen has a subject: cohort views can count subjects.
fn cohort(manifest: &crate::dataset::DatasetManifest) -> Cohort {
    let subjects = dimensions::subjects_of(manifest);
    let reason = if manifest.design.is_none() {
        Some("the dataset has no design".to_owned())
    } else {
        manifest
            .samples
            .iter()
            .find(|s| {
                s.specimen
                    .as_deref()
                    .is_none_or(|specimen| !subjects.contains_key(specimen))
            })
            .map(|s| format!("sample `{}` has no subject", s.id))
    };
    Cohort {
        available: reason.is_none(),
        reason,
    }
}

fn write(directory: &Path, inputs: &inputs::Inputs, parity: Vec<ParityEntry>) -> Result<Product> {
    let manifest = &inputs.manifest;
    let annotation_of = inputs
        .labels
        .iter()
        .map(|t| {
            (
                t.summary.descriptor.producer.native_sha256.as_str(),
                t.summary.annotation_id.as_str(),
            )
        })
        .collect();
    let parity_table = tables::Columns::default()
        .text(
            "annotation_id",
            parity.iter().map(|p| p.annotation_id.clone()).collect(),
        )
        .uint(
            "observations",
            parity.iter().map(|p| p.observations).collect(),
        )
        .text(
            "level",
            parity.iter().map(|p| dimensions::name(&p.level)).collect(),
        );
    let built = [
        ("subjects", dimensions::subjects(manifest)),
        ("specimens", dimensions::specimens(manifest)),
        ("samples", dimensions::samples(manifest)),
        ("factors", dimensions::factors(manifest)),
        ("acquisitions", dimensions::acquisitions(manifest)),
        ("panels", dimensions::panels(manifest)),
        ("channels", dimensions::channels(manifest)),
        ("layers", dimensions::layers(manifest)),
        ("labels", counts::catalog(&inputs.labels)),
        ("label_columns", counts::columns(&inputs.labels)),
        ("label_classes", counts::classes(&inputs.labels)),
        ("label_states", counts::states(&inputs.labels)),
        ("label_counts", counts::counts(&inputs.labels)),
        ("observations", observations::observations(inputs)?),
        (
            "sources",
            observations::sources(&inputs.descriptors, &annotation_of),
        ),
        ("bindings", observations::bindings(&inputs.descriptors)),
        ("parity", parity_table),
    ];
    debug_assert!(built.iter().map(|(n, _)| *n).eq(views::TABLES));
    let tables = built
        .into_iter()
        .map(|(name, columns)| tables::write(directory, name, &columns))
        .collect::<Result<Vec<_>>>()?;
    let cohort = cohort(manifest);
    let generated = views::generate(manifest, &inputs.labels, cohort.available);
    std::fs::write(directory.join(VIEWS_FILE), &generated.text)?;
    let product = Product {
        contract: CONTRACT.into(),
        dataset_id: manifest.dataset_id(),
        tables,
        views: ViewsEntry {
            contract: VIEWS_CONTRACT.into(),
            location: VIEWS_FILE.into(),
            sha256: cytiq_provenance::hash_file(&directory.join(VIEWS_FILE))?.1,
            names: generated.names,
        },
        parity,
        cohort,
    };
    std::fs::write(
        directory.join(PRODUCT_FILE),
        serialization::canonical(&serde_json::to_value(&product).map_err(PopulationError::from)?),
    )?;
    Ok(product)
}
