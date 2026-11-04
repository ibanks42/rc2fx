//! Which license text a build embeds. Used by `build.rs`; tested in the app.

use super::schema;
use std::path::Path;

/// Embedded when a development build is given no license file. It says so
/// plainly rather than looking like the full notices.
pub const PLACEHOLDER: &str = r#"{"schemaVersion":1,"sections":[{"title":"Licenses not bundled in this development build","text":"This development build does not include the license notices. Released builds show the full RC2FX license and every third-party notice here."}]}"#;

/// The JSON to embed. A release build (`release`) must be given a valid
/// file; a development build without one gets [`PLACEHOLDER`].
pub fn resolve(release: bool, path: Option<&Path>) -> Result<String, String> {
    let Some(path) = path else {
        return if release {
            Err("RC2FX_LICENSES_JSON is not set; release builds must embed the generated license file (run `cargo xtask package`)".into())
        } else {
            Ok(PLACEHOLDER.into())
        };
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read license file {}: {e}", path.display()))?;
    schema::parse(&text).map_err(|e| format!("invalid license file {}: {e}", path.display()))?;
    Ok(text)
}
