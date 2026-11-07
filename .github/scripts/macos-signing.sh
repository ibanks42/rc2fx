#!/usr/bin/env bash
# Load the Apple signing credentials for `cargo xtask package`. With none
# configured, the package is sealed ad hoc and the run says so.
set -euo pipefail

names=(MACOS_CERTIFICATE MACOS_CERTIFICATE_PASSWORD MACOS_NOTARY_KEY MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER)
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
certificate="$RUNNER_TEMP/rc2fx-signing.p12"
trap 'rm -f "$certificate"' EXIT
printf '%s' "$MACOS_CERTIFICATE" | base64 --decode > "$certificate"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$certificate" -k "$keychain" -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" > /dev/null
security list-keychains -d user -s "$keychain" login.keychain-db

identity="$(security find-identity -v -p codesigning "$keychain" | awk '/Developer ID Application/ { print $2; exit }')"
if [ -z "$identity" ]; then
    echo "::error::MACOS_CERTIFICATE holds no valid Developer ID Application certificate."
    exit 1
fi

key="$RUNNER_TEMP/rc2fx-notary.p8"
printf '%s' "$MACOS_NOTARY_KEY" > "$key"
{
    echo "RC2FX_MACOS_IDENTITY=$identity"
    echo "RC2FX_NOTARY_KEY=$key"
    echo "RC2FX_NOTARY_KEY_ID=$MACOS_NOTARY_KEY_ID"
    echo "RC2FX_NOTARY_ISSUER=$MACOS_NOTARY_ISSUER"
} >> "$GITHUB_ENV"
