//! End-to-end validation of the whole cytiq chain on golden data with known
//! per-event truth (`data/fixtures/pipeline`, see its README):
//!
//! preflight → compensate → transform → peacoqc → clean (raw) → mask combine
//! → qc → gate --mask --membership → flowsom --mask
//!
//! Acceptance thresholds live in `data/fixtures/pipeline/thresholds.json`.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cytiq_fcs::{FcsDataset, FcsReader};
use cytiq_mask::EventMask;
use serde_json::Value;

const FLUORESCENCE: &str = "FL1-A,FL2-A,FL3-A,FL4-A,FL5-A";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/fixtures/pipeline")
        .join(name)
}

fn create_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&directory).expect("temp dir should be created");
    directory
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("path should be UTF-8")
}

/// Run `cytiq` in `directory`, so outputs can be named relatively and two
/// runs in different directories see identical arguments.
fn cytiq(directory: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_cytiq"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("command should run");
    assert!(
        output.status.success(),
        "`cytiq {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The documented conventional chain, with outputs named relative to
/// `directory`.
fn run_chain(directory: &Path) {
    let sample = fixture("sample.fcs");
    let gates = fixture("gates.xml");
    let manifest = fixture("run.yaml");
    let steps: [&[&str]; 10] = [
        &[
            "preflight",
            "--manifest",
            utf8(&manifest),
            "--output",
            "preflight.json",
        ],
        &[
            "compensate",
            "--input",
            utf8(&sample),
            "--output",
            "s.comp.fcs",
        ],
        &["transform", "--input", "s.comp.fcs", "--output", "s.t.fcs"],
        &[
            "peacoqc",
            "--input",
            "s.t.fcs",
            "--output",
            "s.peacoqc.fcs",
            "--channels",
            FLUORESCENCE,
        ],
        &["clean", "--input", utf8(&sample), "--output", "clean"],
        &[
            "mask",
            "combine",
            "--mask",
            "s.comp.masks.parquet",
            "--mask",
            "s.peacoqc.masks.parquet",
            "--mask",
            "clean/masks.parquet",
            "--output",
            "analysis.parquet",
        ],
        &[
            "qc",
            "--manifest",
            utf8(&manifest),
            "--sample",
            "GOLDEN",
            "--mask",
            "analysis.parquet",
            "--output",
            "qc.json",
        ],
        &[
            "gate",
            "--input",
            "s.t.fcs",
            "--gating-ml",
            utf8(&gates),
            "--output",
            "counts.json",
            "--format",
            "json",
            "--mask",
            "analysis.parquet",
            "--membership",
            "membership.parquet",
        ],
        &[
            "flowsom",
            "--input",
            "s.t.fcs",
            "--output",
            "flowsom7.csv",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "analysis.parquet",
            "--metaclusters",
            "7",
        ],
        &[
            "flowsom",
            "--input",
            "s.t.fcs",
            "--output",
            "flowsom.csv",
            "--channels",
            FLUORESCENCE,
            "--mask",
            "analysis.parquet",
        ],
    ];
    for step in steps {
        cytiq(directory, step);
    }
}

struct Truth {
    population: Vec<String>,
    /// Flags per event: debris, doublet, dead, unstable, margin.
    flags: HashMap<&'static str, Vec<bool>>,
}

impl Truth {
    const FLAGS: [&'static str; 5] = ["debris", "doublet", "dead", "unstable", "margin"];

    fn read() -> Self {
        let mut reader = csv::Reader::from_path(fixture("truth/events.csv")).expect("truth");
        let headers = reader.headers().expect("truth header").clone();
        let column = |name: &str| {
            headers
                .iter()
                .position(|header| header == name)
                .expect(name)
        };
        let mut population = Vec::new();
        let mut flags: HashMap<&'static str, Vec<bool>> =
            Self::FLAGS.iter().map(|flag| (*flag, Vec::new())).collect();
        for (row, record) in reader.records().enumerate() {
            let record = record.expect("truth row");
            assert_eq!(record[column("raw_row")].parse::<usize>().ok(), Some(row));
            population.push(record[column("population")].to_owned());
            for flag in Self::FLAGS {
                flags
                    .get_mut(flag)
                    .expect("flag")
                    .push(&record[column(flag)] == "1");
            }
        }
        Self { population, flags }
    }

    fn len(&self) -> usize {
        self.population.len()
    }

    fn flag(&self, name: &str) -> &[bool] {
        &self.flags[name]
    }

    fn good(&self) -> Vec<bool> {
        (0..self.len())
            .map(|row| Self::FLAGS.iter().all(|flag| !self.flags[flag][row]))
            .collect()
    }
}

/// Mask column `name` (or `keep`) per raw row: `true` means removed.
fn removed(mask: &EventMask, name: &str, rows: usize) -> Vec<bool> {
    let column = if name == "keep" {
        mask.keep.iter().map(|keep| Some(*keep)).collect::<Vec<_>>()
    } else {
        mask.filters
            .iter()
            .find(|(filter, _)| filter == name)
            .unwrap_or_else(|| panic!("mask has no `{name}` column"))
            .1
            .clone()
    };
    let mut result = vec![false; rows];
    for (source, pass) in mask.source_event_ids.iter().zip(column) {
        result[usize::try_from(*source).expect("row")] = pass == Some(false);
    }
    result
}

fn read_mask(directory: &Path, name: &str) -> EventMask {
    EventMask::read_parquet(&directory.join(name)).expect("mask should be readable")
}

fn read_fcs(path: &Path) -> FcsDataset {
    FcsReader::open(path)
        .and_then(FcsReader::read)
        .expect("FCS should be readable")
}

fn column(dataset: &FcsDataset, name: &str) -> Vec<f64> {
    let index = dataset.find_parameter(name).expect("channel");
    dataset.data.column(index).expect("column").to_f64_vec()
}

/// Least-squares slope of `y` on `x` over `rows`.
#[allow(clippy::cast_precision_loss)]
fn slope(x: &[f64], y: &[f64], rows: &[usize]) -> f64 {
    let n = rows.len() as f64;
    let mean_x = rows.iter().map(|row| x[*row]).sum::<f64>() / n;
    let mean_y = rows.iter().map(|row| y[*row]).sum::<f64>() / n;
    let covariance = rows
        .iter()
        .map(|row| (x[*row] - mean_x) * (y[*row] - mean_y))
        .sum::<f64>();
    let variance = rows
        .iter()
        .map(|row| (x[*row] - mean_x).powi(2))
        .sum::<f64>();
    covariance / variance
}

#[allow(clippy::cast_precision_loss)]
fn fraction(count: usize, total: usize) -> f64 {
    count as f64 / total.max(1) as f64
}

fn count(flags: impl Iterator<Item = bool>) -> usize {
    flags.filter(|flag| *flag).count()
}

/// Recall of `truth` by `predicted`, and the fraction of `good` events that
/// `predicted` hits.
fn recall_and_good_loss(predicted: &[bool], truth: &[bool], good: &[bool]) -> (f64, f64) {
    let hits = count(predicted.iter().zip(truth).map(|(p, t)| *p && *t));
    let good_hits = count(predicted.iter().zip(good).map(|(p, g)| *p && *g));
    (
        fraction(hits, count(truth.iter().copied())),
        fraction(good_hits, count(good.iter().copied())),
    )
}

struct Thresholds(Value);

impl Thresholds {
    fn read() -> Self {
        Self(
            serde_json::from_str(
                &fs::read_to_string(fixture("thresholds.json")).expect("thresholds"),
            )
            .expect("thresholds should be JSON"),
        )
    }

    fn get(&self, name: &str) -> f64 {
        self.0[name]
            .as_f64()
            .unwrap_or_else(|| panic!("threshold `{name}`"))
    }

    fn at_least(&self, name: &str, value: f64, what: &str) {
        let bound = self.get(name);
        assert!(value >= bound, "{what}: {value:.4} < {name} {bound}");
    }

    fn at_most(&self, name: &str, value: f64, what: &str) {
        let bound = self.get(name);
        assert!(value <= bound, "{what}: {value:.4} > {name} {bound}");
    }
}

/// `(event_index, source_event_id, node, metacluster)` rows of a `FlowSOM` CSV.
fn read_flowsom(path: &Path) -> Vec<(usize, usize, u64, u64)> {
    let mut reader = csv::Reader::from_path(path).expect("flowsom CSV");
    reader
        .records()
        .map(|record| {
            let record = record.expect("row");
            let field = |index: usize| record[index].parse::<u64>().expect("integer");
            (
                usize::try_from(field(0)).expect("index"),
                usize::try_from(field(1)).expect("source"),
                field(2),
                field(3),
            )
        })
        .collect()
}

/// Largest share of `population` within one group that contains it.
fn best_purity(groups: &[u64], labels: &[&str], population: &str) -> f64 {
    let mut totals = HashMap::<u64, (usize, usize)>::new();
    for (group, label) in groups.iter().zip(labels) {
        let entry = totals.entry(*group).or_default();
        entry.0 += 1;
        entry.1 += usize::from(*label == population);
    }
    totals
        .values()
        .filter(|(_, hits)| *hits > 0)
        .map(|(size, hits)| fraction(*hits, *size))
        .fold(0.0, f64::max)
}

#[test]
#[allow(clippy::too_many_lines)]
fn golden_chain_recovers_the_known_truth() {
    let directory = create_temp_dir("cytiq-engine-pipeline");
    run_chain(&directory);
    let truth = Truth::read();
    let limits = Thresholds::read();
    let rows = truth.len();
    let good = truth.good();

    // Preflight passes on the raw golden file.
    let preflight: Value = serde_json::from_str(
        &fs::read_to_string(directory.join("preflight.json")).expect("preflight report"),
    )
    .expect("JSON");
    assert_eq!(preflight["failures"], 0);

    // Compensation removes the CD19 → CD56 spillover (0.12 in the matrix).
    let raw = read_fcs(&fixture("sample.fcs"));
    let compensated = read_fcs(&directory.join("s.comp.fcs"));
    let b_cells = (0..rows)
        .filter(|row| good[*row] && truth.population[*row] == "b")
        .collect::<Vec<_>>();
    let raw_slope = slope(&column(&raw, "FL4-A"), &column(&raw, "FL5-A"), &b_cells);
    let compensated_slope = slope(
        &column(&compensated, "FL4-A"),
        &column(&compensated, "FL5-A"),
        &b_cells,
    );
    assert!(raw_slope > 0.1, "raw spillover slope {raw_slope}");
    limits.at_most(
        "compensation_residual_slope_max",
        compensated_slope.abs(),
        "residual CD19→CD56 spillover",
    );

    // Detector margins, judged on raw values before compensation.
    let margin_removed = removed(
        &read_mask(&directory, "s.comp.masks.parquet"),
        "margin_pass",
        rows,
    );
    let (recall, loss) = recall_and_good_loss(&margin_removed, truth.flag("margin"), &good);
    limits.at_least("margin_recall_min", recall, "saturated events found");
    limits.at_most("margin_good_loss_max", loss, "good events lost to margins");

    // PeacoQC on compensated, transformed values.
    let peacoqc_removed = removed(
        &read_mask(&directory, "s.peacoqc.masks.parquet"),
        "keep",
        rows,
    );
    let (recall, loss) = recall_and_good_loss(&peacoqc_removed, truth.flag("unstable"), &good);
    limits.at_least(
        "peacoqc_unstable_recall_min",
        recall,
        "unstable events found",
    );
    limits.at_most("peacoqc_good_loss_max", loss, "good events lost to PeacoQC");

    // clean on the raw file.
    let clean = read_mask(&directory, "clean/masks.parquet");
    for (flag, filter) in [
        ("debris", "non_debris"),
        ("doublet", "singlets"),
        ("dead", "live"),
    ] {
        let filter_removed = removed(&clean, filter, rows);
        let (recall, loss) = recall_and_good_loss(&filter_removed, truth.flag(flag), &good);
        limits.at_least("clean_recall_min", recall, &format!("{flag} recall"));
        limits.at_most(
            "clean_good_loss_max",
            loss,
            &format!("good events lost to {filter}"),
        );
    }

    // The combined analysis mask against the true good events.
    let analysis = read_mask(&directory, "analysis.parquet");
    let margins = read_mask(&directory, "s.comp.masks.parquet");
    let peacoqc = read_mask(&directory, "s.peacoqc.masks.parquet");
    let labels = truth
        .population
        .iter()
        .cloned()
        .map(Some)
        .collect::<Vec<_>>();
    let retention = cytiq_qc::retention::population_retention(
        &analysis.acquisition,
        &labels,
        Some(&good),
        &[
            ("margins", &margins),
            ("peacoqc", &peacoqc),
            ("clean", &clean),
            ("combined", &analysis),
        ],
    )
    .expect("population retention");
    fs::write(
        directory.join("population_retention.json"),
        serde_json::to_vec_pretty(&retention).unwrap(),
    )
    .unwrap();
    for population in &retention.last().unwrap().populations {
        if population.good_original.unwrap_or(0) != 0 {
            limits.at_least(
                "original_good_population_retention_min",
                population.good_retention.unwrap(),
                &population.population,
            );
        }
    }
    let kept = removed(&analysis, "keep", rows)
        .into_iter()
        .map(|removed| !removed)
        .collect::<Vec<_>>();
    let bad_kept = count(kept.iter().zip(&good).map(|(k, g)| *k && !*g));
    limits.at_most(
        "final_bad_kept_max",
        fraction(bad_kept, rows - count(good.iter().copied())),
        "bad events kept",
    );
    let both = count(kept.iter().zip(&good).map(|(k, g)| *k && *g));
    #[allow(clippy::cast_precision_loss)]
    let f1 = 2.0 * both as f64 / (count(kept.iter().copied()) + count(good.iter().copied())) as f64;
    limits.at_least(
        "final_keep_f1_min",
        f1,
        "F1 of the kept events against good",
    );

    // Lineage: every per-event output covers each raw row exactly once.
    let all_rows = (0..rows as u64).collect::<Vec<_>>();
    for name in [
        "s.comp.masks.parquet",
        "s.peacoqc.masks.parquet",
        "clean/masks.parquet",
        "analysis.parquet",
        "membership.parquet",
    ] {
        let mut sources = read_mask(&directory, name).source_event_ids;
        sources.sort_unstable();
        assert_eq!(sources, all_rows, "{name} must cover every raw row once");
    }

    // Population gates on the kept events against the true populations.
    let membership = read_mask(&directory, "membership.parquet");
    let gate = |id: &str| {
        let column = &membership
            .filters
            .iter()
            .find(|(gate, _)| gate == id)
            .unwrap_or_else(|| panic!("gate {id}"))
            .1;
        let mut members = vec![false; rows];
        for (source, member) in membership.source_event_ids.iter().zip(column) {
            members[usize::try_from(*source).expect("row")] = *member == Some(true);
        }
        members
    };
    for (id, population) in [
        ("CD4T", "cd4_t"),
        ("CD8T", "cd8_t"),
        ("DNT", "dn_t"),
        ("B", "b"),
        ("NK", "nk"),
        ("Rare", "rare"),
    ] {
        let members = gate(id);
        let truth_members = (0..rows)
            .map(|row| kept[row] && truth.population[row] == population)
            .collect::<Vec<_>>();
        let hits = count(members.iter().zip(&truth_members).map(|(m, t)| *m && *t));
        let precision = fraction(hits, count(members.iter().copied()));
        let recall = fraction(hits, count(truth_members.iter().copied()));
        limits.at_least("gate_precision_min", precision, &format!("{id} precision"));
        limits.at_least("gate_recall_min", recall, &format!("{id} recall"));
        if id == "Rare" {
            let expected = count(truth_members.iter().copied());
            assert!(expected > 0, "the rare population must survive QC");
            #[allow(clippy::cast_precision_loss)]
            let error =
                (count(members.iter().copied()) as f64 - expected as f64).abs() / expected as f64;
            limits.at_most(
                "rare_count_relative_error_max",
                error,
                "rare population count",
            );
        }
    }
    assert!(
        (0..rows).all(|row| !gate("Lymphocytes")[row] || kept[row]),
        "events outside the analysis mask must not be gated"
    );

    // FlowSOM on the kept events: every kept event and only those, and the
    // clusters match the true populations.
    let flowsom7 = read_flowsom(&directory.join("flowsom7.csv"));
    let clustered = flowsom7.iter().map(|row| row.1).collect::<HashSet<_>>();
    let expected = (0..rows).filter(|row| kept[*row]).collect::<HashSet<_>>();
    assert_eq!(
        clustered, expected,
        "FlowSOM must cluster exactly the kept events"
    );
    assert!(
        flowsom7.iter().all(|row| row.0 == row.1),
        "no Original_ID: row == source"
    );
    let labels = flowsom7
        .iter()
        .map(|row| truth.population[row.1].as_str())
        .collect::<Vec<_>>();
    let label_ids = {
        let mut ids = BTreeMap::new();
        labels
            .iter()
            .map(|label| {
                let next = ids.len() as u64;
                *ids.entry(*label).or_insert(next)
            })
            .collect::<Vec<_>>()
    };
    let metaclusters = flowsom7.iter().map(|row| row.3).collect::<Vec<_>>();
    limits.at_least(
        "flowsom_ari_min",
        common::adjusted_rand_index(&label_ids, &metaclusters),
        "FlowSOM ARI (7 metaclusters)",
    );
    let nodes = flowsom7.iter().map(|row| row.2).collect::<Vec<_>>();
    limits.at_least(
        "rare_node_purity_min",
        best_purity(&nodes, &labels, "rare"),
        "rare population SOM node purity",
    );
    let flowsom = read_flowsom(&directory.join("flowsom.csv"));
    let default_labels = flowsom
        .iter()
        .map(|row| truth.population[row.1].as_str())
        .collect::<Vec<_>>();
    limits.at_least(
        "rare_default_metacluster_purity_min",
        best_purity(
            &flowsom.iter().map(|row| row.3).collect::<Vec<_>>(),
            &default_labels,
            "rare",
        ),
        "rare population metacluster purity (20 metaclusters)",
    );

    // The panel's QC policy accepts the final mask, and its counts are the
    // mask's own.
    let qc: Value =
        serde_json::from_str(&fs::read_to_string(directory.join("qc.json")).expect("qc report"))
            .expect("JSON");
    assert_eq!(qc["failures"], 0, "{qc:#}");
    let analysis = read_mask(&directory, "analysis.parquet");
    assert_eq!(qc["metrics"]["events"], rows);
    assert_eq!(qc["metrics"]["kept"], analysis.kept());

    // Every derived file and mask names the raw file as its acquisition.
    let acquisition = common::sha256_hex(&fixture("sample.fcs"));
    for name in ["s.comp.fcs", "s.t.fcs", "s.peacoqc.fcs"] {
        assert_eq!(
            common::acquisition_of(&directory.join(name)),
            acquisition,
            "{name}"
        );
    }
    for name in [
        "s.comp.masks.parquet",
        "s.peacoqc.masks.parquet",
        "clean/masks.parquet",
        "analysis.parquet",
        "membership.parquet",
    ] {
        assert_eq!(
            read_mask(&directory, name).acquisition,
            acquisition,
            "{name}"
        );
    }

    // Provenance: each stage's inputs are byte-identical to the outputs the
    // earlier stage recorded.
    common::assert_provenance_chain(&directory);
    let record = |name: &str| -> Value {
        serde_json::from_str(&fs::read_to_string(directory.join(name)).expect("record"))
            .expect("JSON")
    };
    let gate = record("counts.json.provenance.json");
    let roles = gate["inputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .map(|input| input["role"].as_str().expect("role"))
        .collect::<Vec<_>>();
    assert_eq!(roles, ["input", "gating_ml", "mask"]);
    assert_eq!(gate["algorithms"]["Gating-ML"], "2.0");
    let peacoqc = record("s.peacoqc.fcs.provenance.json");
    assert_eq!(peacoqc["config"]["channels"], FLUORESCENCE);
    assert_eq!(peacoqc["config"]["method"], "all");
    assert_eq!(peacoqc["constants"]["mad"], 6.0);
    assert_eq!(peacoqc["algorithms"]["PeacoQC"], "1.20.0");
}

/// The order the audit flagged: transform after `PeacoQC` must leave
/// `Original_ID` alone, so the `PeacoQC` mask still applies to the transformed
/// file.
#[test]
fn peacoqc_mask_applies_after_transform() {
    let directory = create_temp_dir("cytiq-engine-pipeline-peacoqc-first");
    let sample = fixture("sample.fcs");
    for step in [
        &[
            "compensate",
            "--input",
            utf8(&sample),
            "--output",
            "s.comp.fcs",
        ][..],
        &[
            "peacoqc",
            "--input",
            "s.comp.fcs",
            "--output",
            "s.peacoqc.fcs",
            "--channels",
            FLUORESCENCE,
        ],
        &[
            "transform",
            "--input",
            "s.peacoqc.fcs",
            "--output",
            "s.t.fcs",
        ],
        &[
            "gate",
            "--input",
            "s.t.fcs",
            "--gating-ml",
            utf8(&fixture("gates.xml")),
            "--output",
            "counts.json",
            "--mask",
            "s.peacoqc.masks.parquet",
        ],
    ] {
        cytiq(&directory, step);
    }

    let cleaned = read_fcs(&directory.join("s.peacoqc.fcs"));
    let transformed = read_fcs(&directory.join("s.t.fcs"));
    assert_eq!(
        column(&transformed, "Original_ID"),
        column(&cleaned, "Original_ID")
    );
    let state = cytiq_fcs::transform_state(&transformed.text)
        .expect("state should parse")
        .expect("transform state");
    assert!(!state.covers("Original_ID"));
    assert!(state.covers("FL1-A"));
}

#[test]
fn golden_chain_is_reproducible() {
    let first = create_temp_dir("cytiq-engine-pipeline-first");
    let second = create_temp_dir("cytiq-engine-pipeline-second");
    run_chain(&first);
    run_chain(&second);

    let files = cytiq_provenance::files_under(&first);
    assert!(files.len() > 10, "the chain should write its outputs");
    for file in files {
        let relative = file.strip_prefix(&first).expect("under first");
        assert_eq!(
            fs::read(&file).expect("first"),
            fs::read(second.join(relative)).expect("second run should write the same file"),
            "{} differs between runs",
            relative.display()
        );
    }
}

/// One bead population on an integer grid: area equals height, so the
/// singlet ratio has zero MAD, and the scatter is discrete.
fn write_beads(path: &Path) {
    let events = 3_000_u16;
    let jitter = |row: u16, modulus: u16, step: f32| f32::from(row % modulus) * step;
    let fsc = (0..events)
        .map(|row| 80_000.0 + jitter(row, 3, 1_000.0))
        .collect::<Vec<f32>>();
    let ssc = (0..events)
        .map(|row| 30_000.0 + jitter(row, 5, 1_000.0))
        .collect::<Vec<f32>>();
    let width = vec![50_000.0_f32; usize::from(events)];
    let fluor = |level: f32| {
        (0..events)
            .map(|row| level + jitter(row, 4, 100.0))
            .collect()
    };
    let time = (0..events).map(|row| f32::from(row) * 20.0).collect();
    let parameters = ["FSC-A", "FSC-H", "FSC-W", "SSC-A", "FL1-A", "FL2-A", "Time"]
        .into_iter()
        .zip(["", "", "", "", "CD3", "CD4", ""])
        .map(|(name, label)| {
            let mut parameter = if label.is_empty() {
                cytiq_fcs::FcsWriteParameter::new(name)
            } else {
                cytiq_fcs::FcsWriteParameter::with_label(name, label)
            };
            // An 18-bit instrument's range: without it `$PnR` is each column's
            // maximum, and the constant FSC-W would put every bead at the
            // detector limit `$PnR - 1`, a saturated scatter margin.
            parameter.range = Some(262_144.0);
            parameter
        })
        .collect::<Vec<_>>();
    let columns = vec![
        fsc.clone(),
        fsc,
        width,
        ssc,
        fluor(20_000.0),
        fluor(200.0),
        time,
    ];
    let options = cytiq_fcs::FcsWriteOptions::new()
        .with_text_keyword("$SPILLOVER", "2,FL1-A,FL2-A,1,0.1,0.05,1");
    cytiq_fcs::write_fcs3_1_f32_with_options(path, &parameters, &columns, &options)
        .expect("beads should write");
}

#[test]
fn degenerate_beads_keep_their_events() {
    let directory = create_temp_dir("cytiq-engine-pipeline-beads");
    let beads = directory.join("beads.fcs");
    write_beads(&beads);
    let steps: [&[&str]; 5] = [
        &[
            "compensate",
            "--input",
            utf8(&beads),
            "--output",
            "b.comp.fcs",
        ],
        &["transform", "--input", "b.comp.fcs", "--output", "b.t.fcs"],
        &[
            "peacoqc",
            "--input",
            "b.t.fcs",
            "--output",
            "b.peacoqc.fcs",
            "--channels",
            "FL1-A,FL2-A",
        ],
        &[
            "clean",
            "--input",
            utf8(&beads),
            "--output",
            "clean",
            "--no-viability",
        ],
        &[
            "flowsom",
            "--input",
            "b.t.fcs",
            "--output",
            "flowsom.csv",
            "--channels",
            "FL1-A,FL2-A",
        ],
    ];
    for step in steps {
        cytiq(&directory, step);
    }

    let metrics: Value = serde_json::from_str(
        &fs::read_to_string(directory.join("clean/metrics.json")).expect("metrics"),
    )
    .expect("JSON");
    let singlets = &metrics["filters"]["singlets"];
    assert_eq!(singlets["status"], "applied");
    assert_ne!(singlets["parameters"]["spread_source"], "mad");
    assert_eq!(
        metrics["kept"], 3_000,
        "a zero spread must not remove the beads"
    );
    for name in ["b.comp.masks.parquet", "b.peacoqc.masks.parquet"] {
        let mask = read_mask(&directory, name);
        assert!(mask.kept() * 2 > mask.len(), "{name} removed most beads");
    }
    assert_eq!(read_flowsom(&directory.join("flowsom.csv")).len(), 3_000);
}
