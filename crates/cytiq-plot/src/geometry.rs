//! Gate outlines as polylines in the unit square.
//!
//! Vertices, bounds, means and covariances are in the space of each gate
//! dimension; the plot axis bound to that dimension holds values in the same
//! space, so outlines only go through the axes' display scales. Straight edges
//! are densified when a display scale is non-linear, so they bend with it.

use cytiq_gating::{Dimension, Gate, GateKind};

use crate::axis::Axis;
use crate::spec::Polyline;

/// Points on an ellipse outline.
const ELLIPSE_POINTS: usize = 256;
/// Segments per polygon edge on non-linear axes.
const EDGE_STEPS: usize = 64;
/// Open rectangle bounds and range-gate lines reach this far past the plot
/// edge (clipped when drawn).
const BEYOND: f64 = 0.02;

/// Why a gate cannot be drawn on a plot.
#[derive(Debug, PartialEq)]
pub(crate) enum GeometryError {
    /// The gate uses a dimension that is not one of the plot axes.
    Mismatch(String),
    /// The gate has no drawable 2-D geometry.
    Unsupported(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    X,
    Y,
}

/// The gate dimension each axis shows (`None` for a plain channel axis).
pub(crate) struct AxisDimensions<'a> {
    pub x: Option<&'a Dimension>,
    pub y: Option<&'a Dimension>,
}

/// Same data space: same channel, compensation and transformation.
pub(crate) fn same_dimension(a: &Dimension, b: &Dimension) -> bool {
    a.name == b.name && a.compensation == b.compensation && a.transform == b.transform
}

fn side(dimension: &Dimension, axes: &AxisDimensions<'_>) -> Result<Side, GeometryError> {
    if axes.x.is_some_and(|x| same_dimension(x, dimension)) {
        Ok(Side::X)
    } else if axes.y.is_some_and(|y| same_dimension(y, dimension)) {
        Ok(Side::Y)
    } else {
        Err(GeometryError::Mismatch(format!(
            "uses dimension `{}` (compensation {:?}, transformation {}), which is not an axis of this plot",
            dimension.name,
            dimension.compensation,
            dimension.transform.as_deref().unwrap_or("none"),
        )))
    }
}

fn two_sides(dimensions: &[Dimension], axes: &AxisDimensions<'_>) -> Result<bool, GeometryError> {
    match (side(&dimensions[0], axes)?, side(&dimensions[1], axes)?) {
        (Side::X, Side::Y) => Ok(false),
        (Side::Y, Side::X) => Ok(true),
        _ => Err(GeometryError::Mismatch(
            "uses the same plot axis for both of its dimensions".to_owned(),
        )),
    }
}

/// Outline of `gate` in unit coordinates of the plot axes.
pub(crate) fn outline(
    gate: &Gate,
    axes: &AxisDimensions<'_>,
    x: &Axis,
    y: &Axis,
) -> Result<Vec<Polyline>, GeometryError> {
    match &gate.kind {
        GateKind::Boolean { .. } => Err(GeometryError::Unsupported(
            "is a Boolean gate without geometry; use it as the population instead".to_owned(),
        )),
        GateKind::Polygon {
            dimensions,
            vertices,
        } => polygon(dimensions, vertices, axes, x, y),
        GateKind::Rectangle { dimensions } | GateKind::Quadrant { dimensions, .. } => {
            match dimensions.as_slice() {
                [dimension] => range_lines(dimension, axes, x, y),
                [_, _] => rectangle(dimensions, axes, x, y),
                _ => Err(GeometryError::Unsupported(format!(
                    "has {} dimensions; only gates on one or two plot axes can be drawn",
                    dimensions.len()
                ))),
            }
        }
        GateKind::Ellipsoid {
            dimensions,
            mean,
            covariance,
            distance_square,
        } => {
            if dimensions.len() != 2 {
                return Err(GeometryError::Unsupported(format!(
                    "is a {}-dimensional ellipsoid; only two-dimensional ellipsoids can be drawn",
                    dimensions.len()
                )));
            }
            let (mean, covariance) = if two_sides(dimensions, axes)? {
                (
                    [mean[1], mean[0]],
                    [covariance[3], covariance[2], covariance[1], covariance[0]],
                )
            } else {
                (
                    [mean[0], mean[1]],
                    [covariance[0], covariance[1], covariance[2], covariance[3]],
                )
            };
            let points = ellipse(mean, covariance, *distance_square).ok_or_else(|| {
                GeometryError::Unsupported(
                    "has a covariance matrix that is not positive definite".to_owned(),
                )
            })?;
            Ok(vec![Polyline {
                points: points
                    .into_iter()
                    .map(|[vx, vy]| [x.unit(vx), y.unit(vy)])
                    .collect(),
                closed: true,
            }])
        }
    }
}

/// Where a gate's label goes: the top-left corner of the gate's region on the
/// plot, clamped onto it. A range gate's lines do not say which side its
/// events are on (`T` ≥ 3 and `NonT` < 3 draw the same line), so its corner
/// comes from its bounds: the lower bound on its axis, else the plot edge.
/// Other gates use the top-left corner of their outline. `None` when nothing
/// is drawn.
pub(crate) fn label_anchor(
    gate: &Gate,
    axes: &AxisDimensions<'_>,
    x: &Axis,
    y: &Axis,
    paths: &[Polyline],
) -> Option<[f64; 2]> {
    if paths.is_empty() {
        return None;
    }
    let [left, top] = match &gate.kind {
        GateKind::Rectangle { dimensions } | GateKind::Quadrant { dimensions, .. }
            if dimensions.len() == 1 =>
        {
            let dimension = &dimensions[0];
            let side = side(dimension, axes).ok()?;
            let unit = |axis: &Axis, bound: Option<f64>| {
                bound
                    .map(|bound| axis.unit(bound))
                    .filter(|unit| unit.is_finite())
            };
            match side {
                Side::X => [unit(x, dimension.min).unwrap_or(0.0), 1.0],
                Side::Y => [0.0, unit(y, dimension.max).unwrap_or(1.0)],
            }
        }
        _ => paths.iter().flat_map(|path| path.points.iter()).fold(
            [f64::INFINITY, f64::NEG_INFINITY],
            |[left, top], &[px, py]| [left.min(px), top.max(py)],
        ),
    };
    Some([left.clamp(0.0, 1.0), top.clamp(0.0, 1.0)])
}

fn polygon(
    dimensions: &[Dimension; 2],
    vertices: &[[f64; 2]],
    axes: &AxisDimensions<'_>,
    x: &Axis,
    y: &Axis,
) -> Result<Vec<Polyline>, GeometryError> {
    let swapped = two_sides(dimensions, axes)?;
    let vertices = vertices
        .iter()
        .map(|&[a, b]| if swapped { [b, a] } else { [a, b] })
        .collect::<Vec<_>>();
    let points = if x.is_linear() && y.is_linear() {
        vertices
    } else {
        densify(&vertices)
    };
    Ok(vec![Polyline {
        points: points
            .into_iter()
            .map(|[vx, vy]| [x.unit(vx), y.unit(vy)])
            .collect(),
        closed: true,
    }])
}

/// A one-dimensional range gate: a line across the plot at each bound.
fn range_lines(
    dimension: &Dimension,
    axes: &AxisDimensions<'_>,
    x: &Axis,
    y: &Axis,
) -> Result<Vec<Polyline>, GeometryError> {
    let side = side(dimension, axes)?;
    let bound_axis = if side == Side::X { x } else { y };
    Ok([dimension.min, dimension.max]
        .into_iter()
        .flatten()
        .map(|bound| bound_axis.unit(bound))
        .filter(|unit| unit.is_finite())
        .map(|unit| {
            let (start, end) = if side == Side::X {
                ([unit, -BEYOND], [unit, 1.0 + BEYOND])
            } else {
                ([-BEYOND, unit], [1.0 + BEYOND, unit])
            };
            Polyline {
                points: vec![start, end],
                closed: false,
            }
        })
        .collect())
}

/// A two-dimensional rectangle; open bounds reach past the plot edge.
fn rectangle(
    dimensions: &[Dimension],
    axes: &AxisDimensions<'_>,
    x: &Axis,
    y: &Axis,
) -> Result<Vec<Polyline>, GeometryError> {
    let (dx, dy) = if two_sides(dimensions, axes)? {
        (&dimensions[1], &dimensions[0])
    } else {
        (&dimensions[0], &dimensions[1])
    };
    let bound = |axis: &Axis, value: Option<f64>, open: f64| {
        value
            .map(|value| axis.unit(value))
            .filter(|unit| unit.is_finite())
            .unwrap_or(open)
    };
    let (left, right) = (bound(x, dx.min, -BEYOND), bound(x, dx.max, 1.0 + BEYOND));
    let (bottom, top) = (bound(y, dy.min, -BEYOND), bound(y, dy.max, 1.0 + BEYOND));
    Ok(vec![Polyline {
        points: vec![[left, bottom], [right, bottom], [right, top], [left, top]],
        closed: true,
    }])
}

/// Points of a closed polygon with every edge (including the closing one)
/// split into [`EDGE_STEPS`] segments.
fn densify(vertices: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut points = Vec::with_capacity(vertices.len() * EDGE_STEPS);
    for (index, &[x0, y0]) in vertices.iter().enumerate() {
        let [x1, y1] = vertices[(index + 1) % vertices.len()];
        for step in 0..EDGE_STEPS {
            #[allow(clippy::cast_precision_loss, reason = "step < 64")]
            let t = step as f64 / EDGE_STEPS as f64;
            points.push([x0 + t * (x1 - x0), y0 + t * (y1 - y0)]);
        }
    }
    points
}

/// Boundary `(p − μ)ᵀ Σ⁻¹ (p − μ) = d²` of a 2-D ellipsoid gate:
/// `μ + d · L (cos θ, sin θ)` with `L Lᵀ = Σ` (Cholesky). `None` when `Σ` is
/// not positive definite or `d²` is not positive.
pub(crate) fn ellipse(
    mean: [f64; 2],
    covariance: [f64; 4],
    distance_square: f64,
) -> Option<Vec<[f64; 2]>> {
    let [a, b, _, d] = covariance;
    if !(a > 0.0 && distance_square > 0.0) {
        return None;
    }
    let l11 = a.sqrt();
    let l21 = b / l11;
    let rest = d - l21 * l21;
    if rest.is_nan() || rest <= 0.0 {
        return None;
    }
    let l22 = rest.sqrt();
    let radius = distance_square.sqrt();
    Some(
        (0..ELLIPSE_POINTS)
            .map(|index| {
                #[allow(clippy::cast_precision_loss, reason = "index < 256")]
                let theta = std::f64::consts::TAU * index as f64 / ELLIPSE_POINTS as f64;
                let (sin, cos) = theta.sin_cos();
                [
                    mean[0] + radius * l11 * cos,
                    mean[1] + radius * (l21 * cos + l22 * sin),
                ]
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use cytiq_gating::{BooleanOp, CompensationRef, GateRef};

    use super::*;
    use crate::axis::{AxisInput, Display};

    fn dimension(name: &str) -> Dimension {
        Dimension {
            name: name.into(),
            compensation: CompensationRef::Uncompensated,
            transform: None,
            min: None,
            max: None,
        }
    }

    fn linear_axis() -> Axis {
        Axis::new(AxisInput {
            title: "x".into(),
            data: None,
            display: Display::Linear,
            values: &[],
            population: &[],
            range: Some((0.0, 100.0)),
            starts_at_zero: false,
        })
        .unwrap()
    }

    fn arcsinh_axis() -> Axis {
        Axis::new(AxisInput {
            title: "x".into(),
            data: None,
            display: Display::arcsinh(),
            values: &[],
            population: &[],
            range: Some((0.0, 100.0)),
            starts_at_zero: false,
        })
        .unwrap()
    }

    fn gate(kind: GateKind) -> Gate {
        Gate {
            id: "g".into(),
            parent_id: None,
            kind,
        }
    }

    #[test]
    fn ellipse_outline_lies_on_the_mahalanobis_boundary() {
        let covariance = [0.010, 0.002, 0.002, 0.012];
        let points = ellipse([0.75, 0.35], covariance, 3.0).unwrap();
        let determinant = covariance[0] * covariance[3] - covariance[1] * covariance[2];
        let inverse = [
            covariance[3] / determinant,
            -covariance[1] / determinant,
            -covariance[2] / determinant,
            covariance[0] / determinant,
        ];
        for [x, y] in points {
            let (dx, dy) = (x - 0.75, y - 0.35);
            let distance =
                dx * (inverse[0] * dx + inverse[1] * dy) + dy * (inverse[2] * dx + inverse[3] * dy);
            assert!((distance - 3.0).abs() < 1e-9, "{distance}");
        }
        assert!(ellipse([0.0, 0.0], [1.0, 2.0, 2.0, 1.0], 1.0).is_none());
    }

    #[test]
    fn swapped_gate_dimensions_are_transposed() {
        let (fsc, ssc) = (dimension("FSC-A"), dimension("SSC-A"));
        let axes = AxisDimensions {
            x: Some(&ssc),
            y: Some(&fsc),
        };
        let polygon = gate(GateKind::Polygon {
            dimensions: [fsc.clone(), ssc.clone()],
            vertices: vec![[10.0, 20.0], [30.0, 20.0], [30.0, 40.0]],
        });
        let lines = outline(&polygon, &axes, &linear_axis(), &linear_axis()).unwrap();
        assert_eq!(lines[0].points[0], [0.2, 0.1]);
        assert_eq!(lines[0].points[1], [0.2, 0.3]);
        assert!(lines[0].closed);
    }

    #[test]
    fn open_rectangle_bounds_extend_past_the_plot_edge() {
        let mut fsc = dimension("FSC-A");
        fsc.min = Some(50.0);
        let mut ssc = dimension("SSC-A");
        ssc.max = Some(25.0);
        let (x, y) = (dimension("FSC-A"), dimension("SSC-A"));
        let axes = AxisDimensions {
            x: Some(&x),
            y: Some(&y),
        };
        let rectangle = gate(GateKind::Rectangle {
            dimensions: vec![fsc, ssc],
        });
        let lines = outline(&rectangle, &axes, &linear_axis(), &linear_axis()).unwrap();
        assert_eq!(
            lines[0].points,
            [
                [0.5, -BEYOND],
                [1.0 + BEYOND, -BEYOND],
                [1.0 + BEYOND, 0.25],
                [0.5, 0.25]
            ]
        );
    }

    #[test]
    fn range_gate_draws_lines_on_its_axis() {
        let mut time = dimension("Time");
        time.min = Some(10.0);
        time.max = Some(20.0);
        let (x, y) = (dimension("FSC-A"), dimension("Time"));
        let axes = AxisDimensions {
            x: Some(&x),
            y: Some(&y),
        };
        let range = gate(GateKind::Rectangle {
            dimensions: vec![time],
        });
        let lines = outline(&range, &axes, &linear_axis(), &linear_axis()).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].points, [[-BEYOND, 0.1], [1.0 + BEYOND, 0.1]]);
        assert!(!lines[0].closed);
    }

    #[test]
    fn complementary_range_gates_label_their_own_side() {
        // `T` ≥ 30 and `NonT` < 30 draw the same line; each label goes on
        // its own gate's side of it, never at one shared corner.
        let (cd3, ssc) = (dimension("CD3"), dimension("SSC-A"));
        let range = |min: Option<f64>, max: Option<f64>| {
            let mut bound = dimension("CD3");
            (bound.min, bound.max) = (min, max);
            gate(GateKind::Rectangle {
                dimensions: vec![bound],
            })
        };
        let anchor = |gate: &Gate, axes: &AxisDimensions<'_>| {
            let paths = outline(gate, axes, &linear_axis(), &linear_axis()).unwrap();
            label_anchor(gate, axes, &linear_axis(), &linear_axis(), &paths).unwrap()
        };
        let on_x = AxisDimensions {
            x: Some(&cd3),
            y: Some(&ssc),
        };
        let (positive, negative) = (range(Some(30.0), None), range(None, Some(30.0)));
        assert_eq!(anchor(&positive, &on_x), [0.3, 1.0]);
        assert_eq!(anchor(&negative, &on_x), [0.0, 1.0]);
        let on_y = AxisDimensions {
            x: Some(&ssc),
            y: Some(&cd3),
        };
        assert_eq!(anchor(&positive, &on_y), [0.0, 1.0]);
        assert_eq!(anchor(&negative, &on_y), [0.0, 0.3]);
    }

    #[test]
    fn boolean_gate_overlay_is_rejected() {
        let axes = AxisDimensions { x: None, y: None };
        let boolean = gate(GateKind::Boolean {
            op: BooleanOp::Not,
            refs: vec![GateRef {
                id: "a".into(),
                complement: false,
            }],
        });
        assert!(matches!(
            outline(&boolean, &axes, &linear_axis(), &linear_axis()),
            Err(GeometryError::Unsupported(_))
        ));
    }

    #[test]
    fn gate_on_another_dimension_is_a_mismatch() {
        let (x, y) = (dimension("FSC-A"), dimension("SSC-A"));
        let axes = AxisDimensions {
            x: Some(&x),
            y: Some(&y),
        };
        let mut other = dimension("FL1-A");
        other.min = Some(1.0);
        let range = gate(GateKind::Rectangle {
            dimensions: vec![other],
        });
        assert!(matches!(
            outline(&range, &axes, &linear_axis(), &linear_axis()),
            Err(GeometryError::Mismatch(_))
        ));
    }

    #[test]
    fn edges_are_densified_on_non_linear_axes() {
        let (x, y) = (dimension("FL1-A"), dimension("FL2-A"));
        let axes = AxisDimensions {
            x: Some(&x),
            y: Some(&y),
        };
        let triangle = gate(GateKind::Polygon {
            dimensions: [x.clone(), y.clone()],
            vertices: vec![[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]],
        });
        let linear = outline(&triangle, &axes, &linear_axis(), &linear_axis()).unwrap();
        assert_eq!(linear[0].points.len(), 3);
        let curved = outline(&triangle, &axes, &arcsinh_axis(), &arcsinh_axis()).unwrap();
        assert_eq!(curved[0].points.len(), 3 * EDGE_STEPS);
    }
}
