//! Flat Parquet tables of the analytics product: built column by column,
//! written with the types and encodings the bounded reader accepts, and read
//! back through it before publication.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use cytiq_mask::parquet::{ParquetReadLimits, ScalarParquetReader};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;
use serde::{Deserialize, Serialize};

use crate::{DEFAULT_BATCH_SIZE, PopulationError, Result, require};

/// Identifies the writer in the Parquet footer.
const CREATED_BY: &str = "cytiq analytics.v1";

/// Columns of one table, in order. Every column has the same length.
#[derive(Default)]
pub(crate) struct Columns {
    fields: Vec<Field>,
    arrays: Vec<ArrayRef>,
}

impl Columns {
    pub(crate) fn text(self, name: &str, values: Vec<String>) -> Self {
        self.push(name, false, Arc::new(StringArray::from(values)))
    }

    pub(crate) fn text_or_null(self, name: &str, values: Vec<Option<String>>) -> Self {
        self.push(name, true, Arc::new(StringArray::from(values)))
    }

    pub(crate) fn uint(self, name: &str, values: Vec<u64>) -> Self {
        self.push(name, false, Arc::new(UInt64Array::from(values)))
    }

    pub(crate) fn uint_or_null(self, name: &str, values: Vec<Option<u64>>) -> Self {
        self.push(name, true, Arc::new(UInt64Array::from(values)))
    }

    pub(crate) fn float_or_null(self, name: &str, values: Vec<Option<f64>>) -> Self {
        self.push(name, true, Arc::new(Float64Array::from(values)))
    }

    pub(crate) fn boolean(self, name: &str, values: Vec<bool>) -> Self {
        self.push(name, false, Arc::new(BooleanArray::from(values)))
    }

    pub(crate) fn boolean_or_null(self, name: &str, values: Vec<Option<bool>>) -> Self {
        self.push(name, true, Arc::new(BooleanArray::from(values)))
    }

    fn push(mut self, name: &str, nullable: bool, array: ArrayRef) -> Self {
        self.fields
            .push(Field::new(name, array.data_type().clone(), nullable));
        self.arrays.push(array);
        self
    }
}

/// A table of the product, as `product.json` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableEntry {
    pub name: String,
    /// Relative to the product directory.
    pub location: String,
    pub rows: u64,
    pub bytes: u64,
    pub sha256: String,
}

/// Write `columns` as the table `name` at `directory/tables/<name>.parquet`
/// and read it back through the bounded reader.
pub(crate) fn write(directory: &Path, name: &str, columns: &Columns) -> Result<TableEntry> {
    let location = format!("tables/{name}.parquet");
    let path = directory.join(&location);
    std::fs::create_dir_all(path.parent().expect("tables directory"))?;
    let rows = columns.arrays.first().map_or(0, |array| array.len());
    require(
        columns.arrays.iter().all(|array| array.len() == rows),
        format!("the columns of table `{name}` differ in length"),
    )?;
    let schema = Arc::new(Schema::new(columns.fields.clone()));
    let mut properties = WriterProperties::builder()
        .set_created_by(CREATED_BY.to_owned())
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(1)?))
        .set_dictionary_enabled(false);
    for field in &columns.fields {
        if field.data_type() == &DataType::Utf8 {
            properties = properties
                .set_column_dictionary_enabled(ColumnPath::from(field.name().as_str()), true);
        }
    }
    let mut writer = ArrowWriter::try_new(
        File::create(&path)?,
        Arc::clone(&schema),
        Some(properties.build()),
    )?;
    // An empty table still has its schema in the footer.
    for offset in (0..rows).step_by(DEFAULT_BATCH_SIZE) {
        let take = DEFAULT_BATCH_SIZE.min(rows - offset);
        let arrays = columns
            .arrays
            .iter()
            .map(|array| array.slice(offset, take))
            .collect();
        writer.write(&RecordBatch::try_new(Arc::clone(&schema), arrays)?)?;
    }
    writer.close()?;
    read_back(&path, &schema, rows as u64)?;
    let (bytes, sha256) = cytiq_provenance::hash_file(&path)?;
    Ok(TableEntry {
        name: name.to_owned(),
        location,
        rows: rows as u64,
        bytes,
        sha256,
    })
}

/// The table reads back through the bounded reader with its schema and
/// every row.
fn read_back(path: &Path, schema: &Schema, rows: u64) -> Result<()> {
    let reader = ScalarParquetReader::new(File::open(path)?, ParquetReadLimits::default())?;
    let fields = reader.schema().fields();
    require(
        fields.len() == schema.fields().len()
            && fields.iter().zip(schema.fields()).all(|(read, written)| {
                read.name() == written.name() && read.data_type() == written.data_type()
            }),
        "an analytics table reads back with other columns than were written",
    )?;
    let mut read = 0_u64;
    for batch in reader {
        read += batch?.num_rows() as u64;
    }
    if read == rows {
        Ok(())
    } else {
        Err(PopulationError::Invalid(format!(
            "an analytics table reads back {read} rows, not {rows}"
        )))
    }
}
