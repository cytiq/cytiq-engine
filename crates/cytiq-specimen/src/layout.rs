//! Protocol ownership: source bindings and policies, without copied ontology nodes.

use crate::{Result, require};
use cytiq_population::{
    Target,
    annotations::{KnowledgeRef, SourceDefinition},
    serialization,
};
use cytiq_qc::CheckStatus;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecimenLayout {
    pub schema_version: u32,
    pub protocol_id: String,
    pub panels: BTreeMap<String, PanelLayout>,
    #[serde(default)]
    pub reconciliation: Vec<ReconciliationRule>,
    #[serde(default)]
    pub consistency: Vec<ConsistencyPolicy>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelLayout {
    #[serde(default = "required_panel")]
    pub required: bool,
    pub projection: KnowledgeRef,
    pub sources: BTreeMap<String, SourceDefinition>,
}

const fn required_panel() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub panel: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Strategy {
    Independent,
    PreferReference {
        reference: SourceRef,
    },
    ReferenceRatio {
        source: SourceRef,
        reference: SourceRef,
        anchor: Target,
        /// Required: the limits come from assay validation, never a default.
        anchor_limits: AnchorLimits,
    },
}

/// Reliability limits of a `reference_ratio` anchor. A count below
/// `min_events` or a rescaling beyond `max_scale` rejects the derived
/// estimate; diverging anchor frequencies only warn. Measured observations
/// are never changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorLimits {
    /// Fewest anchor events accepted in either panel (Poisson CV `1/√n`).
    pub min_events: u64,
    /// Largest accepted `max(s, 1/s)`, `s = reference anchor / source anchor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_scale: Option<f64>,
    /// Largest ratio between the anchor's frequencies (count / assessed
    /// events) in the two panels before a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_frequency_ratio: Option<f64>,
}

impl AnchorLimits {
    /// # Errors
    /// Rejects a zero minimum and bounds that are not finite or below one.
    pub fn validate(&self) -> Result<()> {
        let bound = |limit: Option<f64>| limit.is_none_or(|v| v.is_finite() && v >= 1.0);
        require(
            self.min_events >= 1 && bound(self.max_scale) && bound(self.max_frequency_ratio),
            "invalid reference_ratio anchor limits",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRule {
    pub target: Target,
    pub strategy: Strategy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsistencyPolicy {
    pub target: Target,
    pub max_abs_fraction_difference: f64,
    pub severity: CheckStatus,
}

impl SpecimenLayout {
    /// Resolve definition paths relative to the layout, verify references and source bindings.
    /// # Errors
    /// Rejects unsupported versions, unavailable knowledge and inconsistent policies.
    pub fn load(path: &Path) -> Result<Self> {
        let mut layout: Self = serde_json::from_value(serialization::read(path)?)?;
        let base = std::fs::canonicalize(path)?
            .parent()
            .unwrap_or(Path::new("."))
            .to_owned();
        for panel in layout.panels.values_mut() {
            resolve(&base, &mut panel.projection.path);
            for source in panel.sources.values_mut() {
                resolve(&base, &mut source.definition.path);
                if let Some(bundle) = &mut source.bundle_manifest {
                    resolve(&base, &mut bundle.path);
                }
                if let Some(policy) = &mut source.decision_policy {
                    resolve(&base, &mut policy.path);
                }
                if let Some(vocabulary) = &mut source.vocabulary {
                    resolve(&base, &mut vocabulary.path);
                }
            }
        }
        layout.validate()?;
        Ok(layout)
    }

    /// # Errors
    /// Rejects invalid definitions or incompatible reconciliation sources.
    pub fn validate(&mut self) -> Result<()> {
        require(
            self.schema_version == 1
                && !self.protocol_id.trim().is_empty()
                && !self.panels.is_empty(),
            "invalid layout identity/version",
        )?;
        let mut identity = None;
        for (name, panel) in &mut self.panels {
            require(
                !name.trim().is_empty() && !panel.sources.is_empty(),
                "panel requires a name and sources",
            )?;
            let projection = panel.projection.load()?;
            require(
                projection.value()["panel"]["panel"] == *name,
                "layout panel/projection name mismatch",
            )?;
            let current = (
                projection.value()["tree_sha256"].clone(),
                projection.value()["tree"]["context"].clone(),
            );
            if let Some(previous) = &identity {
                require(
                    previous == &current,
                    "specimen panels require the same tree and context",
                )?;
            }
            identity = Some(current);
            for (source_id, source) in &mut panel.sources {
                require(!source_id.trim().is_empty(), "empty source id")?;
                source.resolve(&projection)?;
            }
        }
        let mut targets = std::collections::BTreeSet::new();
        for rule in &self.reconciliation {
            rule.target.validate()?;
            require(
                targets.insert(&rule.target),
                "multiple reconciliation rules for one target",
            )?;
            match &rule.strategy {
                Strategy::Independent => require(
                    self.panels.values().any(|p| {
                        p.sources
                            .values()
                            .any(|s| s.bindings.iter().any(|b| b.target == rule.target))
                    }),
                    "unknown independent target",
                )?,
                Strategy::PreferReference { reference } => {
                    self.require_target(reference, &rule.target)?;
                }
                Strategy::ReferenceRatio {
                    source,
                    reference,
                    anchor,
                    anchor_limits,
                } => {
                    anchor.validate()?;
                    anchor_limits.validate()?;
                    require(
                        source.panel != reference.panel && anchor.scope == rule.target.scope,
                        "reference ratio requires different panels and compatible scopes",
                    )?;
                    self.require_target(source, &rule.target)?;
                    self.require_target(source, anchor)?;
                    self.require_target(reference, anchor)?;
                }
            }
        }
        for policy in &self.consistency {
            policy.target.validate()?;
            require(
                policy.max_abs_fraction_difference.is_finite()
                    && (0.0..=1.0).contains(&policy.max_abs_fraction_difference)
                    && policy.severity != CheckStatus::Pass,
                "invalid consistency policy threshold/severity",
            )?;
        }
        Ok(())
    }

    fn require_target(&self, reference: &SourceRef, target: &Target) -> Result<()> {
        require(
            self.panels
                .get(&reference.panel)
                .and_then(|p| p.sources.get(&reference.source))
                .is_some_and(|s| s.bindings.iter().any(|b| &b.target == target)),
            "reconciliation source does not define the target",
        )
    }

    #[must_use]
    pub fn input_files(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for panel in self.panels.values() {
            paths.push(panel.projection.path.clone());
            for source in panel.sources.values() {
                paths.push(source.definition.path.clone());
                paths.extend(source.bundle_manifest.iter().map(|v| v.path.clone()));
                paths.extend(source.decision_policy.iter().map(|v| v.path.clone()));
                paths.extend(source.vocabulary.iter().map(|v| v.path.clone()));
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

fn resolve(base: &Path, path: &mut PathBuf) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}
