#[path = "src/licenses/schema.rs"]
mod schema;
#[path = "src/licenses/source.rs"]
mod source;

fn main() {
    embed_licenses();
    // GPUI's debug render/layout path exceeds Windows' default 1 MiB main
    // stack when the full EQ editor is visible. Reserve 8 MiB of virtual
    // address space; pages are committed on demand by Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let flag = match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => "/STACK:8388608",
            Ok("gnu") => "-Wl,--stack,8388608",
            _ => return,
        };
        println!("cargo:rustc-link-arg-bin=rc2fx={flag}");
    }
}

/// Check the license file packaging generated and copy it to OUT_DIR.
fn embed_licenses() {
    println!("cargo:rerun-if-env-changed=RC2FX_LICENSES_JSON");
    println!("cargo:rerun-if-env-changed=RC2FX_RELEASE_BUILD");
    let release = std::env::var("RC2FX_RELEASE_BUILD").as_deref() == Ok("1");
    let path = std::env::var_os("RC2FX_LICENSES_JSON")
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(path) = &path {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let text = source::resolve(release, path.as_deref())
        .unwrap_or_else(|e| panic!("\n\ncannot embed licenses: {e}\n\n"));
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("licenses.json"), text).expect("write licenses.json");
}
