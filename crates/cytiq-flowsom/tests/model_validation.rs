//! `FlowSomModel::validate`: every guard rejects its defect and a consistent
//! model passes. Saved models are loaded through it, so each defect is tested
//! in a model that is otherwise valid.

use cytiq_flowsom::{
    CohortAcquisition, CohortSampling, CohortSummary, FlowSomError, FlowSomModel,
    FlowSomScaleSummary, Metaclustering,
};

fn scale(channel: &str, mean: f64, standard_deviation: f64) -> FlowSomScaleSummary {
    FlowSomScaleSummary {
        channel: channel.into(),
        mean,
        standard_deviation,
    }
}

fn acquisition(digit: char, events: usize, sampled: usize) -> CohortAcquisition {
    CohortAcquisition {
        acquisition: digit.to_string().repeat(64),
        events,
        sampled,
    }
}

/// A per-file model: a small file below the quota, a large one capped by it,
/// a labelled and an unlabelled channel, and a constant (zero-deviation)
/// channel, which training produces and apply leaves unscaled.
fn valid() -> FlowSomModel {
    FlowSomModel {
        schema_version: 1,
        channels: vec!["A".into(), "B".into()],
        channel_labels: Some(vec![vec!["CD3".into()], vec![]]),
        xdim: 3,
        ydim: 1,
        seed: 1,
        scale_input: true,
        scale: vec![scale("A", 100.0, 10.0), scale("B", 200.0, 0.0)],
        codebook: vec![vec![0.0, 0.0], vec![1.0, 1.0], vec![2.0, 2.0]],
        node_metaclusters: vec![1, 2, 2],
        metaclustering: Metaclustering::Average,
        training_events: 70,
        transform: None,
        normalization_model: None,
        cohort: Some(CohortSummary {
            sampling: CohortSampling::PerFile,
            per_file_quota: 50,
            acquisitions: vec![acquisition('a', 20, 20), acquisition('b', 600, 50)],
        }),
    }
}

#[test]
fn consistent_models_pass() {
    valid().validate().expect("per-file model");
    let mut pooled = valid();
    pooled.cohort = None;
    pooled.channel_labels = None;
    pooled.validate().expect("pooled model without labels");
}

const MODEL: &str = "invalid SOM model dimensions, scale or labels";
const COHORT: &str = "invalid cohort sampling summary";

/// A defect, the message that names it, and how to introduce it.
type Defect = (&'static str, &'static str, fn(&mut FlowSomModel));

const DEFECTS: &[Defect] = &[
    ("grid overflow", "SOM grid overflow", |m| {
        m.xdim = usize::MAX;
        m.ydim = 2;
    }),
    ("unknown schema", MODEL, |m| m.schema_version = 2),
    // Without nodes or channels the codebook check would also fail, with a
    // vaguer message; these must be named as dimension errors.
    ("empty grid", MODEL, |m| {
        m.xdim = 0;
        m.codebook.clear();
        m.node_metaclusters.clear();
    }),
    ("no channels", MODEL, |m| {
        m.channels.clear();
        m.scale.clear();
        m.channel_labels = None;
    }),
    ("extra codebook node", MODEL, |m| {
        m.codebook.push(vec![3.0, 3.0]);
    }),
    ("missing node label", MODEL, |m| {
        m.node_metaclusters.pop();
    }),
    ("missing scale", MODEL, |m| {
        m.scale.pop();
    }),
    ("no training events", MODEL, |m| {
        m.training_events = 0;
        m.cohort = None;
    }),
    ("blank channel", MODEL, |m| {
        m.channels[0] = " ".into();
        m.scale[0].channel = " ".into();
    }),
    ("duplicate channel", MODEL, |m| {
        m.channels[1] = "A".into();
        m.scale[1].channel = "A".into();
    }),
    ("label count", MODEL, |m| {
        m.channel_labels = Some(vec![vec!["CD3".into()]]);
    }),
    ("blank label", MODEL, |m| {
        m.channel_labels = Some(vec![vec!["CD3".into()], vec![" ".into()]]);
    }),
    ("scale of another channel", MODEL, |m| {
        m.scale[1].channel = "C".into();
    }),
    ("non-finite mean", MODEL, |m| m.scale[0].mean = f64::NAN),
    ("non-finite deviation", MODEL, |m| {
        m.scale[0].standard_deviation = f64::INFINITY;
    }),
    ("negative deviation", MODEL, |m| {
        m.scale[0].standard_deviation = -1.0;
    }),
    ("metacluster zero", MODEL, |m| {
        m.node_metaclusters = vec![0, 1, 1];
    }),
    ("metacluster beyond the grid", MODEL, |m| {
        m.node_metaclusters = vec![1, 2, 4];
    }),
    (
        "metacluster gap",
        "metacluster labels must be contiguous",
        |m| m.node_metaclusters = vec![1, 3, 3],
    ),
    ("non-finite codebook", "matrix must be nonempty", |m| {
        m.codebook[0][0] = f64::NAN;
    }),
    ("pooled cohort summary", COHORT, |m| {
        m.cohort.as_mut().unwrap().sampling = CohortSampling::Pooled;
    }),
    ("zero quota", COHORT, |m| {
        let cohort = m.cohort.as_mut().unwrap();
        cohort.per_file_quota = 0;
        for entry in &mut cohort.acquisitions {
            entry.sampled = 0;
        }
    }),
    ("no acquisitions", COHORT, |m| {
        m.cohort.as_mut().unwrap().acquisitions.clear();
    }),
    ("repeated acquisition", COHORT, |m| {
        let acquisitions = &mut m.cohort.as_mut().unwrap().acquisitions;
        acquisitions[1].acquisition = acquisitions[0].acquisition.clone();
    }),
    ("acquisition not a SHA-256", COHORT, |m| {
        m.cohort.as_mut().unwrap().acquisitions[0].acquisition = "a".repeat(63);
    }),
    ("sampled above the quota", COHORT, |m| {
        m.cohort.as_mut().unwrap().acquisitions[1].sampled = 60;
        m.training_events = 80;
    }),
    ("sampled below the available events", COHORT, |m| {
        m.cohort.as_mut().unwrap().acquisitions[0].sampled = 10;
        m.training_events = 60;
    }),
    ("training events differ from the draw", COHORT, |m| {
        m.training_events = 71;
    }),
    // A saved model is untrusted: an overflowing draw must not panic or wrap.
    ("draw overflows", COHORT, |m| {
        let cohort = m.cohort.as_mut().unwrap();
        cohort.per_file_quota = usize::MAX;
        for entry in &mut cohort.acquisitions {
            entry.events = usize::MAX;
            entry.sampled = usize::MAX;
        }
    }),
];

#[test]
fn each_defect_is_rejected_with_its_message() {
    for (defect, expected, corrupt) in DEFECTS {
        let mut model = valid();
        corrupt(&mut model);
        match model.validate() {
            Err(FlowSomError::InvalidConfig(message)) => {
                assert!(message.contains(expected), "{defect}: {message}");
            }
            other => panic!("{defect}: expected `{expected}`, got {other:?}"),
        }
    }
}
