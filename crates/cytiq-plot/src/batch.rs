//! Batch plot files: many plots of one sample from YAML or JSON.
//!
//! ```yaml
//! version: 1
//! defaults: { bins: 256, palette: pseudocolor }
//! plots:
//!   - { name: fsc_ssc, type: density, x: FSC-A, y: SSC-A }
//!   - { name: singlets, type: scatter, gates: [singlets] }
//!   - { name: t_cd4_cd8, type: contour, x: CD4, y: CD8, population: t_cells }
//! ```
//!
//! Unknown fields are errors. A plot field overrides `defaults`, which
//! override the built-in defaults. Names are file stems: `[A-Za-z0-9_-]`,
//! unique without regard to case (case-insensitive file systems).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{
    AxisOptions, OutputFormat, Palette, PlotJob, PlotKind, PlotRequest, RemovedLayer, ScaleChoice,
    validate_name,
};
use crate::error::PlotError;

/// Batch file schema version this crate reads.
pub const BATCH_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchFile {
    version: u32,
    #[serde(default)]
    defaults: PlotDefaults,
    plots: Vec<PlotEntry>,
}

/// Every plot field except `name` and `type`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotDefaults {
    x: Option<String>,
    y: Option<String>,
    gates: Option<Vec<String>>,
    population: Option<String>,
    x_scale: Option<ScaleChoice>,
    y_scale: Option<ScaleChoice>,
    x_range: Option<[f64; 2]>,
    y_range: Option<[f64; 2]>,
    bins: Option<usize>,
    max_points: Option<usize>,
    palette: Option<Palette>,
    removed: Option<RemovedLayer>,
    contour_step: Option<f64>,
    color_by: Option<bool>,
    width: Option<u32>,
    height: Option<u32>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotEntry {
    name: String,
    #[serde(rename = "type")]
    kind: PlotKind,
    x: Option<String>,
    y: Option<String>,
    gates: Option<Vec<String>>,
    population: Option<String>,
    x_scale: Option<ScaleChoice>,
    y_scale: Option<ScaleChoice>,
    x_range: Option<[f64; 2]>,
    y_range: Option<[f64; 2]>,
    bins: Option<usize>,
    max_points: Option<usize>,
    palette: Option<Palette>,
    removed: Option<RemovedLayer>,
    contour_step: Option<f64>,
    color_by: Option<bool>,
    width: Option<u32>,
    height: Option<u32>,
    title: Option<String>,
}

/// Largest batch file read; real batches are a few KiB.
const BATCH_BYTES_MAX: u64 = 16 << 20;

/// Read at most [`BATCH_BYTES_MAX`] bytes as UTF-8 before parsing.
fn read_batch(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(BATCH_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > BATCH_BYTES_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("batch file exceeds {BATCH_BYTES_MAX} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Read a batch file and plan each plot's outputs as
/// `<output_dir>/<name>.<extension>` for every format.
///
/// # Errors
///
/// Returns [`PlotError::Batch`] when the file cannot be read or parsed, has
/// an unsupported version, no plots, or invalid or duplicate names, and
/// [`PlotError::InvalidOption`] for an invalid plot option.
pub fn load_batch(
    path: &Path,
    output_dir: &Path,
    formats: &[OutputFormat],
) -> Result<Vec<PlotJob>, PlotError> {
    let fail = |message: String| PlotError::Batch {
        path: path.to_path_buf(),
        message,
    };
    let text = read_batch(path).map_err(|error| fail(error.to_string()))?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let file: BatchFile = match extension.as_deref() {
        Some("yaml" | "yml") => {
            serde_saphyr::from_str(&text).map_err(|error| fail(error.to_string()))?
        }
        Some("json") => serde_json::from_str(&text).map_err(|error| fail(error.to_string()))?,
        _ => return Err(fail("must be `.yaml`, `.yml` or `.json`".to_owned())),
    };
    if file.version != BATCH_VERSION {
        return Err(fail(format!(
            "version {} is not supported; expected {BATCH_VERSION}",
            file.version
        )));
    }
    if file.plots.is_empty() {
        return Err(fail("`plots` is empty".to_owned()));
    }
    if formats.is_empty() {
        return Err(fail("no output format".to_owned()));
    }
    let mut names = HashSet::new();
    let mut jobs = Vec::with_capacity(file.plots.len());
    for entry in file.plots {
        validate_name(&entry.name).map_err(|message| fail(format!("plot name {message}")))?;
        if !names.insert(entry.name.to_ascii_lowercase()) {
            return Err(fail(format!("plot name `{}` is used twice", entry.name)));
        }
        let request = merge(entry, &file.defaults);
        request.validate()?;
        let outputs = formats
            .iter()
            .map(|&format| (format, output_path(output_dir, &request.name, format)))
            .collect();
        jobs.push(PlotJob { request, outputs });
    }
    Ok(jobs)
}

fn output_path(directory: &Path, name: &str, format: OutputFormat) -> PathBuf {
    directory.join(format!("{name}.{}", format.extension()))
}

fn merge(entry: PlotEntry, defaults: &PlotDefaults) -> PlotRequest {
    let base = PlotRequest::new(entry.name);
    let range = |value: Option<[f64; 2]>| value.map(|[lo, hi]| (lo, hi));
    PlotRequest {
        kind: entry.kind,
        x: AxisOptions {
            channel: entry.x.or_else(|| defaults.x.clone()),
            scale: entry.x_scale.or(defaults.x_scale).unwrap_or_default(),
            range: range(entry.x_range.or(defaults.x_range)),
        },
        y: AxisOptions {
            channel: entry.y.or_else(|| defaults.y.clone()),
            scale: entry.y_scale.or(defaults.y_scale).unwrap_or_default(),
            range: range(entry.y_range.or(defaults.y_range)),
        },
        gates: entry
            .gates
            .or_else(|| defaults.gates.clone())
            .unwrap_or_default(),
        population: entry.population.or_else(|| defaults.population.clone()),
        color_by: entry.color_by.or(defaults.color_by).unwrap_or(false),
        bins: entry.bins.or(defaults.bins).unwrap_or(base.bins),
        max_points: entry
            .max_points
            .or(defaults.max_points)
            .unwrap_or(base.max_points),
        palette: entry.palette.or(defaults.palette).unwrap_or(base.palette),
        removed: entry.removed.or(defaults.removed).unwrap_or(base.removed),
        contour_step: entry
            .contour_step
            .or(defaults.contour_step)
            .unwrap_or(base.contour_step),
        width: entry.width.or(defaults.width).unwrap_or(base.width),
        height: entry.height.or(defaults.height).unwrap_or(base.height),
        title: entry.title.or_else(|| defaults.title.clone()),
        name: base.name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str, extension: &str) -> Result<Vec<PlotJob>, PlotError> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("plots.{extension}"));
        std::fs::write(&path, text).unwrap();
        load_batch(
            &path,
            Path::new("out"),
            &[OutputFormat::Svg, OutputFormat::Plotly],
        )
    }

    #[test]
    fn oversized_batch_is_refused_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plots.yaml");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(BATCH_BYTES_MAX + 1)
            .unwrap();
        let error = load_batch(&path, Path::new("out"), &[OutputFormat::Svg]).unwrap_err();
        assert!(
            matches!(&error, PlotError::Batch { message, .. } if message.contains("exceeds")),
            "{error}"
        );
    }

    #[test]
    fn defaults_apply_when_a_plot_omits_a_field() {
        let jobs = load(
            "version: 1\ndefaults:\n  bins: 128\n  palette: viridis\nplots:\n  - name: fsc_ssc\n    type: density\n    x: FSC-A\n    y: SSC-A\n  - name: singlets\n    type: scatter\n    gates: [singlets]\n    bins: 64\n    x_range: [0, 1000]\n",
            "yaml",
        )
        .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].request.bins, 128);
        assert_eq!(jobs[0].request.palette, Palette::Viridis);
        assert_eq!(jobs[0].request.x.channel.as_deref(), Some("FSC-A"));
        assert_eq!(jobs[1].request.bins, 64);
        assert_eq!(jobs[1].request.kind, PlotKind::Scatter);
        assert_eq!(jobs[1].request.gates, ["singlets"]);
        assert_eq!(jobs[1].request.x.range, Some((0.0, 1000.0)));
        assert_eq!(
            jobs[0].outputs,
            [
                (OutputFormat::Svg, PathBuf::from("out/fsc_ssc.svg")),
                (
                    OutputFormat::Plotly,
                    PathBuf::from("out/fsc_ssc.plotly.json")
                ),
            ]
        );
    }

    #[test]
    fn batch_rejects_unknown_fields() {
        let error = load(
            "version: 1\nplots:\n  - {name: a, type: density, x: FSC-A, y: SSC-A, colour: red}\n",
            "yaml",
        )
        .unwrap_err();
        assert!(error.to_string().contains("colour"), "{error}");
        let error = load(r#"{"version": 2, "plots": []}"#, "json").unwrap_err();
        assert!(error.to_string().contains("version 2"), "{error}");
    }

    #[test]
    fn batch_rejects_duplicate_or_unsafe_names() {
        let duplicate = load(
            "version: 1\nplots:\n  - {name: Plot, type: density}\n  - {name: plot, type: scatter}\n",
            "yaml",
        )
        .unwrap_err();
        assert!(duplicate.to_string().contains("used twice"), "{duplicate}");
        let unsafe_name = load(
            "version: 1\nplots:\n  - {name: ../escape, type: density}\n",
            "yaml",
        )
        .unwrap_err();
        assert!(
            unsafe_name.to_string().contains("may only contain"),
            "{unsafe_name}"
        );
    }

    #[test]
    fn batch_options_are_validated() {
        let error = load(
            "version: 1\nplots:\n  - {name: a, type: density, bins: 4}\n",
            "yaml",
        )
        .unwrap_err();
        assert!(error.to_string().contains("bins"), "{error}");
    }
}
