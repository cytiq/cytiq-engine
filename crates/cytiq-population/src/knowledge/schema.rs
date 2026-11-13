//! The assertion vocabulary used by the six pinned upstream schemas.
//! No external references or executable schema extensions are accepted.

use crate::{PopulationError, Result, require};
use serde_json::Value;

pub(crate) fn normalize(value: &mut Value, schema: &Value, root: &Value) -> Result<()> {
    if let Some(reference) = schema["$ref"].as_str() {
        let reference = reference
            .strip_prefix('#')
            .and_then(|p| root.pointer(p))
            .ok_or_else(|| PopulationError::Invalid("unsupported schema reference".into()))?;
        return normalize(value, reference, root);
    }
    if let Some(choices) = schema["anyOf"].as_array() {
        for choice in choices {
            let mut candidate = value.clone();
            if normalize(&mut candidate, choice, root).is_ok() {
                *value = candidate;
                return Ok(());
            }
        }
        return Err(PopulationError::Invalid(
            "knowledge value matches no permitted type".into(),
        ));
    }
    if let Some(kind) = schema["type"].as_str() {
        let fits = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            _ => false,
        };
        require(fits, format!("knowledge schema expects {kind}"))?;
    }
    if let Some(constant) = schema.get("const") {
        require(value == constant, "invalid knowledge constant/version")?;
    }
    if let Some(options) = schema["enum"].as_array() {
        require(options.contains(value), "unsupported knowledge enum value")?;
    }
    normalize_scalar(value, schema)?;
    if let Some(items) = value.as_array_mut() {
        if let Some(minimum) = schema["minItems"].as_u64() {
            require(items.len() as u64 >= minimum, "too few knowledge items")?;
        }
        for item in items {
            normalize(item, &schema["items"], root)?;
        }
    }
    if let Some(object) = value.as_object_mut() {
        if let Some(properties) = schema["properties"].as_object() {
            for (key, property) in properties {
                if !object.contains_key(key) {
                    if let Some(default) = property.get("default") {
                        object.insert(key.clone(), default.clone());
                    } else if !schema["required"]
                        .as_array()
                        .is_some_and(|keys| keys.iter().any(|k| k == key))
                    {
                        // Upstream Pydantic factory defaults do not carry a
                        // JSON Schema `default` annotation. Its optional
                        // collection fields normalize to empty collections.
                        match property["type"].as_str() {
                            Some("array") => {
                                object.insert(key.clone(), Value::Array(Vec::new()));
                            }
                            Some("object") => {
                                object.insert(key.clone(), Value::Object(serde_json::Map::new()));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some(required) = schema["required"].as_array() {
            for key in required {
                require(
                    object.contains_key(key.as_str().expect("schema key")),
                    format!("missing knowledge field: {key}"),
                )?;
            }
        }
        for (key, item) in object {
            if let Some(property) = schema["properties"].get(key) {
                normalize(item, property, root)?;
            } else if schema["additionalProperties"] == false {
                return Err(PopulationError::Invalid(format!(
                    "unknown knowledge field: {key}"
                )));
            } else if schema["additionalProperties"].is_object() {
                normalize(item, &schema["additionalProperties"], root)?;
            }
        }
    }
    Ok(())
}

fn normalize_scalar(value: &mut Value, schema: &Value) -> Result<()> {
    if let Some(string) = value.as_str() {
        if let Some(minimum) = schema["minLength"].as_u64() {
            require(
                string.chars().count() as u64 >= minimum,
                "empty knowledge identifier",
            )?;
        }
        if schema.get("pattern").is_some() {
            require(
                schema["pattern"] == "^[0-9a-f]{64}$",
                "unsupported pinned schema pattern",
            )?;
            require(
                string.len() == 64
                    && string
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid knowledge digest",
            )?;
        }
    }
    if let Some(number) = value.as_f64() {
        require(number.is_finite(), "nonfinite knowledge number")?;
        if let Some(minimum) = schema["minimum"].as_f64() {
            require(number >= minimum, "knowledge number below minimum")?;
        }
        if let Some(minimum) = schema["exclusiveMinimum"].as_f64() {
            require(number > minimum, "knowledge number below exclusive minimum")?;
        }
        if schema["type"] == "number" {
            *value = Value::from(number);
        }
    }
    Ok(())
}
