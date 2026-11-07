//! Minimal distro packages: use system libraries instead of bundling them.
use crate::{Result, run, sha256};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Runs as root from the package's maintainer scripts, so it names udevadm
/// absolutely and hands it a clean environment.
const UDEV_REFRESH: &str = r#"#!/bin/sh
# Without a running udev (a container or chroot) a trigger reaches only the host's devices.
[ -S /run/udev/control ] || exit 0
for udevadm in /usr/bin/udevadm /sbin/udevadm /bin/udevadm; do
  if [ -x "$udevadm" ]; then
    /usr/bin/env -i "$udevadm" control --reload-rules || true
    /usr/bin/env -i "$udevadm" trigger --action=add --subsystem-match=hidraw || true
    exit 0
  fi
done
"#;

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
            "rpm":{"depends":[format!("glibc >= {glibc}"),"libgcc","libxcb","libxkbcommon","libxkbcommon-x11","vulkan-loader","fontconfig","libwayland-client.so.0()(64bit)","polkit","systemd-udev"]}
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
        (
            "rc2fx.desktop",
            include_bytes!("../../assets/rc2fx.desktop").as_slice(),
        ),
        (
            "rc2fx.svg",
            include_bytes!("../../assets/rc2fx.svg").as_slice(),
        ),
        (
            "70-rc2fx.rules",
            include_bytes!("../../../config/udev/70-rc2fx.rules").as_slice(),
        ),
        ("udev-refresh", UDEV_REFRESH.as_bytes()),
    ] {
        fs::write(stage.join(name), bytes).map_err(|e| e.to_string())?;
    }
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
    let artifacts = [
        format!("{name}.AppImage"),
        format!("{name}.deb"),
        format!("{name}.rpm"),
        format!(
            "rc2fx-{}-appimage-runtime-sources.tar.gz",
            env!("CARGO_PKG_VERSION")
        ),
    ];
    for artifact in artifacts {
        let path = out.join(&artifact);
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        sums.push_str(&format!("{}  {artifact}\n", sha256(&data)));
    }
    fs::write(out.join("SHA256SUMS"), sums).map_err(|e| e.to_string())?;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_udev_refresh_script_never_finds_udevadm_through_path() {
        assert!(!super::UDEV_REFRESH.contains("command -v"));
        for line in super::UDEV_REFRESH.lines() {
            assert!(
                !line.trim_start().starts_with("udevadm"),
                "bare udevadm: {line}"
            );
        }
        let status = Command::new("sh")
            .args(["-n", "-c", super::UDEV_REFRESH])
            .status()
            .unwrap();
        assert!(status.success());
    }

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
        let rpm_dependencies = value["overrides"]["rpm"]["depends"].as_array().unwrap();
        assert!(rpm_dependencies.contains(&json!("libwayland-client.so.0()(64bit)")));
        assert!(!rpm_dependencies.contains(&json!("wayland-libs")));
    }
}
