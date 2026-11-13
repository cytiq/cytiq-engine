//! `GatingML` 2.0 XML parser.
//!
//! Specification: <http://flowcyt.sourceforge.net/gating/latest.pdf>

use std::collections::{HashMap, HashSet, VecDeque};

use cytiq_transform::scales::{Hyperlog, Logicle};
use roxmltree::{Document, Node};

use crate::error::GatingError;
use crate::model::{
    BooleanOp, CompensationRef, Dimension, Gate, GateKind, GateRef, GatingStrategy, Quadrant,
    QuadrantDivider, QuadrantGate, SpectrumMatrix,
};
use crate::transforms::Transform;

const GATING_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/gating";
const TRANSFORMS_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/transformations";
const DATA_TYPE_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/datatypes";

/// Largest Gating-ML document read: far above real strategies (the bundled
/// fixtures are under 8 KiB), and small enough to bound the parse tree.
pub const GATINGML_BYTES_MAX: u64 = 16 << 20;

/// Read a Gating-ML document, refusing more than [`GATINGML_BYTES_MAX`] bytes
/// (also when the file grows after opening) before anything is parsed.
///
/// # Errors
/// Returns the I/O error, `InvalidData` over the limit or for invalid UTF-8.
pub fn read_gatingml(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(GATINGML_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > GATINGML_BYTES_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Gating-ML document exceeds {GATINGML_BYTES_MAX} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Parse a `GatingML` 2.0 document.
///
/// # Errors
///
/// Returns an error when the XML is malformed, uses unsupported `GatingML`
/// features (ratio transforms, ratio or new dimensions), or has dangling or
/// cyclic references.
pub fn parse_gatingml(xml: &str) -> Result<GatingStrategy, GatingError> {
    let document = Document::parse(xml)?;
    let root = document.root_element();
    if !is(root, GATING_NS, "Gating-ML") {
        return Err(GatingError::InvalidGatingMl(format!(
            "root element must be `gating:Gating-ML`, found `{}`",
            root.tag_name().name()
        )));
    }

    let mut transforms = HashMap::new();
    let mut matrices = HashMap::new();
    let mut gates = Vec::new();
    for node in root.children().filter(Node::is_element) {
        match (node.tag_name().namespace(), node.tag_name().name()) {
            (Some(TRANSFORMS_NS), "transformation") => {
                let id = required_attr(node, TRANSFORMS_NS, "id")?;
                let transform = parse_transformation(node, &id)?;
                if transforms.insert(id.clone(), transform).is_some() {
                    return Err(GatingError::InvalidGatingMl(format!(
                        "duplicate transformation id `{id}`"
                    )));
                }
            }
            (Some(TRANSFORMS_NS), "spectrumMatrix") => {
                let id = required_attr(node, TRANSFORMS_NS, "id")?;
                if id == "uncompensated" || id.eq_ignore_ascii_case("fcs") {
                    return Err(GatingError::InvalidGatingMl(format!(
                        "spectrumMatrix id `{id}` is reserved"
                    )));
                }
                let matrix = parse_spectrum_matrix(node, &id)?;
                if matrices.insert(id.clone(), matrix).is_some() {
                    return Err(GatingError::InvalidGatingMl(format!(
                        "duplicate spectrumMatrix id `{id}`"
                    )));
                }
            }
            (Some(GATING_NS), "QuadrantGate") => gates.extend(parse_quadrant(node)?.gates()?),
            (Some(GATING_NS), name) if name.ends_with("Gate") => gates.push(parse_gate(node)?),
            _ => {}
        }
    }

    build_strategy(gates, transforms, matrices)
}

pub(crate) fn build_strategy(
    gates: Vec<Gate>,
    transforms: HashMap<String, Transform>,
    matrices: HashMap<String, SpectrumMatrix>,
) -> Result<GatingStrategy, GatingError> {
    let mut document_index = HashMap::new();
    for (index, gate) in gates.iter().enumerate() {
        if document_index.insert(gate.id.clone(), index).is_some() {
            return Err(GatingError::DuplicateGate {
                id: gate.id.clone(),
            });
        }
    }
    // A QuadrantGate id shares the gate id namespace but names no population.
    let mut quadrant_gates = HashMap::new();
    for gate in &gates {
        if let GateKind::Quadrant { definition, .. } = &gate.kind {
            let known = quadrant_gates
                .entry(definition.id.as_str())
                .or_insert(definition);
            if document_index.contains_key(&definition.id) || *known != definition {
                return Err(GatingError::DuplicateGate {
                    id: definition.id.clone(),
                });
            }
        }
    }

    let mut dependents = vec![Vec::new(); gates.len()];
    let mut pending = vec![0_usize; gates.len()];
    for (index, gate) in gates.iter().enumerate() {
        validate_dimension_refs(gate, &transforms, &matrices)?;
        let mut dependencies = gate.parent_id.iter().collect::<Vec<_>>();
        if let GateKind::Boolean { refs, .. } = &gate.kind {
            dependencies.extend(refs.iter().map(|reference| &reference.id));
        }
        let unique = dependencies.into_iter().collect::<HashSet<_>>();
        for dependency in unique {
            if quadrant_gates.contains_key(dependency.as_str()) {
                return Err(invalid_gate(
                    &gate.id,
                    &format!(
                        "`{dependency}` is a QuadrantGate; reference one of its quadrants instead"
                    ),
                ));
            }
            let Some(&dependency_index) = document_index.get(dependency) else {
                return Err(GatingError::UnknownGateRef {
                    gate: gate.id.clone(),
                    reference: dependency.clone(),
                });
            };
            dependents[dependency_index].push(index);
            pending[index] += 1;
        }
    }

    // Kahn's algorithm; ties keep document order.
    let mut ready = (0..gates.len())
        .filter(|index| pending[*index] == 0)
        .collect::<VecDeque<_>>();
    let mut order = Vec::with_capacity(gates.len());
    while let Some(index) = ready.pop_front() {
        order.push(index);
        for &dependent in &dependents[index] {
            pending[dependent] -= 1;
            if pending[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if let Some(stuck) = (0..gates.len()).find(|index| pending[*index] > 0) {
        return Err(GatingError::CyclicHierarchy {
            gate: gates[stuck].id.clone(),
        });
    }

    let mut slots = gates.into_iter().map(Some).collect::<Vec<_>>();
    let gates = order
        .into_iter()
        .map(|index| slots[index].take().expect("each gate is ordered once"))
        .collect::<Vec<_>>();
    let index = gates
        .iter()
        .enumerate()
        .map(|(position, gate)| (gate.id.clone(), position))
        .collect();

    Ok(GatingStrategy {
        gates,
        index,
        transforms,
        matrices,
    })
}

fn validate_dimension_refs(
    gate: &Gate,
    transforms: &HashMap<String, Transform>,
    matrices: &HashMap<String, SpectrumMatrix>,
) -> Result<(), GatingError> {
    for dimension in gate.dimensions() {
        if let Some(reference) = &dimension.transform
            && !transforms.contains_key(reference)
        {
            return Err(GatingError::UnknownTransform {
                gate: gate.id.clone(),
                reference: reference.clone(),
            });
        }
        if let CompensationRef::Matrix(reference) = &dimension.compensation
            && !matrices.contains_key(reference)
        {
            return Err(GatingError::UnknownCompensation {
                gate: gate.id.clone(),
                reference: reference.clone(),
            });
        }
    }
    Ok(())
}

fn parse_gate(node: Node<'_, '_>) -> Result<Gate, GatingError> {
    let id = required_attr(node, GATING_NS, "id")?;
    let parent_id = attr(node, GATING_NS, "parent_id");
    let kind = match node.tag_name().name() {
        "RectangleGate" => {
            let dimensions = parse_dimensions(node)?;
            if dimensions.is_empty() {
                return Err(invalid_gate(
                    &id,
                    "rectangle gate needs at least one dimension",
                ));
            }
            if dimensions
                .iter()
                .any(|dimension| dimension.min.is_none() && dimension.max.is_none())
            {
                return Err(invalid_gate(
                    &id,
                    "every rectangle dimension needs a min or a max",
                ));
            }
            GateKind::Rectangle { dimensions }
        }
        "PolygonGate" => {
            let dimensions: [Dimension; 2] = parse_dimensions(node)?
                .try_into()
                .map_err(|_| invalid_gate(&id, "polygon gate needs exactly two dimensions"))?;
            let vertices = children(node, GATING_NS, "vertex")
                .map(|vertex| {
                    let coordinates = coordinate_values(vertex, "coordinate")?;
                    <[f64; 2]>::try_from(coordinates)
                        .map_err(|_| invalid_gate(&id, "vertex must have two coordinates"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if vertices.len() < 3 {
                return Err(invalid_gate(
                    &id,
                    "polygon gate needs at least three vertices",
                ));
            }
            GateKind::Polygon {
                dimensions,
                vertices,
            }
        }
        "EllipsoidGate" => parse_ellipsoid(node, &id)?,
        "BooleanGate" => parse_boolean(node, &id)?,
        other => {
            return Err(GatingError::UnsupportedElement {
                element: format!("gating:{other}"),
            });
        }
    };
    Ok(Gate {
        id,
        parent_id,
        kind,
    })
}

/// A `gating:QuadrantGate`: its dividers (a dimension plus `gating:value`
/// children) and quadrants (`gating:position` with `divider_ref` and
/// `location`).
fn parse_quadrant(node: Node<'_, '_>) -> Result<QuadrantGate, GatingError> {
    let id = required_attr(node, GATING_NS, "id")?;
    let dividers = children(node, GATING_NS, "divider")
        .map(|divider| {
            let values = children(divider, GATING_NS, "value")
                .map(|value| parse_number(value, "value", value.text().unwrap_or_default()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(QuadrantDivider {
                id: required_attr(divider, GATING_NS, "id")?,
                dimension: parse_dimension(divider)?,
                values,
            })
        })
        .collect::<Result<Vec<_>, GatingError>>()?;
    let quadrants = children(node, GATING_NS, "Quadrant")
        .map(|quadrant| {
            let positions = children(quadrant, GATING_NS, "position")
                .map(|position| {
                    Ok((
                        required_attr(position, GATING_NS, "divider_ref")?,
                        number_attr(position, GATING_NS, "location")?,
                    ))
                })
                .collect::<Result<Vec<_>, GatingError>>()?;
            Ok(Quadrant {
                id: required_attr(quadrant, GATING_NS, "id")?,
                positions,
            })
        })
        .collect::<Result<Vec<_>, GatingError>>()?;
    Ok(QuadrantGate {
        parent_id: attr(node, GATING_NS, "parent_id"),
        id,
        dividers,
        quadrants,
    })
}

fn parse_ellipsoid(node: Node<'_, '_>, id: &str) -> Result<GateKind, GatingError> {
    let dimensions = parse_dimensions(node)?;
    let size = dimensions.len();
    if size < 2 {
        return Err(invalid_gate(
            id,
            "ellipsoid gate needs at least two dimensions",
        ));
    }
    let mean = child(node, GATING_NS, "mean")
        .ok_or_else(|| invalid_gate(id, "missing `gating:mean`"))
        .and_then(|mean| coordinate_values(mean, "coordinate"))?;
    if mean.len() != size {
        return Err(invalid_gate(
            id,
            "mean must have one coordinate per dimension",
        ));
    }
    let matrix = child(node, GATING_NS, "covarianceMatrix")
        .ok_or_else(|| invalid_gate(id, "missing `gating:covarianceMatrix`"))?;
    let mut covariance = Vec::with_capacity(size * size);
    let mut rows = 0;
    for row in children(matrix, GATING_NS, "row") {
        let entries = coordinate_values(row, "entry")?;
        if entries.len() != size {
            return Err(invalid_gate(
                id,
                "covariance rows must have one entry per dimension",
            ));
        }
        covariance.extend(entries);
        rows += 1;
    }
    if rows != size {
        return Err(invalid_gate(
            id,
            "covariance matrix must be square over the dimensions",
        ));
    }
    let distance_square = child(node, GATING_NS, "distanceSquare")
        .ok_or_else(|| invalid_gate(id, "missing `gating:distanceSquare`"))
        .and_then(|element| number_attr(element, DATA_TYPE_NS, "value"))?;
    Ok(GateKind::Ellipsoid {
        dimensions,
        mean,
        covariance,
        distance_square,
    })
}

fn parse_boolean(node: Node<'_, '_>, id: &str) -> Result<GateKind, GatingError> {
    let mut operations = node.children().filter(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(GATING_NS)
            && matches!(child.tag_name().name(), "and" | "or" | "not")
    });
    let operation = operations
        .next()
        .ok_or_else(|| invalid_gate(id, "boolean gate needs an `and`, `or` or `not` element"))?;
    if operations.next().is_some() {
        return Err(invalid_gate(
            id,
            "boolean gate must have exactly one operation",
        ));
    }
    let op = match operation.tag_name().name() {
        "and" => BooleanOp::And,
        "or" => BooleanOp::Or,
        _ => BooleanOp::Not,
    };
    let refs = children(operation, GATING_NS, "gateReference")
        .map(|reference| {
            Ok(GateRef {
                id: required_attr(reference, GATING_NS, "ref")?,
                complement: attr(reference, GATING_NS, "use-as-complement")
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("true")),
            })
        })
        .collect::<Result<Vec<_>, GatingError>>()?;
    match op {
        BooleanOp::Not if refs.len() != 1 => {
            return Err(invalid_gate(id, "`not` takes exactly one gate reference"));
        }
        BooleanOp::And | BooleanOp::Or if refs.len() < 2 => {
            return Err(invalid_gate(
                id,
                "`and`/`or` take at least two gate references",
            ));
        }
        _ => {}
    }
    Ok(GateKind::Boolean { op, refs })
}

fn parse_dimensions(node: Node<'_, '_>) -> Result<Vec<Dimension>, GatingError> {
    children(node, GATING_NS, "dimension")
        .map(parse_dimension)
        .collect()
}

/// A `gating:dimension` or `gating:divider`: one FCS channel, its
/// compensation and transformation, and optional rectangle bounds.
fn parse_dimension(dimension: Node<'_, '_>) -> Result<Dimension, GatingError> {
    for unsupported in ["new-dimension", "ratio-dimension"] {
        if child(dimension, DATA_TYPE_NS, unsupported).is_some() {
            return Err(GatingError::UnsupportedElement {
                element: format!("data-type:{unsupported}"),
            });
        }
    }
    let fcs_dimension = child(dimension, DATA_TYPE_NS, "fcs-dimension").ok_or_else(|| {
        GatingError::InvalidGatingMl("gate dimension needs a `data-type:fcs-dimension`".to_owned())
    })?;
    let compensation = match attr(dimension, GATING_NS, "compensation-ref").as_deref() {
        None | Some("uncompensated") => CompensationRef::Uncompensated,
        Some("FCS") => CompensationRef::Fcs,
        Some(reference) => CompensationRef::Matrix(reference.to_owned()),
    };
    Ok(Dimension {
        name: required_attr(fcs_dimension, DATA_TYPE_NS, "name")?,
        compensation,
        transform: attr(dimension, GATING_NS, "transformation-ref"),
        min: optional_number_attr(dimension, GATING_NS, "min")?,
        max: optional_number_attr(dimension, GATING_NS, "max")?,
    })
}

fn parse_transformation(node: Node<'_, '_>, id: &str) -> Result<Transform, GatingError> {
    let mut elements = node.children().filter(Node::is_element);
    let element = elements
        .next()
        .ok_or_else(|| GatingError::InvalidTransform {
            id: id.to_owned(),
            reason: "transformation has no function element".to_owned(),
        })?;
    if elements.next().is_some() {
        return Err(invalid_transform(
            id,
            "transformation must have exactly one function",
        ));
    }
    if element.tag_name().namespace() != Some(TRANSFORMS_NS) {
        return Err(invalid_transform(
            id,
            "transformation function must be in the transforms namespace",
        ));
    }
    let param = |name: &str| number_attr(element, TRANSFORMS_NS, name);
    let transform = match element.tag_name().name() {
        "flin" => parse_linear_transform(element, id)?,
        "flog" => parse_log_transform(element, id)?,
        "fasinh" => parse_asinh_transform(element, id)?,
        "logicle" => Transform::Logicle(
            Logicle::new(param("T")?, param("W")?, param("M")?, param("A")?)
                .map_err(|error| invalid_transform(id, &error.to_string()))?,
        ),
        "hyperlog" => Transform::Hyperlog(
            Hyperlog::new(param("T")?, param("W")?, param("M")?, param("A")?)
                .map_err(|error| invalid_transform(id, &error.to_string()))?,
        ),
        other => {
            return Err(GatingError::UnsupportedElement {
                element: format!("transforms:{other}"),
            });
        }
    };
    Ok(transform)
}

fn parse_linear_transform(element: Node<'_, '_>, id: &str) -> Result<Transform, GatingError> {
    let t = number_attr(element, TRANSFORMS_NS, "T")?;
    let a = number_attr(element, TRANSFORMS_NS, "A")?;
    if t + a == 0.0 {
        return Err(invalid_transform(id, "flin requires T + A != 0"));
    }
    Ok(Transform::Linear { t, a })
}

fn parse_log_transform(element: Node<'_, '_>, id: &str) -> Result<Transform, GatingError> {
    let t = number_attr(element, TRANSFORMS_NS, "T")?;
    let m = number_attr(element, TRANSFORMS_NS, "M")?;
    if t <= 0.0 || m <= 0.0 {
        return Err(invalid_transform(id, "flog requires T > 0 and M > 0"));
    }
    Ok(Transform::Log { t, m })
}

fn parse_asinh_transform(element: Node<'_, '_>, id: &str) -> Result<Transform, GatingError> {
    let t = number_attr(element, TRANSFORMS_NS, "T")?;
    let m = number_attr(element, TRANSFORMS_NS, "M")?;
    let a = number_attr(element, TRANSFORMS_NS, "A")?;
    if t <= 0.0 || m <= 0.0 || m + a == 0.0 {
        return Err(invalid_transform(
            id,
            "fasinh requires T > 0, M > 0 and M + A != 0",
        ));
    }
    Ok(Transform::Asinh { t, m, a })
}

fn parse_spectrum_matrix(node: Node<'_, '_>, id: &str) -> Result<SpectrumMatrix, GatingError> {
    let names = |list: &str| -> Result<Vec<String>, GatingError> {
        let element = child(node, TRANSFORMS_NS, list).ok_or_else(|| {
            GatingError::InvalidGatingMl(format!("spectrumMatrix `{id}` is missing `{list}`"))
        })?;
        children(element, DATA_TYPE_NS, "fcs-dimension")
            .map(|dimension| required_attr(dimension, DATA_TYPE_NS, "name"))
            .collect()
    };
    let fluorochromes = names("fluorochromes")?;
    let detectors = names("detectors")?;
    let mut values = Vec::with_capacity(fluorochromes.len() * detectors.len());
    let mut rows = 0;
    for spectrum in children(node, TRANSFORMS_NS, "spectrum") {
        let coefficients = children(spectrum, TRANSFORMS_NS, "coefficient")
            .map(|coefficient| number_attr(coefficient, TRANSFORMS_NS, "value"))
            .collect::<Result<Vec<_>, _>>()?;
        if coefficients.len() != detectors.len() {
            return Err(GatingError::InvalidGatingMl(format!(
                "spectrumMatrix `{id}` rows must have one coefficient per detector"
            )));
        }
        values.extend(coefficients);
        rows += 1;
    }
    if rows != fluorochromes.len() {
        return Err(GatingError::InvalidGatingMl(format!(
            "spectrumMatrix `{id}` must have one spectrum per fluorochrome"
        )));
    }
    if fluorochromes.len() != detectors.len() {
        return Err(GatingError::UnsupportedElement {
            element: format!("non-square transforms:spectrumMatrix `{id}`"),
        });
    }
    Ok(SpectrumMatrix {
        fluorochromes,
        detectors,
        values,
    })
}

fn coordinate_values(node: Node<'_, '_>, name: &str) -> Result<Vec<f64>, GatingError> {
    children(node, GATING_NS, name)
        .map(|element| number_attr(element, DATA_TYPE_NS, "value"))
        .collect()
}

fn is(node: Node<'_, '_>, namespace: &str, name: &str) -> bool {
    node.tag_name().namespace() == Some(namespace) && node.tag_name().name() == name
}

fn children<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    namespace: &'a str,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |child| is(*child, namespace, name))
}

fn child<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    namespace: &'a str,
    name: &'a str,
) -> Option<Node<'a, 'input>> {
    children(node, namespace, name).next()
}

/// Attribute lookup that accepts the namespaced form and, as `FlowKit` does,
/// falls back to an un-prefixed attribute.
fn attr(node: Node<'_, '_>, namespace: &str, name: &str) -> Option<String> {
    node.attribute((namespace, name))
        .or_else(|| node.attribute(name))
        .map(str::to_owned)
}

fn required_attr(node: Node<'_, '_>, namespace: &str, name: &str) -> Result<String, GatingError> {
    attr(node, namespace, name).ok_or_else(|| {
        GatingError::InvalidGatingMl(format!(
            "`{}` element is missing attribute `{name}`",
            node.tag_name().name()
        ))
    })
}

fn optional_number_attr(
    node: Node<'_, '_>,
    namespace: &str,
    name: &str,
) -> Result<Option<f64>, GatingError> {
    attr(node, namespace, name)
        .map(|value| parse_number(node, name, &value))
        .transpose()
}

fn number_attr(node: Node<'_, '_>, namespace: &str, name: &str) -> Result<f64, GatingError> {
    let value = required_attr(node, namespace, name)?;
    parse_number(node, name, &value)
}

fn parse_number(node: Node<'_, '_>, name: &str, value: &str) -> Result<f64, GatingError> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .ok_or_else(|| {
            GatingError::InvalidGatingMl(format!(
                "`{}` attribute `{name}` has invalid number `{value}`",
                node.tag_name().name()
            ))
        })
}

fn invalid_gate(id: &str, reason: &str) -> GatingError {
    GatingError::InvalidGate {
        id: id.to_owned(),
        reason: reason.to_owned(),
    }
}

fn invalid_transform(id: &str, reason: &str) -> GatingError {
    GatingError::InvalidTransform {
        id: id.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_gatingml_over_the_budget_before_parsing() {
        let directory = std::env::temp_dir().join(format!(
            "cytiq-gating-oversized-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("huge.xml");
        // Sparse: the length is declared, nothing is written.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(GATINGML_BYTES_MAX + 1)
            .unwrap();
        let error = read_gatingml(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeds"), "{error}");

        std::fs::write(&path, document("")).unwrap();
        assert!(read_gatingml(&path).unwrap().contains("Gating-ML"));
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(
            read_gatingml(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(directory).ok();
    }

    fn document(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<gating:Gating-ML
    xmlns:gating="{GATING_NS}"
    xmlns:transforms="{TRANSFORMS_NS}"
    xmlns:data-type="{DATA_TYPE_NS}">
{body}
</gating:Gating-ML>"#
        )
    }

    fn range(id: &str, parent: Option<&str>) -> String {
        let parent = parent.map_or(String::new(), |parent| {
            format!(r#" gating:parent_id="{parent}""#)
        });
        format!(
            r#"<gating:RectangleGate gating:id="{id}"{parent}>
  <gating:dimension gating:min="1" gating:compensation-ref="uncompensated">
    <data-type:fcs-dimension data-type:name="FSC-A"/>
  </gating:dimension>
</gating:RectangleGate>"#
        )
    }

    #[test]
    fn orders_children_after_parents() {
        let xml = document(&format!(
            "{}{}{}",
            range("child", Some("parent")),
            range("grandchild", Some("child")),
            range("parent", None)
        ));
        let strategy = parse_gatingml(&xml).unwrap();
        let ids = strategy
            .gates()
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["parent", "child", "grandchild"]);
        assert_eq!(
            strategy.gate_path("grandchild"),
            ["parent", "child", "grandchild"]
        );
    }

    #[test]
    fn fails_when_hierarchy_is_cyclic() {
        let xml = document(&format!(
            "{}{}",
            range("a", Some("b")),
            range("b", Some("a"))
        ));
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::CyclicHierarchy { .. })
        ));
    }

    #[test]
    fn fails_when_parent_is_unknown() {
        let xml = document(&range("a", Some("missing")));
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::UnknownGateRef { .. })
        ));
    }

    #[test]
    fn fails_when_gate_id_is_duplicated() {
        let xml = document(&format!("{}{}", range("a", None), range("a", None)));
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::DuplicateGate { .. })
        ));
    }

    #[test]
    fn parses_all_supported_shapes_and_transforms() {
        let xml = document(
            r#"
<transforms:transformation transforms:id="lin">
  <transforms:flin transforms:T="1000" transforms:A="0"/>
</transforms:transformation>
<transforms:transformation transforms:id="logicle">
  <transforms:logicle transforms:T="262144" transforms:W="0.5" transforms:M="4.5" transforms:A="0"/>
</transforms:transformation>
<transforms:spectrumMatrix transforms:id="comp">
  <transforms:fluorochromes>
    <data-type:fcs-dimension data-type:name="FITC"/>
    <data-type:fcs-dimension data-type:name="PE"/>
  </transforms:fluorochromes>
  <transforms:detectors>
    <data-type:fcs-dimension data-type:name="FL1-A"/>
    <data-type:fcs-dimension data-type:name="FL2-A"/>
  </transforms:detectors>
  <transforms:spectrum>
    <transforms:coefficient transforms:value="1"/>
    <transforms:coefficient transforms:value="0.1"/>
  </transforms:spectrum>
  <transforms:spectrum>
    <transforms:coefficient transforms:value="0.2"/>
    <transforms:coefficient transforms:value="1"/>
  </transforms:spectrum>
</transforms:spectrumMatrix>
<gating:PolygonGate gating:id="poly">
  <gating:dimension gating:compensation-ref="comp" gating:transformation-ref="logicle">
    <data-type:fcs-dimension data-type:name="FITC"/>
  </gating:dimension>
  <gating:dimension gating:compensation-ref="FCS" gating:transformation-ref="lin">
    <data-type:fcs-dimension data-type:name="FL2-A"/>
  </gating:dimension>
  <gating:vertex><gating:coordinate data-type:value="0"/><gating:coordinate data-type:value="0"/></gating:vertex>
  <gating:vertex><gating:coordinate data-type:value="1"/><gating:coordinate data-type:value="0"/></gating:vertex>
  <gating:vertex><gating:coordinate data-type:value="1"/><gating:coordinate data-type:value="1"/></gating:vertex>
</gating:PolygonGate>
<gating:EllipsoidGate gating:id="ellipse" gating:parent_id="poly">
  <gating:dimension gating:compensation-ref="uncompensated">
    <data-type:fcs-dimension data-type:name="FSC-A"/>
  </gating:dimension>
  <gating:dimension gating:compensation-ref="uncompensated">
    <data-type:fcs-dimension data-type:name="SSC-A"/>
  </gating:dimension>
  <gating:mean><gating:coordinate data-type:value="10"/><gating:coordinate data-type:value="20"/></gating:mean>
  <gating:covarianceMatrix>
    <gating:row><gating:entry data-type:value="4"/><gating:entry data-type:value="1"/></gating:row>
    <gating:row><gating:entry data-type:value="1"/><gating:entry data-type:value="9"/></gating:row>
  </gating:covarianceMatrix>
  <gating:distanceSquare data-type:value="2.5"/>
</gating:EllipsoidGate>
<gating:BooleanGate gating:id="not-ellipse">
  <gating:not><gating:gateReference gating:ref="ellipse"/></gating:not>
</gating:BooleanGate>
<gating:BooleanGate gating:id="and">
  <gating:and>
    <gating:gateReference gating:ref="poly"/>
    <gating:gateReference gating:ref="ellipse" gating:use-as-complement="true"/>
  </gating:and>
</gating:BooleanGate>"#,
        );
        let strategy = parse_gatingml(&xml).unwrap();
        assert_eq!(strategy.gates().len(), 4);
        let GateKind::Polygon {
            dimensions,
            vertices,
        } = &strategy.gate("poly").unwrap().kind
        else {
            panic!("expected polygon");
        };
        assert_eq!(
            dimensions[0].compensation,
            CompensationRef::Matrix("comp".to_owned())
        );
        assert_eq!(dimensions[1].compensation, CompensationRef::Fcs);
        assert_eq!(vertices.len(), 3);
        let GateKind::Ellipsoid {
            covariance,
            distance_square,
            ..
        } = &strategy.gate("ellipse").unwrap().kind
        else {
            panic!("expected ellipsoid");
        };
        assert_eq!(covariance, &[4.0, 1.0, 1.0, 9.0]);
        assert!((distance_square - 2.5).abs() < f64::EPSILON);
        let GateKind::Boolean { op, refs } = &strategy.gate("and").unwrap().kind else {
            panic!("expected boolean");
        };
        assert_eq!(*op, BooleanOp::And);
        assert!(refs[1].complement);
        assert_eq!(
            strategy.matrix("comp").unwrap().values,
            [1.0, 0.1, 0.2, 1.0]
        );
        assert!(matches!(
            strategy.transform("lin"),
            Some(Transform::Linear { .. })
        ));
    }

    #[test]
    fn rejects_ratio_and_new_dimensions_as_unsupported() {
        for dimension in [
            r#"<data-type:ratio-dimension data-type:ratio-ref="ratio"/>"#,
            r#"<data-type:new-dimension data-type:transformation-ref="t"/>"#,
        ] {
            let xml = document(&format!(
                r#"<gating:RectangleGate gating:id="r">
  <gating:dimension gating:min="0">{dimension}</gating:dimension>
</gating:RectangleGate>"#
            ));
            assert!(
                matches!(
                    parse_gatingml(&xml),
                    Err(GatingError::UnsupportedElement { .. })
                ),
                "{dimension}"
            );
        }
    }

    #[test]
    fn an_empty_quadrant_gate_is_invalid() {
        let xml = document(r#"<gating:QuadrantGate gating:id="q"/>"#);
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::InvalidGate { id, .. }) if id == "q"
        ));
    }

    #[test]
    fn rejects_unknown_transformation_reference() {
        let xml = document(
            r#"<gating:RectangleGate gating:id="r">
  <gating:dimension gating:min="0" gating:transformation-ref="missing">
    <data-type:fcs-dimension data-type:name="FSC-A"/>
  </gating:dimension>
</gating:RectangleGate>"#,
        );
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::UnknownTransform { .. })
        ));
    }

    #[test]
    fn rejects_rectangle_dimension_without_bounds() {
        let xml = document(
            r#"<gating:RectangleGate gating:id="r">
  <gating:dimension><data-type:fcs-dimension data-type:name="FSC-A"/></gating:dimension>
</gating:RectangleGate>"#,
        );
        assert!(matches!(
            parse_gatingml(&xml),
            Err(GatingError::InvalidGate { .. })
        ));
    }
}
