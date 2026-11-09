#!/usr/bin/env bash
# Load the Apple signing credentials for `cargo xtask package`, which hands
# them to vpk: the Developer ID Application certificate signs the app, the
# Developer ID Installer certificate signs Setup.pkg, and the App Store
# Connect API key, stored as a notarytool profile in the same keychain,
# notarizes both. With none configured, the packages are unsigned and the run
# says so.
set -euo pipefail

names=(MACOS_CERTIFICATE MACOS_CERTIFICATE_PASSWORD
    MACOS_INSTALLER_CERTIFICATE MACOS_INSTALLER_CERTIFICATE_PASSWORD
    MACOS_NOTARY_KEY MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER)
set_count=0
for name in "${names[@]}"; do
    if [ -n "${!name:-}" ]; then set_count=$((set_count + 1)); fi
done
if [ "$set_count" -eq 0 ]; then
    echo "::warning::No Apple signing secrets: users will have to allow this app in Privacy & Security."
    exit 0
fi
if [ "$set_count" -ne "${#names[@]}" ]; then
    echo "::error::Set all of ${names[*]}, or none."
    exit 1
fi

keychain="$RUNNER_TEMP/rc2fx-signing.keychain-db"
password="$(uuidgen)"
application="$RUNNER_TEMP/rc2fx-application.p12"
installer="$RUNNER_TEMP/rc2fx-installer.p12"
key="$RUNNER_TEMP/rc2fx-notary.p8"
trap 'rm -f "$application" "$installer" "$key"' EXIT
printf '%s' "$MACOS_CERTIFICATE" | base64 --decode > "$application"
printf '%s' "$MACOS_INSTALLER_CERTIFICATE" | base64 --decode > "$installer"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$application" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security import "$installer" -k "$keychain" -P "$MACOS_INSTALLER_CERTIFICATE_PASSWORD" -T /usr/bin/productbuild
security set-key-partition-list -S apple-tool:,apple:,codesign:,productbuild: -s -k "$password" "$keychain" > /dev/null
security list-keychains -d user -s "$keychain" login.keychain-db

# codesign and productbuild take the certificates' full names.
identity() {
    security find-identity -v -p "$1" "$keychain" | sed -n "s/.*\"\\($2: .*\\)\"/\\1/p" | head -n 1
}
app_identity="$(identity codesigning 'Developer ID Application')"
installer_identity="$(identity basic 'Developer ID Installer')"
if [ -z "$app_identity" ]; then
    echo "::error::MACOS_CERTIFICATE holds no valid Developer ID Application certificate."
    exit 1
fi
if [ -z "$installer_identity" ]; then
    echo "::error::MACOS_INSTALLER_CERTIFICATE holds no valid Developer ID Installer certificate."
    exit 1
fi

profile=rc2fx-notary
printf '%s' "$MACOS_NOTARY_KEY" > "$key"
xcrun notarytool store-credentials "$profile" --key "$key" --key-id "$MACOS_NOTARY_KEY_ID" \
    --issuer "$MACOS_NOTARY_ISSUER" --keychain "$keychain" > /dev/null
{
    echo "RC2FX_MACOS_IDENTITY=$app_identity"
    echo "RC2FX_MACOS_INSTALLER_IDENTITY=$installer_identity"
    echo "RC2FX_NOTARY_PROFILE=$profile"
    echo "RC2FX_MACOS_KEYCHAIN=$keychain"
} >> "$GITHUB_ENV"
