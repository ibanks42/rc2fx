//! One notice document feeds the embedded dialog and Linux package HTML.
use crate::{Result, run};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

pub struct Document {
    pub json: Vec<u8>,
    pub html: Vec<u8>,
}

pub fn generate(root: &Path, extra: Vec<(String, String)>) -> Result<Document> {
    let mut sections = Vec::new();
    for (title, path) in [
        ("RC2FX and trained weights — MIT", "LICENSE-MIT"),
        (
            "DeepFilterNet — MIT",
            "crates/rc2fx-dfn/LICENSE-DEEPFILTERNET",
        ),
        (
            "Lucide and Feather icons — ISC / MIT",
            "licenses/LICENSE-LUCIDE",
        ),
        ("Velopack — MIT", "licenses/LICENSE-VELOPACK"),
        ("Third-party notices", "THIRD_PARTY_NOTICES.md"),
        ("Trained on", "licenses/TRAINING-CREDITS.txt"),
    ] {
        let text = fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"))?;
        sections.push((title.to_owned(), text));
    }
    sections.push(("Rust standard library".into(), rust_standard_library()?));
    // Keep each distinct notice, including its copyright holders; only merge
    // byte-identical texts, never all MIT texts into a generic MIT template.
    let mut groups: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for manifest in ["Cargo.toml", "crates/rc2fx-native/Cargo.toml"] {
        let output = Command::new("cargo")
            .current_dir(root)
            .args([
                "about",
                "generate",
                "--locked",
                "--format",
                "json",
                "--config",
                "about.toml",
                "--manifest-path",
                manifest,
            ])
            .output()
            .map_err(|e| format!("could not run cargo-about: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo-about failed for {manifest}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        merge_report(
            &mut groups,
            &serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?,
        )?;
    }
    // Velopack's launcher, updater and installer ship in the Windows and
    // macOS packages, and its updater (UpdateNix, a static musl build) in the
    // AppImage; their notices come from a report generated from Velopack's
    // source for those three targets by .github/scripts/velopack-licenses.py.
    let velopack = format!("licenses/velopack-{}.json", super::velopack::VPK_VERSION);
    let report: Value =
        serde_json::from_str(&fs::read_to_string(root.join(&velopack)).map_err(|e| {
            format!(
                "{velopack}: {e}\nRun .github/scripts/velopack-licenses.py for this vpk version."
            )
        })?)
        .map_err(|e| format!("{velopack}: {e}"))?;
    if report["velopack"].as_str() != Some(super::velopack::VPK_VERSION) {
        return Err(format!(
            "{velopack} is not for vpk {}",
            super::velopack::VPK_VERSION
        ));
    }
    merge_report(&mut groups, &report)?;
    for (name, texts) in groups {
        let mut text = String::new();
        for (notice, users) in texts {
            text.push_str(&format!(
                "Used by: {}\n\n{}\n\n",
                users.into_iter().collect::<Vec<_>>().join(", "),
                notice
            ));
        }
        sections.push((format!("Dependencies — {name}"), text));
    }
    sections.extend(extra);
    render(&sections)
}

fn rust_standard_library() -> Result<String> {
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("rustc could not locate its standard-library notices".into());
    }
    let sysroot = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let path = Path::new(sysroot.trim()).join("share/doc/rust/COPYRIGHT-library.html");
    let html = fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: {e}; install rustup component add rust-docs",
            path.display()
        )
    })?;
    html_text(&html)
}

/// Read the pinned toolchain's simple copyright HTML as character data. Fail
/// on an unfamiliar entity instead of silently dropping attribution text.
fn html_text(html: &str) -> Result<String> {
    let body = html
        .split_once("<body>")
        .and_then(|(_, s)| s.split_once("</body>"))
        .ok_or("Rust copyright document has no body")?
        .0;
    let mut result = String::new();
    let mut rest = body;
    while !rest.is_empty() {
        if rest.starts_with('<') {
            let end = rest.find('>').ok_or("unterminated HTML tag")?;
            result.push('\n');
            rest = &rest[end + 1..];
        } else if rest.starts_with('&') {
            let end = rest.find(';').ok_or("unterminated HTML entity")?;
            let entity = &rest[1..end];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                s if s.starts_with("#x") => u32::from_str_radix(&s[2..], 16)
                    .ok()
                    .and_then(char::from_u32),
                s if s.starts_with('#') => s[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            }
            .ok_or_else(|| format!("unknown copyright HTML entity {entity}"))?;
            result.push(decoded);
            rest = &rest[end + 1..];
        } else {
            let end = rest.find(['<', '&']).unwrap_or(rest.len());
            result.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    Ok(result)
}

fn merge_report(
    groups: &mut BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    report: &Value,
) -> Result {
    let licenses = report["licenses"]
        .as_array()
        .ok_or("cargo-about report has no licenses")?;
    if licenses.is_empty() {
        return Err("cargo-about report contains no licenses".into());
    }
    for license in licenses {
        let name = license["name"].as_str().ok_or("license has no name")?;
        let text = license["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("license has no text")?;
        let users = groups
            .entry(name.to_owned())
            .or_default()
            .entry(text.to_owned())
            .or_default();
        for used in license["used_by"]
            .as_array()
            .ok_or("license has no crate list")?
        {
            let name = used["crate"]["name"].as_str().ok_or("crate has no name")?;
            let version = used["crate"]["version"]
                .as_str()
                .ok_or("crate has no version")?;
            users.insert(format!("{name} {version}"));
        }
    }
    Ok(())
}

pub fn render(sections: &[(String, String)]) -> Result<Document> {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>RC2FX licenses and credits</title><style>body{font:16px system-ui;max-width:72em;margin:2em auto;padding:0 1em}pre{white-space:pre-wrap;overflow-wrap:anywhere}</style></head><body><h1>RC2FX licenses and credits</h1>",
    );
    let mut values = Vec::new();
    for (title, text) in sections {
        html.push_str(&format!(
            "<details><summary>{}</summary><pre>{}</pre></details>",
            escape(title),
            escape(text)
        ));
        values.push(json!({"title": title, "text": text}));
    }
    html.push_str("</body></html>");
    Ok(Document {
        json: serde_json::to_vec(&json!({"schemaVersion":1,"sections":values}))
            .map_err(|e| e.to_string())?,
        html: html.into_bytes(),
    })
}

pub fn build_app(
    root: &Path,
    target: Option<&str>,
    document_path: &Path,
    embedded: bool,
) -> Result {
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(root)
        .env("RC2FX_LICENSES_JSON", document_path)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "rc2fx-app",
            "--no-default-features",
        ]);
    if embedded {
        cargo.args(["--features", "embedded"]);
    }
    if let Some(target) = target {
        cargo.args(["--target", target]);
    }
    run(&mut cargo)
}

pub fn verify_embedded(executable: &[u8], document: &[u8]) -> Result {
    if document.is_empty()
        || !executable
            .windows(document.len())
            .any(|bytes| bytes == document)
    {
        return Err("the executable does not embed the current license document; rebuild with cargo xtask package".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copyright_html_keeps_unicode_entities_and_verbatim_terms() {
        assert_eq!(
            html_text("<body><pre>Copyright &#34;A&#34; &amp; B\nTerms</pre></body>").unwrap(),
            "\nCopyright \"A\" & B\nTerms\n"
        );
        assert!(html_text("<body>&unrecognized;</body>").is_err());
    }
    #[test]
    fn both_formats_preserve_notice_text_and_escape_html() {
        let sections = vec![(
            "Owner <&>".into(),
            "Copyright A\nTerms <&>\n\"quoted\"".into(),
        )];
        let doc = render(&sections).unwrap();
        let json: Value = serde_json::from_slice(&doc.json).unwrap();
        assert_eq!(json["sections"][0]["text"], sections[0].1);
        let html = String::from_utf8(doc.html).unwrap();
        assert!(html.contains("Owner &lt;&amp;&gt;"));
        assert!(html.contains("Copyright A\nTerms &lt;&amp;&gt;"));
        assert!(verify_embedded(b"old executable", &doc.json).is_err());
        assert!(verify_embedded(&doc.json, &doc.json).is_ok());
    }
    #[test]
    fn different_copyrights_are_not_lost_when_license_names_match() {
        let mut groups = BTreeMap::new();
        for (owner, user) in [("A", "one"), ("B", "two"), ("A", "three")] {
            merge_report(&mut groups, &json!({"licenses":[{"name":"MIT","text":format!("Copyright {owner}"),"used_by":[{"crate":{"name":user,"version":"1.0"}}]}]})).unwrap();
        }
        assert_eq!(groups["MIT"].len(), 2);
        assert_eq!(groups["MIT"]["Copyright A"].len(), 2);
        assert!(merge_report(&mut groups, &json!({"licenses":[]})).is_err());
    }
    #[test]
    fn the_velopack_report_matches_the_pinned_vpk_and_merges() {
        let version = super::super::velopack::VPK_VERSION;
        let path = crate::root().join(format!("licenses/velopack-{version}.json"));
        let report: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(report["velopack"], version);
        let mut groups = BTreeMap::new();
        merge_report(&mut groups, &report).unwrap();
        let users: Vec<&String> = groups.values().flat_map(|t| t.values().flatten()).collect();
        for name in ["velopack_bins ", "zstd-sys ", "webpki-roots "] {
            assert!(
                users.iter().any(|u| u.starts_with(name)),
                "{name} has no notice"
            );
        }
    }
}
