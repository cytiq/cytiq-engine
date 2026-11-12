//! Peak heap of the memory-heavy library entry points on fixed synthetic
//! inputs, measured by a counting allocator. Allocation sizes do not depend on
//! timing, so unlike RSS the peak is reproducible, and a new full copy of an
//! event matrix shows up as a step of known size. Wall time and RSS on larger
//! inputs are measured by the paired harness in `parity/scientific`.
//!
//! The budgets are the peaks measured when they were last lowered (see each
//! constant) plus 10%, rounded up to MiB. Lower a budget with the change that
//! lowers its peak; raise one only with a reviewed reason, never to make a run
//! pass. This file holds one test: the allocator counts every thread of the
//! process, and the test harness runs each test binary in its own process.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use cytiq_fcs::{FcsWriteOptions, FcsWriteParameter};

#[allow(dead_code)]
#[path = "../../cytiq-cytonorm/tests/support/mod.rs"]
mod cytonorm_support;

/// Forwards every call to [`System`] and tracks live and peak bytes.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::SeqCst) + bytes;
    PEAK.fetch_max(live, Ordering::SeqCst);
}

fn shrink(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::SeqCst);
}

// SAFETY: every method passes its arguments unchanged to `System`, whose
// contract the caller already upholds, and returns its result unchanged. The
// bookkeeping only updates atomics, which never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged (see the impl).
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged (see the impl).
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged (see the impl).
        unsafe { System.dealloc(pointer, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged (see the impl).
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grow(new_size - layout.size());
            } else {
                shrink(layout.size() - new_size);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MIB: usize = 1 << 20;
const EVENTS: usize = 200_000;
const CHANNELS: usize = 8;

/// Peak bytes allocated above the live heap at the start of `work`.
fn peak_during(work: impl FnOnce()) -> usize {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    work();
    PEAK.load(Ordering::SeqCst) - base
}

/// Deterministic values with spread, a few modes and drift over time.
fn signal(channel: usize, event: usize, state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let noise = f64::from(u32::try_from(*state >> 40).expect("24 bits")) / f64::from(1_u32 << 24);
    let mode = f64::from(u8::try_from((event + channel) % 3).expect("small"));
    #[allow(clippy::cast_precision_loss)]
    let drift = event as f64 / EVENTS as f64;
    1.0 + mode * 1.5 + noise + 0.2 * drift
}

/// An FCS with `Time` and `channels` signal channels, recorded as compensated
/// and arcsinh-transformed so every stage accepts it as signal.
fn write_input(path: &Path, names: &[String], seed: u64) {
    let mut state = seed;
    #[allow(clippy::cast_precision_loss)]
    let mut columns = vec![(0..EVENTS).map(|event| event as f64).collect::<Vec<_>>()];
    for channel in 0..names.len() {
        columns.push(
            (0..EVENTS)
                .map(|event| signal(channel, event, &mut state))
                .collect(),
        );
    }
    let mut parameters = vec![FcsWriteParameter::new("Time")];
    parameters.extend(names.iter().map(FcsWriteParameter::new));
    let channels = serde_json::json!(names);
    let options = FcsWriteOptions::new()
        .with_text_keyword(
            "CYTIQ_COMPENSATION",
            serde_json::json!({"source": "fcs", "channels": channels}).to_string(),
        )
        .with_text_keyword(
            "CYTIQ_TRANSFORM",
            serde_json::json!({"tool": "cytiq transform", "version": "0.1.0", "method": "arcsinh",
                "parameters": {"cofactor": 150.0}, "files": [{"channels": channels}]})
            .to_string(),
        );
    cytiq_fcs::write_fcs3_1_exact_with_options(path, &parameters, &columns, &options)
        .expect("synthetic FCS writes");
}

/// A `CytoNorm` apply input of the `cytonorm_support` panel (channels `A`,
/// `B` as CD3 and CD4) with `EVENTS` events and its contract keywords.
fn write_cytonorm_input(path: &Path) {
    let mut state = 7;
    let mut column = |channel| {
        (0..EVENTS)
            .map(|event| signal(channel, event, &mut state))
            .collect::<Vec<_>>()
    };
    let (marker, functional) = (column(0), column(1));
    #[allow(clippy::cast_precision_loss)]
    let columns = [
        marker,
        functional,
        (0..EVENTS).map(|event| event as f64).collect(),
        (1..=EVENTS).map(|event| event as f64).collect(),
    ];
    let channels = serde_json::json!(["A", "B"]);
    let options = FcsWriteOptions::new()
        .with_text_keyword("$CYT", "synthetic")
        .with_text_keyword(
            "CYTIQ_COMPENSATION",
            serde_json::json!({"source": "fcs", "channels": channels}).to_string(),
        )
        .with_text_keyword(
            "CYTIQ_TRANSFORM",
            serde_json::json!({"tool": "cytiq transform", "version": "0.1.0", "method": "arcsinh",
                "parameters": {"cofactor": 150.0}, "files": [{"channels": channels}]})
            .to_string(),
        )
        .with_text_keyword(
            "CYTIQ_MARGINS",
            serde_json::json!({"A": {"lower": 0.0, "upper": 20.0}, "B": {"lower": 0.0, "upper": 20.0}})
                .to_string(),
        );
    let parameters = [
        FcsWriteParameter::with_label("A", "CD3"),
        FcsWriteParameter::with_label("B", "CD4"),
        FcsWriteParameter::new("Time"),
        FcsWriteParameter::new("Original_ID"),
    ];
    cytiq_fcs::write_fcs3_1_exact_with_options(path, &parameters, &columns, &options)
        .expect("CytoNorm input writes");
}

/// The peak of one entry point, checked against its budget. `input_bytes`
/// is one f64 copy of the selected values: the peak cannot be lower without
/// a broken counter.
#[allow(clippy::cast_precision_loss)]
fn check(name: &str, peak: usize, input_bytes: usize, budget_mib: usize) {
    eprintln!(
        "{name}: peak {peak} B ({:.1} MiB)",
        peak as f64 / MIB as f64
    );
    assert!(peak >= input_bytes, "{name}: implausible peak {peak} B");
    assert!(
        peak <= budget_mib * MIB,
        "{name}: peak heap {:.1} MiB exceeds its {budget_mib} MiB budget",
        peak as f64 / MIB as f64
    );
}

// Peaks measured on macOS arm64 (debug test profile, one rayon thread),
// identical across runs. With the library code of eb9ebd2: PeacoQC 72,150,023 B, FlowSOM
// 33,611,512 B, CytoNorm apply 35,269,712 B. PeacoQC without the channel
// copies: 46,549,575 B. CytoNorm apply normalizing in place: 34,151,514 B.

/// `PeacoQC` on 200k events and 8 channels (all methods, cleaned FCS, masks and
/// summary).
const PEACOQC_BUDGET_MIB: usize = 49;
/// `FlowSOM` assignment of 200k events on 8 channels, 10k training events.
const FLOWSOM_BUDGET_MIB: usize = 36;
/// `CytoNorm` apply to 200k events, after a two-batch training on small files.
const CYTONORM_APPLY_BUDGET_MIB: usize = 36;

#[test]
fn memory_heavy_entry_points_stay_within_their_heap_budgets() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool");
    let directory = tempfile::tempdir().expect("temp dir");
    let names = (1..=CHANNELS)
        .map(|index| format!("FL{index}-A"))
        .collect::<Vec<_>>();
    let input = directory.path().join("sample.fcs");
    write_input(&input, &names, 42);
    let input_bytes = EVENTS * CHANNELS * std::mem::size_of::<f64>();

    pool.install(|| {
        let config = cytiq_peacoqc::PeacoQcConfig::new(names.clone());
        let output = directory.path().join("peacoqc.fcs");
        let peak = peak_during(|| {
            cytiq_peacoqc::PeacoQcRunner::run(&input, &output, &config).expect("peacoqc runs");
        });
        check("peacoqc", peak, input_bytes, PEACOQC_BUDGET_MIB);

        let mut config = cytiq_flowsom::FlowSomConfig::new(names.clone());
        config.training_events = Some(10_000);
        let output = directory.path().join("flowsom.csv");
        let peak = peak_during(|| {
            cytiq_flowsom::FlowSomRunner::run(&input, &output, &config, false)
                .expect("flowsom runs");
        });
        check("flowsom", peak, input_bytes, FLOWSOM_BUDGET_MIB);

        let (manifest, _) = cytonorm_support::design(directory.path(), "aggregate");
        let model = directory.path().join("model");
        cytiq_cytonorm::CytoNormRunner::train(&cytiq_cytonorm::TrainConfig {
            manifest,
            output: model.clone(),
            debug_dir: None,
            overwrite: false,
        })
        .expect("cytonorm trains");
        let apply_input = directory.path().join("apply.fcs");
        write_cytonorm_input(&apply_input);
        let apply = cytiq_cytonorm::ApplyConfig {
            input: apply_input,
            model,
            batch: "b".into(),
            output: directory.path().join("normalized.fcs"),
            debug_dir: None,
            overwrite: false,
        };
        let peak = peak_during(|| {
            cytiq_cytonorm::CytoNormRunner::apply(&apply).expect("cytonorm applies");
        });
        check(
            "cytonorm apply",
            peak,
            EVENTS * 2 * std::mem::size_of::<f64>(),
            CYTONORM_APPLY_BUDGET_MIB,
        );
    });
}
