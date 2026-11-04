//! The license bundle's shape, shared by `build.rs` (which checks the file
//! before embedding it) and the app (which reads the embedded copy).
//!
//! `{"schemaVersion":1,"sections":[{"title":"…","text":"…"}]}`, UTF-8.
//! Unknown fields are refused so a newer generator is noticed, not ignored.

use serde_json::Value;

pub const SCHEMA_VERSION: u64 = 1;
/// Far above any real bundle; refuses a wrong file passed by mistake.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub text: String,
}

pub fn parse(text: &str) -> Result<Vec<Section>, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("larger than {MAX_BYTES} bytes"));
    }
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let object = value.as_object().ok_or("not a JSON object")?;
    if let Some(key) = object
        .keys()
        .find(|k| !matches!(k.as_str(), "schemaVersion" | "sections"))
    {
        return Err(format!("unknown field `{key}`"));
    }
    match object.get("schemaVersion").and_then(Value::as_u64) {
        Some(SCHEMA_VERSION) => {}
        Some(other) => return Err(format!("unsupported schemaVersion {other}")),
        None => return Err("missing numeric schemaVersion".into()),
    }
    let sections = object
        .get("sections")
        .and_then(Value::as_array)
        .ok_or("missing sections array")?;
    if sections.is_empty() {
        return Err("no sections".into());
    }
    sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let s = s
                .as_object()
                .ok_or_else(|| format!("section {i} is not an object"))?;
            if let Some(key) = s.keys().find(|k| !matches!(k.as_str(), "title" | "text")) {
                return Err(format!("section {i}: unknown field `{key}`"));
            }
            let field = |name: &str| {
                s.get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("section {i}: missing string `{name}`"))
            };
            let title = field("title")?;
            if title.trim().is_empty() {
                return Err(format!("section {i}: empty title"));
            }
            Ok(Section {
                title,
                text: field("text")?,
            })
        })
        .collect()
}
