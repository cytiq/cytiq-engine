//! In-memory representation of a `GatingML` 2.0 gating strategy.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::GatingError;
use crate::transforms::Transform;

/// How a dimension's events are compensated before transformation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompensationRef {
    /// Raw (uncompensated) events.
    Uncompensated,
    /// The spillover matrix stored in the FCS `$SPILLOVER`/`$SPILL` keyword.
    Fcs,
    /// A `transforms:spectrumMatrix` declared in the `GatingML` document.
    Matrix(String),
}

/// A gate dimension: an FCS channel plus its compensation, transformation and
/// optional rectangle bounds (in transformed space).
#[derive(Clone, Debug, PartialEq)]
pub struct Dimension {
    pub name: String,
    pub compensation: CompensationRef,
    pub transform: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp {
    And,
    Or,
    Not,
}

/// A reference from a Boolean gate to another gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateRef {
    pub id: String,
    pub complement: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GateKind {
    /// Range (1-D), rectangle (2-D) or hyper-rectangle gate: `min <= x < max`.
    Rectangle { dimensions: Vec<Dimension> },
    /// 2-D polygon: odd winding number (Sunday's algorithm); points on the
    /// left and bottom edges are inside, on the right and top edges outside.
    Polygon {
        dimensions: [Dimension; 2],
        vertices: Vec<[f64; 2]>,
    },
    /// n-D ellipsoid: `(x-μ)ᵀ Σ⁻¹ (x-μ) <= distance_square`.
    Ellipsoid {
        dimensions: Vec<Dimension>,
        mean: Vec<f64>,
        /// Row-major `n × n` covariance matrix.
        covariance: Vec<f64>,
        distance_square: f64,
    },
    /// Boolean combination of other gates.
    Boolean { op: BooleanOp, refs: Vec<GateRef> },
    /// One quadrant of a Gating-ML `QuadrantGate`: the box between the
    /// divider values around its positions, evaluated like a rectangle
    /// (`min <= x < max`). Only quadrants are gates; `definition` is the whole
    /// `QuadrantGate`, shared by its quadrants and written back once.
    Quadrant {
        definition: Arc<QuadrantGate>,
        /// One per position, with bounds derived from the dividers.
        dimensions: Vec<Dimension>,
    },
}

/// A Gating-ML 2.0 `QuadrantGate` as written: dividers split dimensions at
/// fixed values, and each quadrant picks one interval per divider through a
/// location inside it. Its own id names no population; quadrants and their
/// descendants are the gates.
#[derive(Clone, Debug, PartialEq)]
pub struct QuadrantGate {
    pub id: String,
    pub parent_id: Option<String>,
    pub dividers: Vec<QuadrantDivider>,
    pub quadrants: Vec<Quadrant>,
}

/// A `gating:divider`: a dimension (its `min`/`max` are unused) and the
/// values that split it, in document order.
#[derive(Clone, Debug, PartialEq)]
pub struct QuadrantDivider {
    pub id: String,
    pub dimension: Dimension,
    pub values: Vec<f64>,
}

/// A `gating:Quadrant`: `(divider id, location)` per `gating:position`.
#[derive(Clone, Debug, PartialEq)]
pub struct Quadrant {
    pub id: String,
    pub positions: Vec<(String, f64)>,
}

impl QuadrantGate {
    /// The quadrant gates, in document order, with the `FlowKit` interval
    /// rule: on each divider's sorted values, `min` is the last value at or
    /// below the location and `max` the first value above it.
    ///
    /// # Errors
    ///
    /// Returns [`GatingError::InvalidGate`] for a definition without dividers
    /// or quadrants, duplicate divider ids, two dividers on one channel, a
    /// divider without values, a quadrant without positions, or a position
    /// on an unknown or repeated divider.
    pub fn gates(self) -> Result<Vec<Gate>, GatingError> {
        let invalid = |reason: String| GatingError::InvalidGate {
            id: self.id.clone(),
            reason,
        };
        if self.dividers.is_empty() || self.quadrants.is_empty() {
            return Err(invalid(
                "needs at least one divider and one quadrant".to_owned(),
            ));
        }
        let mut divider_ids = HashMap::new();
        for (index, divider) in self.dividers.iter().enumerate() {
            if divider.values.is_empty() {
                return Err(invalid(format!("divider `{}` has no value", divider.id)));
            }
            if divider_ids.insert(divider.id.as_str(), index).is_some() {
                return Err(invalid(format!("duplicate divider `{}`", divider.id)));
            }
            if self.dividers[..index]
                .iter()
                .any(|other| other.dimension.name == divider.dimension.name)
            {
                return Err(invalid(format!(
                    "two dividers split channel `{}`",
                    divider.dimension.name
                )));
            }
        }
        let mut gates = Vec::with_capacity(self.quadrants.len());
        for quadrant in &self.quadrants {
            if quadrant.positions.is_empty() {
                return Err(invalid(format!(
                    "quadrant `{}` has no position",
                    quadrant.id
                )));
            }
            let mut used = Vec::new();
            let mut dimensions = Vec::with_capacity(quadrant.positions.len());
            for (divider_id, location) in &quadrant.positions {
                let Some(&index) = divider_ids.get(divider_id.as_str()) else {
                    return Err(invalid(format!(
                        "quadrant `{}` references unknown divider `{divider_id}`",
                        quadrant.id
                    )));
                };
                if used.contains(&index) {
                    return Err(invalid(format!(
                        "quadrant `{}` repeats divider `{divider_id}`",
                        quadrant.id
                    )));
                }
                used.push(index);
                let divider = &self.dividers[index];
                let mut values = divider.values.clone();
                values.sort_by(f64::total_cmp);
                let mut dimension = divider.dimension.clone();
                dimension.min = values
                    .iter()
                    .copied()
                    .take_while(|v| *v <= *location)
                    .last();
                dimension.max = values.iter().copied().find(|v| *v > *location);
                dimensions.push(dimension);
            }
            gates.push((quadrant.id.clone(), dimensions));
        }
        let parent_id = self.parent_id.clone();
        let definition = Arc::new(self);
        Ok(gates
            .into_iter()
            .map(|(id, dimensions)| Gate {
                id,
                parent_id: parent_id.clone(),
                kind: GateKind::Quadrant {
                    definition: Arc::clone(&definition),
                    dimensions,
                },
            })
            .collect())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: GateKind,
}

impl Gate {
    /// Dimensions read by the gate (empty for Boolean gates).
    #[must_use]
    pub fn dimensions(&self) -> &[Dimension] {
        match &self.kind {
            GateKind::Rectangle { dimensions }
            | GateKind::Ellipsoid { dimensions, .. }
            | GateKind::Quadrant { dimensions, .. } => dimensions,
            GateKind::Polygon { dimensions, .. } => dimensions,
            GateKind::Boolean { .. } => &[],
        }
    }

    /// Short gate type name, matching the `GatingML` element name.
    #[must_use]
    pub const fn gate_type(&self) -> &'static str {
        match self.kind {
            GateKind::Rectangle { .. } => "RectangleGate",
            GateKind::Polygon { .. } => "PolygonGate",
            GateKind::Ellipsoid { .. } => "EllipsoidGate",
            GateKind::Boolean { .. } => "BooleanGate",
            GateKind::Quadrant { .. } => "Quadrant",
        }
    }
}

/// A `transforms:spectrumMatrix`: rows are fluorochromes, columns detectors.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumMatrix {
    pub fluorochromes: Vec<String>,
    pub detectors: Vec<String>,
    /// Row-major `fluorochromes × detectors` coefficients.
    pub values: Vec<f64>,
}

/// A parsed gating strategy. Gates are stored in evaluation order: every
/// gate appears after its parent and after every gate it references.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GatingStrategy {
    pub(crate) gates: Vec<Gate>,
    pub(crate) index: HashMap<String, usize>,
    pub(crate) transforms: HashMap<String, Transform>,
    pub(crate) matrices: HashMap<String, SpectrumMatrix>,
}

impl GatingStrategy {
    /// Build a strategy from gates in any order, validating references the
    /// same way the `GatingML` parser does.
    ///
    /// # Errors
    ///
    /// Returns an error on duplicate gate ids, dangling gate, transformation
    /// or compensation references, or a cyclic hierarchy.
    pub fn new(
        gates: Vec<Gate>,
        transforms: HashMap<String, Transform>,
        matrices: HashMap<String, SpectrumMatrix>,
    ) -> Result<Self, GatingError> {
        crate::parse::build_strategy(gates, transforms, matrices)
    }

    /// Gates in evaluation order.
    #[must_use]
    pub fn gates(&self) -> &[Gate] {
        &self.gates
    }

    #[must_use]
    pub fn gate(&self, id: &str) -> Option<&Gate> {
        self.index.get(id).map(|index| &self.gates[*index])
    }

    #[must_use]
    pub fn transform(&self, id: &str) -> Option<&Transform> {
        self.transforms.get(id)
    }

    /// Transformations keyed by id.
    #[must_use]
    pub const fn transforms(&self) -> &HashMap<String, Transform> {
        &self.transforms
    }

    /// Spectrum matrices keyed by id.
    #[must_use]
    pub const fn matrices(&self) -> &HashMap<String, SpectrumMatrix> {
        &self.matrices
    }

    #[must_use]
    pub fn matrix(&self, id: &str) -> Option<&SpectrumMatrix> {
        self.matrices.get(id)
    }

    /// Gate ids from the root down to (and including) `id`. A quadrant is
    /// preceded by its `QuadrantGate` id, as in `FlowKit`.
    #[must_use]
    pub fn gate_path(&self, id: &str) -> Vec<String> {
        let mut path = Vec::new();
        let mut current = self.gate(id);
        while let Some(gate) = current {
            path.push(gate.id.clone());
            if let GateKind::Quadrant { definition, .. } = &gate.kind {
                path.push(definition.id.clone());
            }
            current = gate
                .parent_id
                .as_deref()
                .and_then(|parent| self.gate(parent));
        }
        path.reverse();
        path
    }
}
