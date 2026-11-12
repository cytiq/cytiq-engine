use cytiq_flowsom::{
    FlowSomError, FlowSomModel, FlowSomScaleSummary, Metaclustering, PopulationStatistics,
};

fn model() -> FlowSomModel {
    FlowSomModel {
        schema_version: 1,
        channels: vec!["A".into(), "B".into()],
        channel_labels: None,
        xdim: 3,
        ydim: 1,
        seed: 42,
        scale_input: true,
        scale: vec![
            FlowSomScaleSummary {
                channel: "A".into(),
                mean: 100.0,
                standard_deviation: 10.0,
            },
            FlowSomScaleSummary {
                channel: "B".into(),
                mean: 200.0,
                standard_deviation: 20.0,
            },
        ],
        codebook: vec![vec![0.0, 0.0], vec![10.0, 10.0], vec![100.0, 100.0]],
        node_metaclusters: vec![1, 2, 3],
        metaclustering: Metaclustering::Average,
        training_events: 10,
        transform: None,
        normalization_model: None,
        cohort: None,
    }
}

#[test]
fn populations_report_original_signal_means_and_empty_clusters() {
    let stats = model()
        .populations(&[vec![100.0, 200.0], vec![120.0, 240.0], vec![200.0, 400.0]])
        .unwrap();
    assert_eq!(
        stats,
        [
            PopulationStatistics {
                metacluster: 1,
                events: 2,
                frequency: 2.0 / 3.0,
                marker_means: vec![110.0, 220.0]
            },
            PopulationStatistics {
                metacluster: 2,
                events: 1,
                frequency: 1.0 / 3.0,
                marker_means: vec![200.0, 400.0]
            },
            PopulationStatistics {
                metacluster: 3,
                events: 0,
                frequency: 0.0,
                marker_means: vec![0.0, 0.0]
            },
        ]
    );
}

#[test]
fn populations_propagate_model_and_matrix_validation_errors() {
    let mut model = model();
    for values in [vec![], vec![vec![1.0]], vec![vec![f64::NAN, 2.0]]] {
        assert!(matches!(
            model.populations(&values),
            Err(FlowSomError::InvalidConfig(_))
        ));
    }
    model.node_metaclusters = vec![0, 2, 3];
    assert!(model.populations(&[vec![1.0, 2.0]]).is_err());
}
