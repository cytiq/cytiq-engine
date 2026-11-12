//! `GatingML` 2.0 XML writer, the inverse of [`crate::parse_gatingml`].

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::error::GatingError;
use crate::model::{
    BooleanOp, CompensationRef, Dimension, Gate, GateKind, GatingStrategy, QuadrantGate,
};
use crate::transforms::Transform;

const GATING_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/gating";
const TRANSFORMS_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/transformations";
const DATA_TYPE_NS: &str = "http://www.isac-net.org/std/Gating-ML/v2.0/datatypes";

/// Serialize a strategy as a `GatingML` 2.0 document.
///
/// Transformations and matrices are written sorted by id and gates in
/// evaluation order, so the output is deterministic. Numbers use Rust's
/// shortest round-trip formatting, so parsing the document back yields the
/// same strategy.
///
/// # Errors
///
/// Every element the parser accepts can be written; the `Result` is kept so
/// that a future element that cannot be written fails explicitly.
pub fn to_gatingml(strategy: &GatingStrategy) -> Result<String, GatingError> {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gating:Gating-ML\n    \
         xmlns:gating=\"{GATING_NS}\"\n    xmlns:transforms=\"{TRANSFORMS_NS}\"\n    \
         xmlns:data-type=\"{DATA_TYPE_NS}\">\n"
    );

    write_transforms(&mut xml, strategy);
    write_matrices(&mut xml, strategy);
    // A QuadrantGate is written once, where its first quadrant is evaluated.
    let mut quadrant_gates = HashSet::new();
    for gate in strategy.gates() {
        match &gate.kind {
            GateKind::Quadrant { definition, .. } => {
                if quadrant_gates.insert(definition.id.as_str()) {
                    write_quadrant_gate(&mut xml, definition);
                }
            }
            _ => write_gate(&mut xml, gate),
        }
    }
    xml.push_str("</gating:Gating-ML>\n");
    Ok(xml)
}

/// `compensation-ref` and `transformation-ref` of a dimension or divider.
fn reference_attributes(dimension: &Dimension) -> String {
    let compensation = match &dimension.compensation {
        CompensationRef::Uncompensated => "uncompensated".to_owned(),
        CompensationRef::Fcs => "FCS".to_owned(),
        CompensationRef::Matrix(id) => escape(id),
    };
    let mut attributes = format!(" gating:compensation-ref=\"{compensation}\"");
    if let Some(transform) = &dimension.transform {
        let _ = write!(
            attributes,
            " gating:transformation-ref=\"{}\"",
            escape(transform)
        );
    }
    attributes
}

fn write_dimension(xml: &mut String, dimension: &Dimension) {
    let mut attributes = reference_attributes(dimension);
    if let Some(min) = dimension.min {
        let _ = write!(attributes, " gating:min=\"{min}\"");
    }
    if let Some(max) = dimension.max {
        let _ = write!(attributes, " gating:max=\"{max}\"");
    }
    let _ = writeln!(
        xml,
        "    <gating:dimension{attributes}>\n      \
         <data-type:fcs-dimension data-type:name=\"{}\"/>\n    </gating:dimension>",
        escape(&dimension.name)
    );
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn write_transforms(xml: &mut String, strategy: &GatingStrategy) {
    let mut transform_ids = strategy.transforms().keys().collect::<Vec<_>>();
    transform_ids.sort();
    for id in transform_ids {
        let function = match &strategy.transforms()[id] {
            Transform::Linear { t, a } => {
                format!(r#"<transforms:flin transforms:T="{t}" transforms:A="{a}"/>"#)
            }
            Transform::Log { t, m } => {
                format!(r#"<transforms:flog transforms:T="{t}" transforms:M="{m}"/>"#)
            }
            Transform::Asinh { t, m, a } => format!(
                r#"<transforms:fasinh transforms:T="{t}" transforms:M="{m}" transforms:A="{a}"/>"#
            ),
            Transform::Logicle(logicle) => {
                let [t, w, m, a] = logicle.parameters();
                format!(
                    r#"<transforms:logicle transforms:T="{t}" transforms:W="{w}" transforms:M="{m}" transforms:A="{a}"/>"#
                )
            }
            Transform::Hyperlog(hyperlog) => {
                let [t, w, m, a] = hyperlog.parameters();
                format!(
                    r#"<transforms:hyperlog transforms:T="{t}" transforms:W="{w}" transforms:M="{m}" transforms:A="{a}"/>"#
                )
            }
        };
        let _ = write!(
            xml,
            "  <transforms:transformation transforms:id=\"{}\">\n    {function}\n  \
             </transforms:transformation>\n",
            escape(id)
        );
    }
}

fn write_matrices(xml: &mut String, strategy: &GatingStrategy) {
    let mut matrix_ids = strategy.matrices().keys().collect::<Vec<_>>();
    matrix_ids.sort();
    for id in matrix_ids {
        let matrix = &strategy.matrices()[id];
        let names = |names: &[String]| {
            names.iter().fold(String::new(), |mut list, name| {
                let _ = writeln!(
                    list,
                    "      <data-type:fcs-dimension data-type:name=\"{}\"/>",
                    escape(name)
                );
                list
            })
        };
        let _ = write!(
            xml,
            "  <transforms:spectrumMatrix transforms:id=\"{}\">\n    \
             <transforms:fluorochromes>\n{}    </transforms:fluorochromes>\n    \
             <transforms:detectors>\n{}    </transforms:detectors>\n",
            escape(id),
            names(&matrix.fluorochromes),
            names(&matrix.detectors)
        );
        for row in matrix.values.chunks(matrix.detectors.len().max(1)) {
            xml.push_str("    <transforms:spectrum>\n");
            for value in row {
                let _ = writeln!(
                    xml,
                    "      <transforms:coefficient transforms:value=\"{value}\"/>"
                );
            }
            xml.push_str("    </transforms:spectrum>\n");
        }
        xml.push_str("  </transforms:spectrumMatrix>\n");
    }
}

fn write_quadrant_gate(xml: &mut String, definition: &QuadrantGate) {
    let parent = definition
        .parent_id
        .as_ref()
        .map_or(String::new(), |parent| {
            format!(" gating:parent_id=\"{}\"", escape(parent))
        });
    let _ = writeln!(
        xml,
        "  <gating:QuadrantGate gating:id=\"{}\"{parent}>",
        escape(&definition.id)
    );
    for divider in &definition.dividers {
        let _ = writeln!(
            xml,
            "    <gating:divider gating:id=\"{}\"{}>\n      \
             <data-type:fcs-dimension data-type:name=\"{}\"/>",
            escape(&divider.id),
            reference_attributes(&divider.dimension),
            escape(&divider.dimension.name)
        );
        for value in &divider.values {
            let _ = writeln!(xml, "      <gating:value>{value}</gating:value>");
        }
        xml.push_str("    </gating:divider>\n");
    }
    for quadrant in &definition.quadrants {
        let _ = writeln!(
            xml,
            "    <gating:Quadrant gating:id=\"{}\">",
            escape(&quadrant.id)
        );
        for (divider, location) in &quadrant.positions {
            let _ = writeln!(
                xml,
                "      <gating:position gating:divider_ref=\"{}\" gating:location=\"{location}\"/>",
                escape(divider)
            );
        }
        xml.push_str("    </gating:Quadrant>\n");
    }
    xml.push_str("  </gating:QuadrantGate>\n");
}

fn write_gate(xml: &mut String, gate: &Gate) {
    let parent = gate.parent_id.as_ref().map_or(String::new(), |parent| {
        format!(" gating:parent_id=\"{}\"", escape(parent))
    });
    let element = gate.gate_type();
    let _ = writeln!(
        xml,
        "  <gating:{element} gating:id=\"{}\"{parent}>",
        escape(&gate.id)
    );
    for dimension in gate.dimensions() {
        write_dimension(xml, dimension);
    }
    match &gate.kind {
        GateKind::Rectangle { .. } => {}
        GateKind::Quadrant { .. } => unreachable!("quadrants are written by write_quadrant_gate"),
        GateKind::Polygon { vertices, .. } => {
            for [x, y] in vertices {
                let _ = writeln!(
                    xml,
                    "    <gating:vertex><gating:coordinate data-type:value=\"{x}\"/>\
                         <gating:coordinate data-type:value=\"{y}\"/></gating:vertex>"
                );
            }
        }
        GateKind::Ellipsoid {
            mean,
            covariance,
            distance_square,
            ..
        } => {
            xml.push_str("    <gating:mean>");
            for value in mean {
                let _ = write!(xml, "<gating:coordinate data-type:value=\"{value}\"/>");
            }
            xml.push_str("</gating:mean>\n    <gating:covarianceMatrix>\n");
            for row in covariance.chunks(mean.len().max(1)) {
                xml.push_str("      <gating:row>");
                for value in row {
                    let _ = write!(xml, "<gating:entry data-type:value=\"{value}\"/>");
                }
                xml.push_str("</gating:row>\n");
            }
            let _ = writeln!(
                xml,
                "    </gating:covarianceMatrix>\n    \
                     <gating:distanceSquare data-type:value=\"{distance_square}\"/>"
            );
        }
        GateKind::Boolean { op, refs } => {
            let op = match op {
                BooleanOp::And => "and",
                BooleanOp::Or => "or",
                BooleanOp::Not => "not",
            };
            let _ = writeln!(xml, "    <gating:{op}>");
            for reference in refs {
                let complement = if reference.complement {
                    " gating:use-as-complement=\"true\""
                } else {
                    ""
                };
                let _ = writeln!(
                    xml,
                    "      <gating:gateReference gating:ref=\"{}\"{complement}/>",
                    escape(&reference.id)
                );
            }
            let _ = writeln!(xml, "    </gating:{op}>");
        }
    }
    let _ = writeln!(xml, "  </gating:{element}>");
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::{GateRef, SpectrumMatrix};
    use crate::parse::parse_gatingml;

    fn dimension(name: &str, compensation: CompensationRef, transform: Option<&str>) -> Dimension {
        Dimension {
            name: name.to_owned(),
            compensation,
            transform: transform.map(str::to_owned),
            min: None,
            max: None,
        }
    }

    #[test]
    fn written_document_parses_back_to_the_same_strategy() {
        let transforms = HashMap::from([
            (
                "asinh".to_owned(),
                Transform::Asinh {
                    t: 262_144.0,
                    m: 4.5,
                    a: 0.0,
                },
            ),
            ("lin".to_owned(), Transform::Linear { t: 1e5, a: 0.1 }),
            ("log".to_owned(), Transform::Log { t: 1e5, m: 5.0 }),
        ]);
        let matrices = HashMap::from([(
            "comp".to_owned(),
            SpectrumMatrix {
                fluorochromes: vec!["FITC".to_owned(), "PE".to_owned()],
                detectors: vec!["FL1-A".to_owned(), "FL2-A".to_owned()],
                values: vec![1.0, 0.125, 0.3, 1.0],
            },
        )]);
        let mut live = dimension("Live/Dead <A&B>", CompensationRef::Fcs, Some("asinh"));
        live.max = Some(0.312_345_678_9);
        let gates = vec![
            Gate {
                id: "clean".to_owned(),
                parent_id: None,
                kind: GateKind::Boolean {
                    op: BooleanOp::And,
                    refs: vec![
                        GateRef {
                            id: "debris".to_owned(),
                            complement: false,
                        },
                        GateRef {
                            id: "live".to_owned(),
                            complement: true,
                        },
                    ],
                },
            },
            Gate {
                id: "debris".to_owned(),
                parent_id: None,
                kind: GateKind::Polygon {
                    dimensions: [
                        dimension("FSC-A", CompensationRef::Uncompensated, Some("lin")),
                        dimension("SSC-A", CompensationRef::Uncompensated, Some("log")),
                    ],
                    vertices: vec![[0.1, 0.2], [0.9, 0.2], [0.5, 1.0 / 3.0]],
                },
            },
            Gate {
                id: "live".to_owned(),
                parent_id: Some("debris".to_owned()),
                kind: GateKind::Rectangle {
                    dimensions: vec![live],
                },
            },
            Gate {
                id: "ellipse".to_owned(),
                parent_id: None,
                kind: GateKind::Ellipsoid {
                    dimensions: vec![
                        dimension("FITC", CompensationRef::Matrix("comp".to_owned()), None),
                        dimension("PE", CompensationRef::Matrix("comp".to_owned()), None),
                    ],
                    mean: vec![1.0, 2.0],
                    covariance: vec![4.0, 1.0, 1.0, 9.0],
                    distance_square: 2.5,
                },
            },
        ];
        let strategy = GatingStrategy::new(gates, transforms, matrices).unwrap();

        let xml = to_gatingml(&strategy).unwrap();
        let parsed = parse_gatingml(&xml).unwrap();
        assert_eq!(parsed, strategy);
        assert_eq!(to_gatingml(&parsed).unwrap(), xml);
    }

    #[test]
    fn logicle_and_hyperlog_round_trip_with_their_parameters() {
        let logicle = cytiq_transform::scales::Logicle::new(10_000.0, 0.7, 4.0, 0.25).unwrap();
        let hyperlog = cytiq_transform::scales::Hyperlog::new(262_144.0, 0.3, 5.0, 0.1).unwrap();
        let strategy = GatingStrategy::new(
            Vec::new(),
            HashMap::from([
                ("l".to_owned(), Transform::Logicle(logicle)),
                ("h".to_owned(), Transform::Hyperlog(hyperlog)),
            ]),
            HashMap::new(),
        )
        .unwrap();
        let xml = to_gatingml(&strategy).unwrap();
        assert!(xml.contains(
            r#"<transforms:logicle transforms:T="10000" transforms:W="0.7" transforms:M="4" transforms:A="0.25"/>"#
        ));
        assert!(xml.contains(
            r#"<transforms:hyperlog transforms:T="262144" transforms:W="0.3" transforms:M="5" transforms:A="0.1"/>"#
        ));
        let parsed = parse_gatingml(&xml).unwrap();
        assert_eq!(parsed, strategy);
        assert_eq!(to_gatingml(&parsed).unwrap(), xml);
    }
}
