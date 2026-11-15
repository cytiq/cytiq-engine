//! Stable artifact JSON, compatible with Python's json.dumps used by cytiq-tree.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::PopulationError;

mod unique;

pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_VALUES: usize = 1_000_000;

fn bounded(
    value: &Value,
    depth: usize,
    values: &mut usize,
    strings: &mut usize,
) -> Result<(), PopulationError> {
    *values += 1;
    if depth > 64 || *values > MAX_VALUES || *strings > 16 * 1024 * 1024 {
        return Err(invalid("document exceeds knowledge artifact limits"));
    }
    match value {
        Value::String(s) => *strings += s.len(),
        Value::Array(items) => {
            for item in items {
                bounded(item, depth + 1, values, strings)?;
            }
        }
        Value::Object(items) => {
            for (key, item) in items {
                *strings += key.len();
                bounded(item, depth + 1, values, strings)?;
            }
        }
        _ => {}
    }
    if *strings > 16 * 1024 * 1024 {
        return Err(invalid("document string limit exceeded"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> PopulationError {
    PopulationError::Invalid(message.into())
}

/// Resolve exactly one serialization of an artifact.
/// # Errors
/// Rejects absent and ambiguous filenames.
pub fn resolve(directory: &Path, stem: &str) -> Result<PathBuf, PopulationError> {
    let paths: Vec<_> = ["json", "yaml", "yml"]
        .iter()
        .map(|extension| directory.join(format!("{stem}.{extension}")))
        .filter(|path| path.exists())
        .collect();
    match paths.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err(invalid(format!("missing bundle file: {stem}"))),
        _ => Err(invalid(format!("ambiguous bundle files: {stem}"))),
    }
}

/// How a document is encoded: `.json` files are JSON, everything else YAML.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentFormat {
    Json,
    Yaml,
}

impl DocumentFormat {
    #[must_use]
    pub fn of(path: &Path) -> Self {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            Self::Json
        } else {
            Self::Yaml
        }
    }
}

/// Read a bounded JSON-compatible mapping.
/// # Errors
/// Rejects unsupported types, duplicate keys and excessive input.
pub fn read(path: &Path) -> Result<Value, PopulationError> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
    parse_document(&bytes, DocumentFormat::of(path), &path.display())
}

/// [`read`] for bytes already in memory, with the same limits and checks.
/// # Errors
/// Rejects non-UTF-8 input, unsupported types, duplicate keys and excessive input.
pub fn parse(bytes: &[u8], format: DocumentFormat) -> Result<Value, PopulationError> {
    parse_document(bytes, format, &"document")
}

fn parse_document(
    bytes: &[u8],
    format: DocumentFormat,
    origin: &dyn std::fmt::Display,
) -> Result<Value, PopulationError> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("document exceeds 16 MiB"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let payload: Value = match format {
        DocumentFormat::Json => serde_json::from_str::<unique::UniqueValue>(text)?.0,
        DocumentFormat::Yaml => {
            // Preflight explicit tags before type-erasing into JSON; binary and dates
            // must not quietly turn into strings in one language but not the other.
            for event in serde_saphyr::granit_parser::Parser::new_from_str(text) {
                let (event, _) = event.map_err(|error| invalid(error.to_string()))?;
                if event.tag().is_some_and(|tag| tag.core_suffix().is_none()) {
                    return Err(invalid("unsupported YAML tag"));
                }
            }
            let options = serde_saphyr::options! {
                budget: serde_saphyr::budget! {
                    max_depth: 64,
                    max_nodes: 1_000_000,
                    max_total_scalar_bytes: 16 * 1024 * 1024,
                    max_documents: 1,
                },
                strict_booleans: true,
                no_schema: true,
                reject_unsupported_tags: true,
                merge_keys: serde_saphyr::MergeKeyPolicy::Error,
                non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::Reject,
            };
            serde_saphyr::from_str_with_options(text, options)
                .map_err(|error| invalid(format!("invalid YAML {origin}: {error}")))?
        }
    };
    if !payload.is_object() {
        return Err(invalid("expected an object/mapping"));
    }
    bounded(&payload, 0, &mut 0, &mut 0)?;
    Ok(payload)
}

/// The canonical text of `payload`: Python's `json.dumps(indent=2,
/// sort_keys=True, ensure_ascii=True)` followed by a newline.
#[must_use]
pub fn canonical(payload: &Value) -> String {
    let mut text = String::new();
    encode(payload, 0, &mut text);
    text.push('\n');
    text
}

/// SHA-256 of [`canonical`].
#[must_use]
pub fn digest(payload: &Value) -> String {
    let text = canonical(payload);
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        })
}

fn encode(value: &Value, depth: usize, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) if value.is_f64() => {
            output.push_str(&python_float(value.as_f64().expect("float JSON number")));
        }
        Value::Number(value) => write!(output, "{value}").unwrap(),
        Value::String(value) => string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                prefix(index, depth, output);
                encode(value, depth + 1, output);
            }
            finish(values.is_empty(), depth, ']', output);
        }
        Value::Object(values) => {
            output.push('{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                prefix(index, depth, output);
                string(key, output);
                output.push_str(": ");
                encode(value, depth + 1, output);
            }
            finish(values.is_empty(), depth, '}', output);
        }
    }
}

fn prefix(index: usize, depth: usize, output: &mut String) {
    if index > 0 {
        output.push(',');
    }
    output.push('\n');
    output.push_str(&"  ".repeat(depth + 1));
}

fn finish(empty: bool, depth: usize, closing: char, output: &mut String) {
    if !empty {
        output.push('\n');
        output.push_str(&"  ".repeat(depth));
    }
    output.push(closing);
}

fn string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            ' '..='~' => output.push(character),
            _ => {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    write!(output, "\\u{unit:04x}").unwrap();
                }
            }
        }
    }
    output.push('"');
}

fn python_float(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let mut buffer = ryu::Buffer::new();
    let raw = buffer.format_finite(value.abs());
    let (mantissa, exponent) = raw.split_once('e').unwrap_or((raw, "0"));
    let dot = mantissa.find('.').unwrap_or(mantissa.len());
    let mut exponent: i32 = exponent.parse::<i32>().unwrap() + i32::try_from(dot).unwrap() - 1;
    let mut digits = mantissa.replace('.', "");
    while digits.starts_with('0') {
        digits.remove(0);
        exponent -= 1;
    }
    let digits = digits.trim_end_matches('0');
    let mut result = if value.is_sign_negative() { "-" } else { "" }.to_owned();
    if !(-4..16).contains(&exponent) {
        result.push_str(&digits[..1]);
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        write!(result, "e{exponent:+03}").unwrap();
    } else if exponent < 0 {
        result.push_str("0.");
        result.push_str(&"0".repeat(usize::try_from(-exponent - 1).unwrap()));
        result.push_str(digits);
    } else {
        let point = usize::try_from(exponent + 1).unwrap();
        if point >= digits.len() {
            result.push_str(digits);
            result.push_str(&"0".repeat(point - digits.len()));
            result.push_str(".0");
        } else {
            result.push_str(&digits[..point]);
            result.push('.');
            result.push_str(&digits[point..]);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_budgets_accept_the_boundary_and_reject_larger_documents() {
        let check = |value: &Value| bounded(value, 0, &mut 0, &mut 0);
        let mut nested = Value::Null;
        for _ in 0..64 {
            nested = Value::Array(vec![nested]);
        }
        assert!(check(&nested).is_ok());
        nested = Value::Array(vec![nested]);
        assert!(check(&nested).is_err());
        nested = Value::Array(vec![nested]);
        assert!(check(&nested).is_err());
        for (nodes, valid) in [
            (MAX_VALUES - 1, true),
            (MAX_VALUES, false),
            (MAX_VALUES + 1, false),
        ] {
            assert_eq!(
                check(&Value::Array(vec![Value::Null; nodes])).is_ok(),
                valid
            );
        }
        for (bytes, valid) in [
            (16 * 1024 * 1024, true),
            (16 * 1024 * 1024 + 1, false),
            (16 * 1024 * 1024 + 2, false),
        ] {
            assert_eq!(check(&Value::String("x".repeat(bytes))).is_ok(), valid);
            let mut map = serde_json::Map::new();
            map.insert("x".repeat(bytes), Value::Null);
            assert_eq!(check(&Value::Object(map)).is_ok(), valid);
        }
    }

    #[test]
    fn byte_budget_counts_whitespace_and_accepts_exactly_16_mib() {
        let mut bytes = b"{}".to_vec();
        bytes.resize(usize::try_from(MAX_DOCUMENT_BYTES).unwrap(), b' ');
        assert!(parse(&bytes, DocumentFormat::Json).is_ok());
        for _ in 0..2 {
            bytes.push(b' ');
            assert!(parse(&bytes, DocumentFormat::Json).is_err());
        }
    }

    #[test]
    fn yaml_rejects_ambiguous_mappings_and_non_json_types() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("invalid.yaml");
        for text in [
            "a: 1\na: 2",
            "nested: {a: 1, a: 2}",
            "labels: {0: a}",
            "labels: {true: a}",
            "value: .nan",
            "value: .inf",
            "value: !!binary YQ==",
            "value: !!timestamp 2026-10-27",
            "value: !!bool yes",
            "value: !custom x",
            "value: &loop [*loop]",
            "value: {<<: {a: b}}",
            "[]",
            "",
            "null",
            "a: b\n---\nc: d",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(read(&path).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn shared_python_vectors_have_identical_bytes_and_digests() {
        let vectors: Vec<Value> =
            serde_json::from_str(include_str!("../contracts/serialization-vectors.json")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        for vector in vectors {
            let path = temp.path().join("vector.yaml");
            std::fs::write(&path, vector["yaml"].as_str().unwrap()).unwrap();
            let payload = read(&path).unwrap();
            let mut canonical = String::new();
            encode(&payload, 0, &mut canonical);
            canonical.push('\n');
            assert_eq!(canonical, vector["canonical"].as_str().unwrap());
            assert_eq!(digest(&payload), vector["sha256"].as_str().unwrap());
            let json: Value = serde_json::from_str(&canonical).unwrap();
            assert_eq!(digest(&json), digest(&payload));
        }
    }
}
