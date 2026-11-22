#!/usr/bin/env bash
# Run a bounded on-device GUI preview and restore the stock UI (restarts GUI only, not audio).
set -euo pipefail
if [[ ${1:-} != --restart-gui || $# -gt 2 ]]; then
  echo 'Usage: preview.sh --restart-gui [5–30 seconds | --keep-running]' >&2
  echo 'This restarts the physical mixer GUI twice. It does NOT restart audio.' >&2
  exit 2
fi
seconds=${2:-15}
keep=0
if [[ "$seconds" == --keep-running ]]; then keep=1; seconds=30; fi
[[ "$seconds" =~ ^([5-9]|[12][0-9]|30)$ ]] || exit 2
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
setting() {
  local env_name=RC2FX_MIXER_${1^^} value
  value=${!env_name:-}
  if [[ -z "$value" && -f "$root/.rc2fx-mixer.conf" ]]; then
    value=$(awk -F= -v key="$1" '$1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]*$/, "");
      gsub(/^"|"$/, ""); print; exit
    }' "$root/.rc2fx-mixer.conf")
  fi
  printf '%s' "$value"
}
ip=$(setting ip); user=$(setting user); key=$(setting key); known=$(setting known_hosts); alias=$(setting host_key_alias); pre_command=$(setting pre_command)
test -n "$ip" || { echo 'Set RC2FX_MIXER_IP or .rc2fx-mixer.conf' >&2; exit 2; }
[[ "$ip" != -* && "$ip" != *[[:space:]]* ]] || exit 2
user=${user:-root}
[[ "$user" =~ ^[a-zA-Z0-9_-]+$ ]] || exit 2
options=(-T -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=yes)
if [[ -n "$key" ]]; then
  test -f "$key" || { echo 'SSH key missing; set RC2FX_MIXER_KEY to its host path' >&2; exit 2; }
  options+=(-i "$key" -o IdentitiesOnly=yes)
fi
if [[ -n "$known" ]]; then
  test -f "$known" || { echo 'Known-hosts file missing; set RC2FX_MIXER_KNOWN_HOSTS' >&2; exit 2; }
  options+=(-o "UserKnownHostsFile=$known")
fi
if [[ -n "$alias" ]]; then
  options+=(-o "HostKeyAlias=$alias")
fi
host="$user@$ip"
if [[ -n "$pre_command" ]]; then
  if [[ "$pre_command" == "~/"* ]]; then pre_command="$HOME/${pre_command#~/}"; fi
  RC2FX_MIXER_ACTION=preview \
  RC2FX_MIXER_IP="$ip" \
  RC2FX_MIXER_USER="$user" \
  RC2FX_MIXER_HOST="$host" \
  RC2FX_MIXER_KEY="$key" \
  RC2FX_MIXER_KNOWN_HOSTS="$known" \
  RC2FX_MIXER_HOST_KEY_ALIAS="$alias" \
    "$pre_command" preview || {
    echo 'preview.sh was not started: pre_command failed.' >&2
    exit 1
  }
fi
bash "$root/experiments/device-panel/build.sh"
# Verify device state before creating the RAM staging directory.
stage=$(ssh "${options[@]}" "$host" 'set -e
  test ! -e /usr/bin/first-boot
  test ! -e /usr/bin/production-boot
  test "$(tr -d "\000" < /proc/device-tree/model)" = "RODECaster Pro II"
  printf "%s  %s\n" aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca /usr/bin/rc_gui_juce | sha256sum -c - >/dev/null
  umask 077; mktemp -d /run/rc2fx-ui-upload.XXXXXX')
[[ "$stage" =~ ^/run/rc2fx-ui-upload\.[a-zA-Z0-9]+$ ]] || { echo 'Unexpected RAM staging path' >&2; exit 1; }
tar -C "$root/target/device-ui" -cf - librc2fx_ui.so rc2fx-ui-status ui.sh SHA256SUMS \
  | ssh "${options[@]}" "$host" "set -e; tar -xf - -C '$stage'; cd '$stage'; sha256sum -c SHA256SUMS"
ssh "${options[@]}" "$host" "'$stage/rc2fx-ui-status'" \
  || echo 'Touchscreen controller not ready; the panel will show offline and refuse setting changes' >&2
# RAM-only session: no persistent files installed; bounded trials exit on their
# own and kept sessions run under an independent watchdog.
if [[ "$keep" == 1 ]]; then
  pid=$(ssh "${options[@]}" "$host" \
    "setsid /bin/bash '$stage/ui.sh' ui-run --restart-gui '$stage/librc2fx_ui.so' < /dev/null > '$stage/session.log' 2>&1 & echo \$!")
  [[ "$pid" =~ ^[0-9]+$ ]] || exit 1
  printf 'RAM UI supervisor PID %s; startup log %s/session.log\n' "$pid" "$stage"
  printf 'To restore stock UI: '
  printf '%q ' ssh "${options[@]}" "$host" "/bin/bash '$stage/ui.sh' ui-stop"
  printf '\n'
else
  ssh "${options[@]}" "$host" \
    "/bin/bash '$stage/ui.sh' ui-preview --restart-gui '$stage/librc2fx_ui.so' '$seconds'"
fi
