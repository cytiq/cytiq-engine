//! Runtime readers for the schemas owned by cytiq-tree, never a second ontology.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::{Result, require, serialization};

pub(crate) mod schema;

/// A validated, default-expanded v2 knowledge artifact.
#[derive(Clone, Debug)]
pub struct KnowledgeArtifact {
    pub(crate) value: Value,
    pub(crate) sha256: String,
}

impl KnowledgeArtifact {
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Load using the vendored upstream schema and verify internal identities.
    /// # Errors
    /// Rejects unsupported kinds, malformed schemas and inconsistent references.
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_value(serialization::read(path)?)
    }

    /// # Errors
    /// Returns an error for an invalid v2 artifact.
    /// # Panics
    /// Never: the pinned schema validates every field before semantic checks.
    pub fn from_value(mut value: Value) -> Result<Self> {
        let kind = text(&value, "artifact_type")?.to_owned();
        let definition = match kind.as_str() {
            "curated_tree" => include_str!("../contracts/v2/curated_tree.schema.json"),
            "marker_registry" => include_str!("../contracts/v2/marker_registry.schema.json"),
            "panel_profile" => include_str!("../contracts/v2/panel_profile.schema.json"),
            "panel_projection" => include_str!("../contracts/v2/panel_projection.schema.json"),
            "class_vocabulary" => include_str!("../contracts/v2/class_vocabulary.schema.json"),
            "label_mapping" => include_str!("../contracts/v2/label_mapping.schema.json"),
            _ => {
                return Err(crate::PopulationError::Invalid(
                    "unsupported knowledge artifact".into(),
                ));
            }
        };
        let definition: Value = serde_json::from_str(definition)?;
        schema::normalize(&mut value, &definition, &definition)?;
        match kind.as_str() {
            "curated_tree" => validate_tree(&value)?,
            "panel_profile" => validate_panel(&value)?,
            "marker_registry" => validate_registry(&value)?,
            "panel_projection" => validate_projection(&value)?,
            "class_vocabulary" => validate_vocabulary(&value)?,
            "label_mapping" => {
                let labels = value["labels"].as_object().expect("validated mapping");
                for ignored in value["ignored_labels"].as_array().expect("validated array") {
                    require(
                        !labels.contains_key(ignored.as_str().expect("validated label")),
                        "mapped label is also ignored",
                    )?;
                }
            }
            _ => unreachable!(),
        }
        Ok(Self {
            sha256: serialization::digest(&value),
            value,
        })
    }
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| crate::PopulationError::Invalid(format!("missing string: {key}")))
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, description: &str) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        require(
            seen.insert(value),
            format!("duplicate {description}: {value}"),
        )?;
    }
    Ok(())
}

/// Parent links establish membership ancestry only, never count additivity.
/// # Errors
/// Rejects missing parents, cycles, multiple roots and excessive depth.
pub fn validate_hierarchy(parents: &BTreeMap<String, Option<String>>) -> Result<()> {
    require(
        parents.values().filter(|parent| parent.is_none()).count() == 1,
        "hierarchy requires one root",
    )?;
    for node in parents.keys() {
        let mut active = BTreeSet::new();
        let mut current = Some(node.as_str());
        while let Some(id) = current {
            require(
                active.len() < 64 && active.insert(id),
                "cyclic or excessively deep hierarchy",
            )?;
            let parent = parents
                .get(id)
                .ok_or_else(|| crate::PopulationError::Invalid(format!("missing parent: {id}")))?;
            current = parent.as_deref();
        }
    }
    Ok(())
}

pub(crate) fn parents(tree: &Value) -> BTreeMap<String, Option<String>> {
    tree["cells"]
        .as_array()
        .expect("validated cells")
        .iter()
        .map(|cell| {
            (
                cell["id"].as_str().expect("validated id").to_owned(),
                cell["parent"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn validate_registry(registry: &Value) -> Result<()> {
    let markers = registry["markers"].as_array().expect("validated markers");
    unique(
        markers
            .iter()
            .map(|m| m["id"].as_str().expect("validated id")),
        "marker",
    )?;
    let mut aliases = BTreeMap::new();
    for marker in markers {
        for alias in std::iter::once(&marker["id"])
            .chain(marker["aliases"].as_array().expect("validated aliases"))
        {
            let key = (
                text(marker, "measurement_kind")?,
                alias
                    .as_str()
                    .expect("validated alias")
                    .trim()
                    .to_lowercase(),
            );
            if let Some(previous) = aliases.insert(key, &marker["id"]) {
                require(previous == &marker["id"], "ambiguous marker alias")?;
            }
        }
    }
    Ok(())
}

fn validate_tree(tree: &Value) -> Result<()> {
    let cells = tree["cells"].as_array().expect("validated cells");
    unique(
        cells
            .iter()
            .map(|cell| cell["id"].as_str().expect("validated id")),
        "population",
    )?;
    validate_hierarchy(&parents(tree))?;
    validate_registry(&tree["registry"])?;
    let ids: BTreeSet<_> = cells
        .iter()
        .map(|cell| cell["id"].as_str().expect("validated id"))
        .collect();
    let phenotypes = tree["phenotypes"].as_array().expect("validated phenotypes");
    unique(
        phenotypes
            .iter()
            .map(|p| p["id"].as_str().expect("validated id")),
        "phenotype",
    )?;
    let tasks = tree["tasks"].as_array().expect("validated tasks");
    unique(
        tasks
            .iter()
            .map(|t| t["id"].as_str().expect("validated id")),
        "task",
    )?;
    for task in tasks {
        require(ids.contains(text(task, "scope")?), "unknown task scope")?;
        unique(
            task["classes"]
                .as_array()
                .expect("validated classes")
                .iter()
                .map(|c| c["id"].as_str().expect("validated id")),
            "task class",
        )?;
        validate_task(task, &ids, phenotypes)?;
    }
    validate_signatures(tree)?;
    validate_ontology(tree)?;
    for mapping in tree["mappings"].as_array().expect("validated mappings") {
        let task = tasks
            .iter()
            .find(|t| t["id"] == mapping["task"])
            .ok_or_else(|| {
                crate::PopulationError::Invalid("mapping references unknown task".into())
            })?;
        let classes = task["classes"].as_array().expect("validated classes");
        let labels = mapping["labels"].as_object().expect("validated mapping");
        require(
            labels
                .values()
                .all(|label| classes.iter().any(|c| c["id"] == *label))
                && mapping["ignored_labels"]
                    .as_array()
                    .expect("validated labels")
                    .iter()
                    .all(|label| !labels.contains_key(label.as_str().expect("validated label"))),
            "invalid tree label mapping",
        )?;
    }
    Ok(())
}

fn validate_task(task: &Value, ids: &BTreeSet<&str>, phenotypes: &[Value]) -> Result<()> {
    let classes = task["classes"].as_array().expect("validated classes");
    for class in classes {
        require(
            class["id"] != "unknown",
            "unknown is reserved for abstention",
        )?;
        let target = text(class, "target")?;
        match text(class, "kind")? {
            "population" => require(
                ids.contains(target) && class["state"].is_null(),
                "invalid population task target",
            )?,
            "phenotype" => require(
                phenotypes.iter().any(|p| p["id"] == target) && class["state"].is_boolean(),
                "invalid phenotype task target",
            )?,
            "residual" => require(
                class["residual_definition"]["scope"] == task["scope"] && class["state"].is_null(),
                "invalid residual task scope",
            )?,
            _ => unreachable!(),
        }
    }
    if task["kind"] == "binary_phenotype" {
        require(
            classes.len() == 2
                && classes[0]["kind"] == "phenotype"
                && classes[1]["kind"] == "phenotype"
                && classes[0]["target"] == classes[1]["target"]
                && classes[0]["state"] == false
                && classes[1]["state"] == true,
            "invalid binary task definition",
        )?;
    } else {
        require(
            classes.iter().all(|c| c["kind"] != "phenotype"),
            "phenotype cannot be a multiclass population target",
        )?;
    }
    Ok(())
}

fn validate_signatures(tree: &Value) -> Result<()> {
    let cells = tree["cells"].as_array().expect("validated cells");
    let nodes: BTreeMap<_, _> = cells
        .iter()
        .map(|c| (c["id"].as_str().expect("validated id"), c))
        .collect();
    let markers: BTreeSet<_> = tree["registry"]["markers"]
        .as_array()
        .expect("validated markers")
        .iter()
        .map(|m| m["id"].as_str().expect("validated id"))
        .collect();
    for cell in cells {
        let mut states: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut current = Some(cell);
        while let Some(node) = current {
            let mut unique = BTreeSet::new();
            for signature in node["marker_signature"]
                .as_array()
                .expect("validated signatures")
            {
                let marker = text(signature, "marker")?;
                let state = text(signature, "state")?;
                require(
                    markers.contains(marker) && unique.insert((marker, state)),
                    "unknown or duplicate marker signature",
                )?;
                if node == cell || signature["inherited"] == true {
                    states.entry(marker).or_default().insert(state);
                }
            }
            current = node["parent"].as_str().map(|id| nodes[id]);
        }
        for states in states.values() {
            let known: BTreeSet<_> = states.iter().copied().filter(|s| *s != "unknown").collect();
            require(
                !(known.contains("negative") && known.len() > 1
                    || known.contains("dim") && known.contains("bright")),
                "contradictory marker states",
            )?;
        }
    }
    Ok(())
}

fn validate_ontology(tree: &Value) -> Result<()> {
    let mut graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in tree["ontology_edges"].as_array().expect("validated edges") {
        graph
            .entry(text(edge, "child")?)
            .or_default()
            .push(text(edge, "parent")?);
    }
    let mut done = BTreeSet::new();
    for node in graph.keys() {
        let mut active = BTreeSet::new();
        let mut stack = vec![(*node, false)];
        while let Some((current, leaving)) = stack.pop() {
            if leaving {
                active.remove(current);
                done.insert(current);
            } else if !done.contains(current) {
                require(active.insert(current), "cyclic ontology relationships")?;
                stack.push((current, true));
                stack.extend(
                    graph
                        .get(current)
                        .into_iter()
                        .flatten()
                        .map(|parent| (*parent, false)),
                );
            }
        }
    }
    Ok(())
}

fn validate_panel(panel: &Value) -> Result<()> {
    let features = panel["features"].as_array().expect("validated features");
    for key in ["name", "channel"] {
        unique(
            features
                .iter()
                .map(|f| f[key].as_str().expect("validated feature")),
            key,
        )?;
    }
    unique(
        panel["resolution_rules"]
            .as_array()
            .expect("validated rules")
            .iter()
            .map(|r| r["population"].as_str().expect("validated population")),
        "resolution rule",
    )
}

fn validate_projection(value: &Value) -> Result<()> {
    let tree = &value["tree"];
    let panel = &value["panel"];
    validate_tree(tree)?;
    validate_panel(panel)?;
    require(
        tree["context"] == panel["context"],
        "tree/panel context mismatch",
    )?;
    require(
        serialization::digest(tree) == text(value, "tree_sha256")?
            && serialization::digest(panel) == text(value, "panel_sha256")?,
        "projection knowledge checksum mismatch",
    )?;
    let entries = value["populations"].as_array().expect("validated entries");
    unique(
        entries
            .iter()
            .map(|p| p["population"].as_str().expect("validated id")),
        "projection population",
    )?;
    let tree_parents = parents(tree);
    let expected = expected_projection(tree, panel, &tree_parents)?;
    require(
        entries.len() == tree_parents.len(),
        "projection must describe every population",
    )?;
    for entry in entries {
        require(
            tree_parents.contains_key(text(entry, "population")?),
            "unknown projection population",
        )?;
        require(
            expected.get(text(entry, "population")?) == Some(entry),
            "edited or inconsistent panel projection status",
        )?;
        if let Some(parent) = entry["resolved_parent"].as_str() {
            require(tree_parents.contains_key(parent), "unknown resolved parent")?;
        }
    }
    Ok(())
}

fn validate_vocabulary(value: &Value) -> Result<()> {
    let hierarchy: BTreeMap<String, Option<String>> =
        serde_json::from_value(value["hierarchy"].clone())?;
    validate_hierarchy(&hierarchy)?;
    require(
        hierarchy.contains_key(text(value, "scope")?),
        "unknown vocabulary scope",
    )?;
    let classes = value["classes"].as_array().expect("validated classes");
    unique(
        classes
            .iter()
            .map(|c| c["id"].as_str().expect("validated id")),
        "class",
    )?;
    for (index, class) in classes.iter().enumerate() {
        require(
            class["index"].as_u64() == Some(index as u64) && class["id"] != "unknown",
            "invalid class order or reserved id",
        )?;
        if class["kind"] == "population" {
            let target = text(class, "target")?;
            require(
                hierarchy.contains_key(target)
                    && serde_json::to_value(&hierarchy[target])? == class["parent"],
                "invalid population class target",
            )?;
        }
        if class["kind"] == "residual" {
            require(
                class["residual_definition"]["scope"] == value["scope"],
                "invalid residual scope",
            )?;
        }
    }
    if value["task_kind"] == "binary_phenotype" {
        require(
            classes.len() == 2
                && classes[0]["kind"] == "phenotype"
                && classes[1]["kind"] == "phenotype"
                && classes[0]["target"] == classes[1]["target"]
                && classes[0]["state"] == false
                && classes[1]["state"] == true
                && value["decision_rule"] == "binary_threshold",
            "invalid binary phenotype task",
        )?;
    } else {
        require(
            value["decision_rule"] == "argmax",
            "invalid multiclass decision rule",
        )?;
        for class in classes.iter().filter(|c| c["kind"] == "population") {
            let mut parent = hierarchy[text(class, "target")?].as_deref();
            while let Some(id) = parent {
                require(
                    !classes.iter().any(|other| other["target"] == id),
                    "classes must form a frontier",
                )?;
                parent = hierarchy[id].as_deref();
            }
        }
    }
    Ok(())
}

fn expected_projection<'a>(
    tree: &Value,
    panel: &Value,
    tree_parents: &'a BTreeMap<String, Option<String>>,
) -> Result<BTreeMap<&'a str, Value>> {
    let markers: BTreeSet<_> = tree["registry"]["markers"]
        .as_array()
        .expect("validated markers")
        .iter()
        .map(|m| m["id"].as_str().expect("validated id"))
        .collect();
    let features = panel["features"].as_array().expect("validated features");
    require(
        features
            .iter()
            .all(|f| markers.contains(f["marker_id"].as_str().expect("validated marker"))),
        "panel references unknown markers",
    )?;
    let measured: BTreeSet<_> = features
        .iter()
        .map(|f| f["name"].as_str().expect("validated feature"))
        .collect();
    let rules: BTreeMap<_, _> = panel["resolution_rules"]
        .as_array()
        .expect("validated rules")
        .iter()
        .map(|r| (r["population"].as_str().expect("validated population"), r))
        .collect();
    require(
        rules.keys().all(|id| tree_parents.contains_key(*id)),
        "resolution rule names unknown population",
    )?;
    let mut expected = BTreeMap::new();
    for id in tree_parents.keys() {
        let (status, reason, missing) = if let Some(rule) = rules.get(id.as_str()) {
            let missing: BTreeSet<_> = rule["required_features"]
                .as_array()
                .expect("validated required features")
                .iter()
                .map(|f| f.as_str().expect("validated feature"))
                .filter(|f| !measured.contains(f))
                .collect();
            (
                if missing.is_empty() {
                    "resolvable"
                } else {
                    "unobservable"
                },
                if missing.is_empty() {
                    "accepted panel-specific rule covered"
                } else {
                    "missing required measurements"
                },
                missing,
            )
        } else {
            (
                "ambiguous",
                "no accepted panel-specific rule",
                BTreeSet::new(),
            )
        };
        expected.insert(id.as_str(),serde_json::json!({"population":id,"status":status,"reason":reason,"missing_features":missing,"resolved_parent":null}));
    }
    let resolvable: BTreeSet<_> = expected
        .iter()
        .filter(|(_, e)| e["status"] == "resolvable")
        .map(|(id, _)| *id)
        .collect();
    for (id, entry) in &mut expected {
        if entry["status"] == "resolvable" {
            continue;
        }
        let mut parent = tree_parents[*id].as_deref();
        while let Some(ancestor) = parent {
            if resolvable.contains(ancestor) {
                entry["status"] = Value::String("parent_only".into());
                entry["resolved_parent"] = Value::String(ancestor.into());
                entry["reason"] = Value::String(format!(
                    "{}; resolvable ancestor: {ancestor}",
                    entry["reason"].as_str().expect("reason")
                ));
                break;
            }
            parent = tree_parents[ancestor].as_deref();
        }
    }
    Ok(expected)
}
