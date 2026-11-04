//! Minimal distro packages: use system libraries instead of bundling them.
use crate::{Result, run, sha256};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn glibc_version(executable: &Path) -> Result<String> {
    let output = Command::new("readelf")
        .arg("--version-info")
        .arg(executable)
        .output()
        .map_err(|e| format!("readelf: {e}"))?;
    if !output.status.success() {
        return Err("readelf could not inspect the Linux executable".into());
    }
    required_glibc(&String::from_utf8_lossy(&output.stdout))
}

fn required_glibc(text: &str) -> Result<String> {
    text.split_whitespace()
        .filter_map(|word| word.strip_prefix("GLIBC_"))
        .filter_map(|version| {
            let parts = version
                .split('.')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            (parts.len() >= 2).then_some(parts)
        })
        .max()
        .map(|parts| {
            parts
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        })
        .ok_or_else(|| "could not determine the executable's glibc requirement".into())
}

fn config(
    stage: &Path,
    version: &str,
    arch: &str,
    glibc: &str,
    licenses: &[(String, Vec<u8>)],
) -> Value {
    let mut contents = vec![
        json!({"src":stage.join("rc2fx"),"dst":"/usr/bin/rc2fx","file_info":{"mode":493}}),
        json!({"src":stage.join("rc2fx.desktop"),"dst":"/usr/share/applications/rc2fx.desktop","file_info":{"mode":420}}),
        json!({"src":stage.join("rc2fx.svg"),"dst":"/usr/share/icons/hicolor/scalable/apps/rc2fx.svg","file_info":{"mode":420}}),
        json!({"src":stage.join("70-rc2fx.rules"),"dst":"/usr/lib/udev/rules.d/70-rc2fx.rules","file_info":{"mode":420}}),
    ];
    for (name, _) in licenses {
        contents.push(json!({"src":stage.join(name),"dst":format!("/usr/share/licenses/rc2fx/{name}"),"file_info":{"mode":420}}));
    }
    json!({
        "name":"rc2fx", "arch":arch, "platform":"linux", "version":version,
        "maintainer":"RC2FX maintainers", "description":"Audio processing for the RODECaster Pro II",
        "homepage":"https://github.com/ibanks42/rc2fx", "license":"MIT",
        "contents":contents,
        "scripts":{"postinstall":stage.join("udev-refresh"),"postremove":stage.join("udev-refresh")},
        "overrides":{
            "deb":{"depends":[format!("libc6 (>= {glibc})"),"libgcc-s1","libxcb1","libxkbcommon0","libxkbcommon-x11-0","libvulkan1","libfontconfig1","libwayland-client0","pkexec","udev"]},
            "rpm":{"depends":[format!("glibc >= {glibc}"),"libgcc","libxcb","libxkbcommon","libxkbcommon-x11","vulkan-loader","fontconfig","wayland-libs","polkit","systemd-udev"]}
        }
    })
}

pub fn package(
    out: &Path,
    name: &str,
    executable: &Path,
    licenses: &[(String, Vec<u8>)],
    arch: &str,
) -> Result<Vec<PathBuf>> {
    let tool = std::env::var_os("RC2FX_NFPM")
        .map(PathBuf::from)
        .ok_or("RC2FX_NFPM must name the pinned nfpm executable")?;
    if !tool.is_absolute() || !tool.is_file() {
        return Err("RC2FX_NFPM must be an absolute executable path".into());
    }
    let arch = match arch {
        "x64" => "amd64",
        "arm64" => "arm64",
        _ => return Err("unsupported Linux package architecture".into()),
    };
    let glibc = glibc_version(executable)?;
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let stage = temporary.path();
    fs::copy(executable, stage.join("rc2fx")).map_err(|e| e.to_string())?;
    for (name, bytes) in [
        ("rc2fx.desktop", include_bytes!("../../assets/rc2fx.desktop").as_slice()),
        ("rc2fx.svg", include_bytes!("../../assets/rc2fx.svg").as_slice()),
        ("70-rc2fx.rules", include_bytes!("../../../config/udev/70-rc2fx.rules").as_slice()),
        ("udev-refresh", b"#!/bin/sh\nif command -v udevadm >/dev/null 2>&1; then\n  udevadm control --reload-rules || true\n  udevadm trigger --action=add --subsystem-match=hidraw || true\nfi\n".as_slice()),
    ] { fs::write(stage.join(name), bytes).map_err(|e| e.to_string())?; }
    for (name, bytes) in licenses {
        fs::write(stage.join(name), bytes).map_err(|e| e.to_string())?;
    }
    let manifest = stage.join("nfpm.json");
    fs::write(
        &manifest,
        serde_json::to_vec(&config(
            stage,
            env!("CARGO_PKG_VERSION"),
            arch,
            &glibc,
            licenses,
        ))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut outputs = Vec::new();
    for format in ["deb", "rpm"] {
        let output = out.join(format!("{name}.{format}"));
        run(Command::new(&tool)
            .args(["package", "--config"])
            .arg(&manifest)
            .args(["--packager", format, "--target"])
            .arg(&output))?;
        let bytes = fs::read(&output).map_err(|e| e.to_string())?;
        let magic: &[u8] = if format == "deb" {
            b"!<arch>\n"
        } else {
            &[0xed, 0xab, 0xee, 0xdb]
        };
        if !bytes.starts_with(magic) {
            return Err(format!("invalid {format} package"));
        }
        println!("Package ready: {} (glibc >= {glibc})", output.display());
        outputs.push(output);
    }
    let mut sums = String::new();
    for extension in ["AppImage", "deb", "rpm"] {
        let path = out.join(format!("{name}.{extension}"));
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        sums.push_str(&format!("{}  {name}.{extension}\n", sha256(&data)));
    }
    fs::write(out.join("SHA256SUMS"), sums).map_err(|e| e.to_string())?;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glibc_floor_comes_from_the_binary_not_the_build_host_name() {
        assert_eq!(
            required_glibc("Name: GLIBC_2.3\n Name: GLIBC_2.39\n GLIBC_2.9 GLIBC_PRIVATE").unwrap(),
            "2.39"
        );
        assert!(required_glibc("no versions").is_err());
    }
    #[test]
    fn distro_packages_have_only_application_integration_and_legal_files() {
        let value = config(
            Path::new("/stage"),
            "0.2.0",
            "amd64",
            "2.39",
            &[("LICENSE-MIT".into(), vec![])],
        );
        assert_eq!(value["version"], "0.2.0");
        assert_eq!(value["license"], "MIT");
        let destinations: Vec<_> = value["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["dst"].as_str().unwrap())
            .collect();
        assert_eq!(destinations.len(), 5);
        assert!(destinations.contains(&"/usr/bin/rc2fx"));
        assert!(destinations.contains(&"/usr/lib/udev/rules.d/70-rc2fx.rules"));
        assert!(destinations.iter().all(|x| !x.contains("/docs/")
            && !x.contains("/crates/")
            && !x.contains("/experiments/")));
        assert_eq!(value["overrides"]["deb"]["depends"][0], "libc6 (>= 2.39)");
        assert_eq!(value["overrides"]["rpm"]["depends"][0], "glibc >= 2.39");
    }
}
