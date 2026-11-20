#!/usr/bin/env bash
# Download and install Velopack's vpk at the version and SHA-256 pinned in
# config/velopack.json, using a local NuGet source only. Exports RC2FX_VPK.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
velopack_config=$here/../../config/velopack.json
json_field() {
    sed -n "s/^[[:space:]]*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$velopack_config" | head -n 1
}
version=$(json_field version)
sha=$(json_field vpkSha256)
[[ -n $version && -n $sha ]] || { echo "could not read version/vpkSha256 from $velopack_config" >&2; exit 1; }

# On Windows Git Bash, use Unix paths here and native Windows paths for dotnet/xtask.
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
