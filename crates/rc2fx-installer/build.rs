//! Embed the installer payload into `rc2fx-installer` so the desktop app can
//! install RC2FX without any runtime, catalog or script files beside it.
//!
//! The payload is checked with the same rules the library applies to a folder's
//! payload, here at build time: every file's checksum, the SHA256SUMS manifest,
//! the ARM64 ELFs, the catalog and the single `rc2fx_build=` line. With the
//! `embedded` feature a missing or corrupt payload is a build error — a
//! desktop build must never silently lose its installer. Core tools, fixtures
//! and host tests build without the feature and without a payload.

#[path = "src/embed/catalog.rs"]
mod catalog;
#[path = "src/embed/format.rs"]
mod format;

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=RC2FX_RELEASE_BUILD");
    let release = env::var("RC2FX_RELEASE_BUILD").as_deref() == Ok("1");
    let embedded = env::var_os("CARGO_FEATURE_EMBEDDED").is_some();

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let blob = out.join("rc2fx-payload.bin");

    if !embedded {
        write_module(
            &out.join("embedded.rs"),
            false,
            "",
            "this build does not embed the installer payload; build it with the `embedded` feature",
        );
        return;
    }

    let root = workspace_root();
    // Watch only what this build actually reads, and only paths that exist:
    // Cargo treats a missing watched path as changed on every build, which
    // would rerun this script and its dependents forever. Directory watches are
    // recursive, so the repository root and `target/` are never watched.
    let source = format::source_dir(&root);
    watch_existing(&source);
    for name in format::PAYLOAD_NAMES {
        if !format::SCRIPTS.contains(&name) {
            watch_existing(&source.join(name));
        }
    }
    for name in format::SCRIPTS {
        watch_existing(&root.join("xtask").join("mixer").join(name));
    }
    watch_existing(&root.join("config").join("firmware-profiles.json"));

    match format::read(&root, release) {
        Ok(files) => {
            let envelope = format::wrap(&format::body(&format::view(&files)));
            // Prove the envelope this build embeds reads back as exactly one
            // payload before compiling it in.
            match format::Envelope::find(&envelope) {
                Ok(found) => {
                    if found.as_bytes() != envelope {
                        fail("the embedded payload does not read back as written")
                    }
                }
                Err(e) => fail(&e),
            }
            if let Err(e) = fs::write(&blob, &envelope) {
                fail(&format!("could not write {}: {e}", blob.display()));
            }
            write_module(
                &out.join("embedded.rs"),
                true,
                &blob.display().to_string(),
                "",
            );
        }
        Err(e) => fail(&e),
    }
}

fn workspace_root() -> PathBuf {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|crates| crates.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate_dir.clone())
}

/// Ask Cargo to rerun when `path` changes, but only if it is there now: a
/// missing path is always "changed".
fn watch_existing(path: &std::path::Path) {
    if path.symlink_metadata().is_ok() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The generated module: whether a payload is embedded, its bytes, and why
/// there is none when there is none.
fn write_module(path: &std::path::Path, present: bool, blob: &str, missing: &str) {
    let body = if present {
        format!(
            "pub(super) const PRESENT: bool = true;\npub(super) const MISSING: &str = \"\";\npub(super) static BLOB: &[u8] = include_bytes!({blob:?});\n"
        )
    } else {
        format!(
            "pub(super) const PRESENT: bool = false;\npub(super) const MISSING: &str = {missing:?};\npub(super) static BLOB: &[u8] = &[];\n"
        )
    };
    if let Err(e) = fs::write(path, body) {
        fail(&format!("could not write {}: {e}", path.display()));
    }
}

/// A missing or damaged payload is fatal with the feature on: the desktop app
/// embeds its installer, so there is no runtime fallback to files beside it.
fn fail(why: &str) {
    panic!(
        "\n\ncannot embed the installer payload: {why}\n\n\
         The desktop app installs RC2FX from this payload, so it must be present\n\
         and valid when the app is built.\n\n\
         In a source checkout the payload is target/mixer (runtime) and\n\
         xtask/mixer (scripts), with config/firmware-profiles.json:\n\
         - `cargo xtask mixer-build` builds and stages it;\n\
         - if target/mixer is a promoted link to a verified bundle, keep it and\n\
           do not run a generic rebuild: `cargo xtask mixer install` deploys the\n\
           staged bundle, and the build refuses to replace the link.\n\
         In a release folder the same files are runtime/ and xtask/mixer.\n"
    )
}
