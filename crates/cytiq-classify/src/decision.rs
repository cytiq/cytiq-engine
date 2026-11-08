//! Batch-bounded evaluation of an explicitly validated decision policy.
use crate::ClassifyError;
use arrow_array::{ArrayRef, BooleanArray, Float32Array, StringArray};
use cytiq_population::decision::DecisionPolicy;
use ort::{session::Session, value::Tensor};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct DecisionSummary {
    pub policy_sha256: String,
    pub bundle_schema_version: u32,
    pub abstained: usize,
    pub ood_events: usize,
    pub ood_assessed: bool,
}

pub(crate) struct Decision {
    pub confidence: Option<f32>,
    pub ood_score: Option<f32>,
    pub is_ood: Option<bool>,
    pub status: &'static str,
    pub reason: Option<&'static str>,
}

pub(crate) fn assess(
    policy: &DecisionPolicy,
    probabilities: Option<&[f32]>,
    score: Option<f32>,
) -> Decision {
    let confidence = probabilities.map(|p| p.iter().copied().fold(0.0_f32, f32::max));
    let low = confidence.is_some_and(|c| policy.min_confidence.is_some_and(|minimum| c < minimum));
    let is_ood = score
        .zip(policy.ood.as_ref())
        .map(|(score, ood)| score >= ood.threshold);
    let reason = match (low, is_ood == Some(true)) {
        (true, true) => Some("low_confidence_and_ood"),
        (true, false) => Some("low_confidence"),
        (false, true) => Some("ood"),
        (false, false) => None,
    };
    Decision {
        confidence,
        ood_score: score,
        is_ood,
        status: if confidence.is_none() {
            "not_assessed"
        } else if reason.is_some() {
            "abstained"
        } else {
            "classified"
        },
        reason,
    }
}

pub(crate) fn columns(rows: &[Decision]) -> Vec<ArrayRef> {
    vec![
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.confidence).collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            rows.iter()
                .map(|r| r.confidence.map(|c| 1.0 - c))
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float32Array::from(
            rows.iter().map(|r| r.ood_score).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.is_ood).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.status))),
        Arc::new(StringArray::from(
            rows.iter().map(|r| r.reason).collect::<Vec<_>>(),
        )),
    ]
}

pub(crate) fn infer_ood(
    session: Option<&mut Session>,
    columns: &[Vec<f32>],
    selected: &[usize],
    batch_len: usize,
) -> Result<Vec<Option<f32>>, ClassifyError> {
    let mut scores = vec![None; batch_len];
    if let Some(session) = session.filter(|_| !selected.is_empty()) {
        let values = selected
            .iter()
            .flat_map(|i| columns.iter().map(move |c| c[*i]))
            .collect::<Vec<_>>();
        let tensor = Tensor::from_array(([selected.len(), columns.len()], values))?;
        let outputs = session.run(ort::inputs!["features" => tensor])?;
        let output = outputs
            .get("ood_score")
            .ok_or_else(|| ClassifyError::Invalid("missing ONNX OOD score".into()))?;
        let (shape, data) = output.try_extract_tensor::<f32>()?;
        let rows = i64::try_from(selected.len())
            .map_err(|_| ClassifyError::Invalid("OOD batch too large".into()))?;
        if shape.as_ref() != [rows, 1]
            || data.len() != selected.len()
            || data.iter().any(|score| !score.is_finite())
        {
            return Err(ClassifyError::Invalid(
                "invalid ONNX OOD scores; expected finite float32 [N,1]".into(),
            ));
        }
        for (index, score) in selected.iter().zip(data) {
            scores[*index] = Some(*score);
        }
    }
    Ok(scores)
}
