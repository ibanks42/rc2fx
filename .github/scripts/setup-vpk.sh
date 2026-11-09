#!/usr/bin/env bash
# Verified vpk, Velopack's packager, for `cargo xtask package` on Windows and
# macOS. The NuGet package is checked against a pinned SHA-256 and installed
# from a local copy only. Sets RC2FX_VPK for later steps.
set -euo pipefail

version=1.3.39-g8d2844f
sha=1a7e031f285768b6418cae43baf98d3bc295510c140a7dc5757bf99af9b78ecf

# Git Bash on Windows: Unix paths for this script, Windows paths for dotnet
# and xtask.
unix() { if command -v cygpath > /dev/null; then cygpath -u "$1"; else printf '%s\n' "$1"; fi; }
native() { if command -v cygpath > /dev/null; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }

dir=$(unix "${RUNNER_TEMP:?}")/rc2fx-vpk
rm -rf "$dir"
mkdir -p "$dir/source"
package=$dir/source/vpk.$version.nupkg
curl --fail --location --retry 3 --proto '=https' --proto-redir '=https' \
    "https://api.nuget.org/v3-flatcontainer/vpk/$version/vpk.$version.nupkg" -o "$package"
if command -v sha256sum > /dev/null; then
    printf '%s  %s\n' "$sha" "$package" | sha256sum -c -
else
    printf '%s  %s\n' "$sha" "$package" | shasum -a 256 -c -
fi

cat > "$dir/nuget.config" << EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="verified" value="$(native "$dir/source")" />
  </packageSources>
</configuration>
EOF
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
dotnet tool install vpk --version "$version" --tool-path "$(native "$dir/bin")" \
    --configfile "$(native "$dir/nuget.config")"

vpk=$dir/bin/vpk
[[ -x $vpk ]] || vpk=$dir/bin/vpk.exe
"$vpk" --help > /dev/null
if [[ -n ${GITHUB_ENV:-} ]]; then printf 'RC2FX_VPK=%s\n' "$(native "$vpk")" >> "$GITHUB_ENV"; fi
printf 'vpk %s ready: %s\n' "$version" "$vpk"
