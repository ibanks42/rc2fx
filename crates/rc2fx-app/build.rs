#[path = "src/licenses/schema.rs"]
mod schema;
#[path = "src/licenses/source.rs"]
mod source;

fn main() {
    embed_licenses();
    embed_windows_resources();
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

/// Give rc2fx.exe its icon and version details. Explorer, the Start menu,
/// Installed apps and the taskbar read them; gpui-fast loads icon 1 for the
/// window. gpui-fast already embeds the application manifest.
fn embed_windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("assets").join("rc2fx.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut parts = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect::<Vec<_>>();
    parts.resize(4, 0);
    let numeric = parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\"\""));
    let script = format!(
        r#"#pragma code_page(65001)
1 ICON {icon}
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Isaiah Banks"
      VALUE "FileDescription", "RC2FX"
      VALUE "FileVersion", {version}
      VALUE "InternalName", "rc2fx"
      VALUE "LegalCopyright", "Copyright (c) 2026 Isaiah Banks. MIT License."
      VALUE "OriginalFilename", "rc2fx.exe"
      VALUE "ProductName", "RC2FX"
      VALUE "ProductVersion", {version}
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = quote(&icon.to_string_lossy()),
        version = quote(&version),
    );
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let rc = out.join("rc2fx.rc");
    std::fs::write(&rc, script).expect("write rc2fx.rc");
    let compiled = embed_resource::compile_for(&rc, ["rc2fx"], embed_resource::NONE);
    // Development machines without a resource compiler still build; releases
    // must carry the icon.
    let result = if std::env::var("RC2FX_RELEASE_BUILD").as_deref() == Ok("1") {
        compiled.manifest_required()
    } else {
        compiled.manifest_optional()
    };
    if let Err(e) = result {
        panic!("\n\ncannot embed the Windows icon and version: {e}\n\n");
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
