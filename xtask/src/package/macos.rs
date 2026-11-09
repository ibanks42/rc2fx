//! The macOS app: `RC2FX.app`. A bare executable opens in Terminal, so the
//! app ships as a bundle. Velopack signs, notarizes and packages it (see
//! `velopack`).

#[cfg(test)]
use super::Member;
use super::{Files, file};
use crate::Result;

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
