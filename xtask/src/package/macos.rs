//! The macOS package: `RC2FX.app`. A bare executable opens in Terminal, so the
//! app ships as a bundle. It is sealed ad hoc by default, Developer ID signed
//! when `RC2FX_MACOS_IDENTITY` names a certificate, and also notarized when
//! `RC2FX_NOTARY_KEY`, `RC2FX_NOTARY_KEY_ID` and `RC2FX_NOTARY_ISSUER` name an
//! App Store Connect API key.

use super::{Files, Member, file};
use crate::Result;
use std::{env, fs, path::Path, process::Command};

pub const APP: &str = "RC2FX.app";
const BUNDLE_ID: &str = "io.github.ibanks42.rc2fx";
const ICON: &[u8] = include_bytes!("../../assets/rc2fx.icns");

fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>RC2FX</string>
    <key>CFBundleExecutable</key><string>rc2fx</string>
    <key>CFBundleIconFile</key><string>rc2fx</string>
    <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>RC2FX</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#
    )
}

/// Move the executable into an app bundle.
pub fn bundle(mut files: Files, version: &str) -> Result<Files> {
    let executable = files
        .remove("rc2fx")
        .ok_or("package has no rc2fx executable")?;
    let contents = format!("{APP}/Contents");
    files.insert(format!("{contents}/MacOS/rc2fx"), executable);
    files.insert(
        format!("{contents}/Info.plist"),
        file(info_plist(version).into_bytes()),
    );
    files.insert(
        format!("{contents}/Resources/rc2fx.icns"),
        file(ICON.to_vec()),
    );
    Ok(files)
}

fn run(program: &str, args: &[&str]) -> Result {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed:\n{}{}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

fn setting(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// Submit the signed app to Apple and attach the ticket. Stapling fails when
/// Apple rejected the app, so `notarytool` need not report that itself.
fn notarize(app: &Path, archive: &Path, key: &str, key_id: &str, issuer: &str) -> Result {
    let (app_path, archive_path) = (app.to_string_lossy(), archive.to_string_lossy());
    run(
        "ditto",
        &["-c", "-k", "--keepParent", &app_path, &archive_path],
    )?;
    let submitted = run(
        "xcrun",
        &[
            "notarytool",
            "submit",
            &archive_path,
            "--key",
            key,
            "--key-id",
            key_id,
            "--issuer",
            issuer,
            "--wait",
        ],
    );
    let _ = fs::remove_file(archive);
    submitted?;
    run("xcrun", &["stapler", "staple", &app_path])?;
    // What a user's Mac will decide about the downloaded app.
    run("spctl", &["--assess", "--type", "execute", &app_path])
}

/// Sign the bundle in `folder`, notarize it when configured, and return the
/// folder as it now is: signing changes the executable and adds files.
pub fn seal(folder: &Path) -> Result<Files> {
    let app = folder.join(APP);
    let app_path = app.to_string_lossy();
    let notary = [
        setting("RC2FX_NOTARY_KEY"),
        setting("RC2FX_NOTARY_KEY_ID"),
        setting("RC2FX_NOTARY_ISSUER"),
    ];
    let notarizing = notary.iter().any(Option::is_some);
    match setting("RC2FX_MACOS_IDENTITY") {
        Some(identity) => run(
            "codesign",
            &[
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--sign",
                &identity,
                &app_path,
            ],
        )?,
        None if notarizing => return Err("notarization needs RC2FX_MACOS_IDENTITY".into()),
        None => run("codesign", &["--force", "--sign", "-", &app_path])?,
    }
    run("codesign", &["--verify", "--strict", "--deep", &app_path])?;
    if notarizing {
        let [Some(key), Some(key_id), Some(issuer)] = &notary else {
            return Err(
                "set all of RC2FX_NOTARY_KEY, RC2FX_NOTARY_KEY_ID and RC2FX_NOTARY_ISSUER".into(),
            );
        };
        let archive = folder.with_extension("notarize.zip");
        notarize(&app, &archive, key, key_id, issuer)?;
    }
    let mut files = Files::new();
    read_tree(folder, "", &mut files)?;
    Ok(files)
}

fn read_tree(dir: &Path, prefix: &str, files: &mut Files) -> Result {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            read_tree(&path, &format!("{name}/"), files)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o777;
            #[cfg(not(unix))]
            let mode = 0o644;
            let data = super::read(&path)?;
            files.insert(name, Member { data, mode });
        } else {
            return Err(format!("{} is not a regular file", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_moves_the_executable_into_an_app() {
        let mut files = Files::new();
        files.insert(
            "rc2fx".into(),
            Member {
                data: b"binary".to_vec(),
                mode: 0o755,
            },
        );
        let files = bundle(files, "1.2.3").unwrap();
        assert!(!files.contains_key("rc2fx"));
        let executable = &files["RC2FX.app/Contents/MacOS/rc2fx"];
        assert_eq!(
            (executable.data.as_slice(), executable.mode),
            (&b"binary"[..], 0o755)
        );
        let plist = String::from_utf8(files["RC2FX.app/Contents/Info.plist"].data.clone()).unwrap();
        assert!(plist.contains("<key>CFBundleShortVersionString</key><string>1.2.3</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key><string>rc2fx</string>"));
        assert_eq!(
            &files["RC2FX.app/Contents/Resources/rc2fx.icns"].data[..4],
            b"icns"
        );
    }

    #[test]
    fn bundle_needs_the_executable() {
        assert!(bundle(Files::new(), "1.2.3").is_err());
    }
}
