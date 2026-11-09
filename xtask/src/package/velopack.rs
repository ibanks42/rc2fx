//! The Windows, macOS and Linux AppImage packages, made by Velopack's `vpk`
//! (`RC2FX_VPK`, or `vpk` on the PATH): an installer (Setup.exe, Setup.pkg),
//! a portable zip or the AppImage, which can update themselves too, and the
//! update package and feed the app downloads from the latest release.
//!
//! On Linux vpk packages the finished AppDir as it is (its name ends in
//! `.AppDir`), adding only its updater and version file under usr/bin, and
//! puts its own AppImage runtime in front; it needs `mksquashfs` on the PATH.
//! Published names have no version, so `releases/latest/download/NAME` always
//! links the newest one, and an AppImage update replaces the file in place.
//!
//! Signing is vpk's. On Windows, `RC2FX_ARTIFACT_SIGNING_METADATA` names the
//! Azure Artifact Signing metadata file, and vpk signs rc2fx.exe, its stub,
//! the updater and Setup. On macOS, `RC2FX_MACOS_IDENTITY` and
//! `RC2FX_MACOS_INSTALLER_IDENTITY` name the Developer ID Application and
//! Installer certificates, and `RC2FX_NOTARY_PROFILE` a notarytool profile in
//! the keychain `RC2FX_MACOS_KEYCHAIN`; vpk signs, notarizes and staples the
//! app and the installer. Without these settings the packages are unsigned.

use super::{read, run, setting};
use crate::{Result, sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The id Velopack installs, updates and names packages by. On Windows it is
/// the install folder under %LOCALAPPDATA%, so it must never be "RC2FX":
/// that folder holds the user's data, which uninstalling would remove.
pub const PACK_ID: &str = "io.github.ibanks42.rc2fx";
/// The vpk release this packaging is written for. Changing it needs a new
/// licenses/velopack-VERSION.json, covering the Windows, macOS and Linux
/// (musl) builds of the programs vpk adds (.github/scripts/velopack-licenses.py).
pub const VPK_VERSION: &str = "1.3.39-g8d2844f";
const ENTITLEMENTS: &str = "xtask/assets/rc2fx.entitlements";

/// Where one platform's packages come from and go.
struct Target<'a> {
    os: &'a str,
    arch: &'a str,
    version: &'a str,
    /// `rc2fx-<version>-<os>-<arch>`.
    name: &'a str,
}

impl Target<'_> {
    fn channel(&self) -> &'static str {
        match self.os {
            "windows" => "win",
            "linux" => "linux",
            _ => "osx",
        }
    }

    fn runtime(&self) -> String {
        format!("{}-{}", self.channel(), self.arch)
    }

    /// The update package. Velopack leaves the channel out of the default
    /// Windows channel's name.
    fn package(&self) -> String {
        if self.os == "windows" {
            format!("{PACK_ID}-{}-full.nupkg", self.version)
        } else {
            format!("{PACK_ID}-{}-{}-full.nupkg", self.version, self.channel())
        }
    }

    fn feed(&self) -> String {
        format!("releases.{}.json", self.channel())
    }

    /// What vpk writes, by its default names, and what each is published as.
    /// The update package and feed come last.
    fn outputs(&self) -> Vec<(String, String)> {
        let channel = self.channel();
        let mut outputs = match self.os {
            "windows" => vec![
                (
                    format!("{PACK_ID}-{channel}-Setup.exe"),
                    format!("{}-setup.exe", self.name),
                ),
                (
                    format!("{PACK_ID}-{channel}-Portable.zip"),
                    format!("{}.zip", self.name),
                ),
            ],
            "linux" => vec![(
                format!("{PACK_ID}.AppImage"),
                format!("{}.AppImage", self.name),
            )],
            _ => vec![
                (
                    format!("{PACK_ID}-{channel}-Setup.pkg"),
                    format!("{}.pkg", self.name),
                ),
                (
                    format!("{PACK_ID}-{channel}-Portable.zip"),
                    format!("{}.zip", self.name),
                ),
            ],
        };
        // The feed names the package, so both keep vpk's names.
        outputs.push((self.package(), self.package()));
        outputs.push((self.feed(), self.feed()));
        outputs
    }
}

fn signing_args(os: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    // vpk signs nothing on Linux: the apt and dnf repositories are signed
    // instead, and the app checks AppImage updates against signed metadata.
    if os == "linux" {
        return Ok(args);
    }
    if os == "windows" {
        match setting("RC2FX_ARTIFACT_SIGNING_METADATA") {
            Some(metadata) => args.extend(["--azureTrustedSignFile".into(), metadata]),
            None => println!("Windows signing is not configured: the packages stay unsigned."),
        }
        return Ok(args);
    }
    let app = setting("RC2FX_MACOS_IDENTITY");
    let installer = setting("RC2FX_MACOS_INSTALLER_IDENTITY");
    let notary = [
        setting("RC2FX_NOTARY_PROFILE"),
        setting("RC2FX_MACOS_KEYCHAIN"),
    ];
    let notarizing = notary.iter().any(Option::is_some);
    match (app, installer) {
        (Some(app), Some(installer)) => {
            args.extend(["--signAppIdentity".into(), app]);
            args.extend(["--signInstallIdentity".into(), installer]);
        }
        (None, None) if !notarizing => {
            // Ad hoc, after vpk adds its updater: an unsealed bundle is
            // reported as damaged, a sealed one can be allowed in Privacy &
            // Security. Setup.pkg stays unsigned.
            println!("macOS signing is not configured: the app is signed ad hoc only.");
            args.extend(["--signAppIdentity".into(), "-".into()]);
            return Ok(args);
        }
        _ => {
            return Err(
                "set both RC2FX_MACOS_IDENTITY and RC2FX_MACOS_INSTALLER_IDENTITY, or neither"
                    .into(),
            );
        }
    }
    match notary {
        [Some(profile), Some(keychain)] => {
            args.extend(["--notaryProfile".into(), profile]);
            args.extend(["--keychain".into(), keychain]);
        }
        [None, None] => println!("Notarization is not configured: the packages are signed only."),
        _ => {
            return Err(
                "set both RC2FX_NOTARY_PROFILE and RC2FX_MACOS_KEYCHAIN, or neither".into(),
            );
        }
    }
    Ok(args)
}

fn pack_args(root: &Path, target: &Target, pack_dir: &Path, staging: &Path) -> Vec<String> {
    let path = |p: &Path| p.display().to_string();
    let mut args: Vec<String> = [
        "pack",
        "--yes",
        "--skip-updates",
        "--legacyConsole",
        "--packId",
        PACK_ID,
        "--packVersion",
        target.version,
        "--packTitle",
        "RC2FX",
        "--packAuthors",
        "Isaiah Banks",
        // Every update downloads the full package: no delta bookkeeping
        // across releases.
        "--delta",
        "None",
    ]
    .map(String::from)
    .to_vec();
    args.extend(["--runtime".into(), target.runtime()]);
    args.extend(["--packDir".into(), path(pack_dir)]);
    args.extend(["--outputDir".into(), path(staging)]);
    if target.os == "windows" {
        args.extend(["--mainExe".into(), "rc2fx.exe".into()]);
        args.extend([
            "--icon".into(),
            path(&root.join("crates/rc2fx-app/assets/rc2fx.ico")),
        ]);
        args.extend(["--shortcuts".into(), "StartMenuRoot".into()]);
    } else if target.os == "linux" {
        // Found at usr/bin/rc2fx in the AppDir. The AppDir already has its
        // desktop entry and icon, so vpk's --icon does not apply.
        // The image keeps vpk's gzip compression: its only other choice, xz,
        // makes every start slower.
        args.extend(["--mainExe".into(), "rc2fx".into()]);
    } else {
        args.extend(["--mainExe".into(), "rc2fx".into()]);
        // The app needs no hardened-runtime exceptions; vpk's default grants
        // .NET's.
        args.extend(["--signEntitlements".into(), path(&root.join(ENTITLEMENTS))]);
    }
    args
}

/// The feed must offer exactly this release's package, with its digest:
/// that digest is what the app checks the download against.
fn check_feed(feed: &[u8], version: &str, package: &str, digest: &str) -> Result {
    let feed: serde_json::Value =
        serde_json::from_slice(feed).map_err(|e| format!("update feed: {e}"))?;
    let assets = feed["Assets"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [asset] = assets else {
        return Err(format!(
            "update feed lists {} packages, not 1",
            assets.len()
        ));
    };
    let field = |name: &str| asset[name].as_str().unwrap_or_default();
    if field("PackageId") != PACK_ID
        || field("Version") != version
        || field("Type") != "Full"
        || field("FileName") != package
        || !field("SHA256").eq_ignore_ascii_case(digest)
    {
        return Err(format!(
            "update feed does not describe {package} {version} with SHA-256 {digest}"
        ));
    }
    Ok(())
}

/// Package the app in `pack_dir` (the folder holding rc2fx.exe, RC2FX.app,
/// or the AppDir) into `out`, returning the published files.
pub fn pack(
    root: &Path,
    os: &str,
    arch: &str,
    version: &str,
    name: &str,
    pack_dir: &Path,
    out: &Path,
) -> Result<Vec<PathBuf>> {
    let target = Target {
        os,
        arch,
        version,
        name,
    };
    let vpk = setting("RC2FX_VPK").unwrap_or_else(|| "vpk".into());
    let staging = out.join(format!("{name}-velopack"));
    let _ = fs::remove_dir_all(&staging);
    let mut args = pack_args(root, &target, pack_dir, &staging);
    args.extend(signing_args(os)?);
    run(&vpk, &args).map_err(|e| {
        format!(
            "{e}\nInstall vpk {VPK_VERSION}: dotnet tool install -g vpk --version {VPK_VERSION}"
        )
    })?;
    let outputs = target.outputs();
    let mut published = Vec::new();
    for (made, publish_as) in &outputs {
        let from = staging.join(made);
        let to = out.join(publish_as);
        let _ = fs::remove_file(&to);
        fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
        published.push(to);
    }
    let package = target.package();
    let digest = sha256(&read(&out.join(&package))?);
    check_feed(&read(&out.join(target.feed()))?, version, &package, &digest)?;
    let _ = fs::remove_dir_all(&staging);
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target<'a>(os: &'a str, arch: &'a str) -> Target<'a> {
        Target {
            os,
            arch,
            version: "1.2.3",
            name: "rc2fx-x",
        }
    }

    #[test]
    fn published_names_follow_the_project_and_the_feed_keeps_vpk_names() {
        let windows: Vec<String> = target("windows", "x64")
            .outputs()
            .into_iter()
            .map(|(_, to)| to)
            .collect();
        assert_eq!(
            windows,
            [
                "rc2fx-x-setup.exe",
                "rc2fx-x.zip",
                "io.github.ibanks42.rc2fx-1.2.3-full.nupkg",
                "releases.win.json",
            ]
        );
        let macos = target("macos", "arm64").outputs();
        assert_eq!(macos[0].0, "io.github.ibanks42.rc2fx-osx-Setup.pkg");
        assert_eq!(macos[0].1, "rc2fx-x.pkg");
        assert_eq!(macos[2].1, "io.github.ibanks42.rc2fx-1.2.3-osx-full.nupkg");
        assert_eq!(macos[3].1, "releases.osx.json");
        assert_eq!(target("macos", "arm64").runtime(), "osx-arm64");
        let linux = target("linux", "x64").outputs();
        assert_eq!(
            linux,
            [
                (
                    "io.github.ibanks42.rc2fx.AppImage".into(),
                    "rc2fx-x.AppImage".into()
                ),
                (
                    "io.github.ibanks42.rc2fx-1.2.3-linux-full.nupkg".into(),
                    "io.github.ibanks42.rc2fx-1.2.3-linux-full.nupkg".into()
                ),
                ("releases.linux.json".into(), "releases.linux.json".into()),
            ]
        );
        assert_eq!(target("linux", "x64").runtime(), "linux-x64");
        assert_eq!(target("windows", "x64").runtime(), "win-x64");
    }

    #[test]
    fn the_feed_must_describe_exactly_this_package() {
        let feed = |version: &str, sha: &str| {
            format!(
                r#"{{"Assets":[{{"PackageId":"{PACK_ID}","Version":"{version}","Type":"Full","FileName":"p.nupkg","SHA1":"x","SHA256":"{sha}","Size":1}}]}}"#
            )
        };
        assert!(check_feed(feed("1.2.3", "ABCD").as_bytes(), "1.2.3", "p.nupkg", "abcd").is_ok());
        assert!(check_feed(feed("1.2.4", "ABCD").as_bytes(), "1.2.3", "p.nupkg", "abcd").is_err());
        assert!(check_feed(feed("1.2.3", "ABCE").as_bytes(), "1.2.3", "p.nupkg", "abcd").is_err());
        assert!(check_feed(br#"{"Assets":[]}"#, "1.2.3", "p.nupkg", "abcd").is_err());
    }

    #[test]
    fn unsigned_macos_builds_are_still_sealed_ad_hoc() {
        // Only when no Apple setting is present in this test's environment.
        if [
            "RC2FX_MACOS_IDENTITY",
            "RC2FX_MACOS_INSTALLER_IDENTITY",
            "RC2FX_NOTARY_PROFILE",
            "RC2FX_MACOS_KEYCHAIN",
        ]
        .iter()
        .any(|name| setting(name).is_some())
        {
            return;
        }
        assert_eq!(signing_args("macos").unwrap(), ["--signAppIdentity", "-"]);
    }

    #[test]
    fn linux_packs_the_appdir_unsigned_with_no_deltas() {
        assert!(signing_args("linux").unwrap().is_empty());
        let args = pack_args(
            Path::new("/r"),
            &target("linux", "x64"),
            Path::new("/w/rc2fx.AppDir"),
            Path::new("/s"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--packDir /w/rc2fx.AppDir"));
        assert!(args.windows(2).any(|pair| pair == ["--mainExe", "rc2fx"]));
        assert!(joined.contains("--runtime linux-x64"));
        assert!(joined.contains("--delta None"));
        assert!(!joined.contains("--icon"));
        assert!(!joined.contains("--sign"));
    }

    #[test]
    fn windows_packs_an_exe_with_a_start_menu_shortcut_and_no_deltas() {
        let args = pack_args(
            Path::new("/r"),
            &target("windows", "x64"),
            Path::new("/p"),
            Path::new("/s"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--packId io.github.ibanks42.rc2fx"));
        assert!(joined.contains("--mainExe rc2fx.exe"));
        assert!(joined.contains("--shortcuts StartMenuRoot"));
        assert!(joined.contains("--delta None"));
        assert!(joined.contains("--runtime win-x64"));
        assert!(!joined.contains("--signEntitlements"));
    }
}
