#!/bin/bash
# Experimental RAM-only touchscreen UI runner for experiments/device-panel.
# Deliberately kept out of the production mixer bundle (mixer/scripts/rc2fx.sh).
set -euo pipefail
self=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
home=${RC2FX_HOME:-$(cd "$(dirname "$0")" && pwd)}
stock_mixer=${RC2FX_STOCK_MIXER:-/usr/bin/rc_audio_mixer}
gui_sha=aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca
mixer_settle=60

stamp() { date -u +%FT%TZ; }
say() { printf '%s %s\n' "$(stamp)" "$*"; }
start_time() { local record rest; local -a fields=(); IFS= read -r record 2>/dev/null < "/proc/$1/stat" || return 1; rest=${record##*) }; read -r -a fields <<< "$rest"; test "${#fields[@]}" -gt 19 || return 1; printf '%s\n' "${fields[19]}"; }
alive() { local record rest; IFS= read -r record 2>/dev/null < "/proc/$1/stat" || return 1; rest=${record##*) }; test "${rest%% *}" != Z; }
uptime_s() { cut -d. -f1 /proc/uptime; }
launched_mixer() {
  local pid parent exe cmd
  for pid in $(pgrep -x rc_audio_mixer 2>/dev/null || true); do
    exe=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
    test "$exe" = "$stock_mixer" || continue
    parent=$(awk '/^PPid:/ {print $2}' "/proc/$pid/status" 2>/dev/null || true)
    test -n "$parent" || continue
    cmd=$(tr '\0' '\n' < "/proc/$parent/cmdline" 2>/dev/null || true)
    if grep -Fxq /usr/bin/audio-start.sh <<< "$cmd"; then
      printf '%s\n' "$pid"
      return 0
    fi
  done
  return 1
}
mixer_age() { echo $(( $(uptime_s) - $(start_time "$1") / $(getconf CLK_TCK) )); }

arm_power_recovery() {
  test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || {
    echo 'Refusing recovery arming during firmware first boot' >&2; return 4;
  }
  local current
  current=$(fw_printenv -n reboot_pwr_button_bypass) || return 1
  case "$current" in 0|1) ;; *) echo 'Unexpected power-button bypass value' >&2; return 1 ;; esac
  if test "$current" != 1; then
    fw_setenv reboot_pwr_button_bypass 1 || return 1
  fi
  test "$(fw_printenv -n reboot_pwr_button_bypass)" = 1 || return 1
  echo 'Power recovery armed for the next boot; the bootloader consumes this flag'
}


# Isolated GUI preview. This is deliberately NOT part of audio start/install,
# boot, or the normal runtime bundle. The user must explicitly request a UI
# restart. The stock display supervisor is paused, never edited, and a separate
# session watchdog owns restoration even if this SSH/session shell disappears.
ui_retire_process() {
  local pid=$1 started=$2 n
  [[ "$pid" =~ ^[0-9]+$ && "$started" =~ ^[0-9]+$ ]] || return 0
  alive "$pid" && test "$(start_time "$pid" 2>/dev/null)" = "$started" || return 0
  kill -TERM "$pid" 2>/dev/null || true
  for ((n=0;n<30;n++)); do alive "$pid" || return 0; sleep 0.1; done
  test "$(start_time "$pid" 2>/dev/null)" = "$started" || return 0
  kill -KILL "$pid" 2>/dev/null || true
  for ((n=0;n<20;n++)); do alive "$pid" || return 0; sleep 0.1; done
  ! alive "$pid" || test "$(start_time "$pid" 2>/dev/null)" != "$started"
}

ui_preview_cleanup() {
  local dir=$1 parent started candidate actual entry fd target n stuck=0
  read -r parent started < "$dir/launcher" || return 1
  touch "$dir/done"
  # Registration happens BEFORE child exec. It remains safe to retire the
  # registered PID even in the fork/exec window, without reading its environ.
  for ((n=0;n<20;n++)); do
    test -e "$dir/child-recorded" && break
    test -e "$dir/launching" || break
    sleep 0.1
  done
  if test -e "$dir/child-recorded" && read -r candidate actual < "$dir/child"; then
    ui_retire_process "$candidate" "$actual" || stuck=1
  fi
  # Retire marked vendor helpers too: system()/popen() can inherit DRM fds.
  # Bound proc-environ reads; a stuck task must not hang restoration forever.
  for entry in /proc/[0-9]*/environ; do
    candidate=${entry#/proc/}; candidate=${candidate%/environ}
    timeout -k 1s 1s /bin/sh -c 'tr "\000" "\n" < "$1"' sh "$entry" 2>/dev/null \
      | grep -Fx "RC2FX_UI_CLOSE=$dir/close" >/dev/null || continue
    actual=$(start_time "$candidate" 2>/dev/null) || continue
    ui_retire_process "$candidate" "$actual" || stuck=1
  done
  # No other userspace DRM holder may survive when the stock launcher resumes.
  # If the original GUI refused TERM it is still the original supervisor's
  # child and is safe to leave in charge; otherwise all handles must be gone.
  local original original_start
  read -r original original_start < "$dir/original"
  for fd in /proc/[0-9]*/fd/*; do
    target=$(readlink "$fd" 2>/dev/null) || continue
    test "$target" = /dev/dri/card0 || continue
    candidate=${fd#/proc/}; candidate=${candidate%%/*}
    test "$candidate" = "$original" && test "$(start_time "$candidate" 2>/dev/null)" = "$original_start" && continue
    stuck=1
  done
  if test "$stuck" = 1; then
    say 'Preview did not release the display; launcher left paused rather than create two DRM owners. Audio was not stopped.'
    touch "$dir/recovery-failed"
    return 1
  fi
  if alive "$parent" && test "$(start_time "$parent" 2>/dev/null)" = "$started" \
    && kill -CONT "$parent" 2>/dev/null; then
    say 'Stock display launcher resumed; it will reopen the unmodified GUI'
    touch "$dir/restored"
  else
    say 'Original display launcher no longer exists; no replacement was started'
    touch "$dir/recovery-failed"
    return 1
  fi
}

ui_watch() {
  local dir=${1:?preview directory} owner started deadline
  case "$dir" in /run/rc2fx-ui.*) ;; *) return 2;; esac
  # A proc entry disappearing must not abort cleanup under errexit.
  set +e
  read -r owner started < "$dir/owner" || return 1
  # Reopen rather than share the owner's open-file-description lock. Cleanup
  # serializes with the STOP/kill/fork sequence; it cannot restore too early.
  exec 8>&-
  exec 8>"$dir/transition.lock"
  trap '' HUP
  touch "$dir/watch-ready"
  while ! test -e "$dir/done" && ! test -e "$dir/close" \
    && alive "$owner" && test "$(start_time "$owner" 2>/dev/null)" = "$started"; do
    read -r deadline < "$dir/deadline" || break
    test "$(uptime_s)" -lt "$deadline" || break
    sleep 0.1
  done
  touch "$dir/done"
  flock -x 8 || return 1
  ui_preview_cleanup "$dir"
}

# The environment the injected panel gets on top of its own (RC2FX_UI_CLOSE
# marks it and any forked helper for cleanup).
ui_injected_environment() {
  local dir=$1 library=$2
  printf '%s\0' "LD_AUDIT=$library" RC2FX_UI_PREVIEW=1 "RC2FX_UI_CLOSE=$dir/close"
}
# Whether the injected panel is verifiably active: it presented a frame.
ui_ready() {
  test -e "$1/frame-ready"
}
# Diagnostics printed at the end (never the stock GUI's own log lines).
ui_summary() {
  grep '^RC2FX UI:' "$1/gui.log" || true
}
ui_preview() {
  if test "${1:-}" != --restart-gui; then
    echo 'Requires explicit GUI-restart approval: ui-preview --restart-gui /run/.../librc2fx_ui.so [SECONDS]' >&2
    return 2
  fi
  shift
  local library=${1:?library in RAM} seconds=${2:-15} gui parent gui_start parent_start mixer state n dir guard status=0
  local gui_cwd gui_affinity gui_cpuset gui_umask gui_nice variable keep=0
  if test "$seconds" = keep; then keep=1; seconds=30; fi
  library=$(readlink -f "$library") || return 2
  case "$library" in /run/*) ;; *) echo 'Preview library must be staged under /run, not installed' >&2; return 2;; esac
  [[ "$seconds" =~ ^([5-9]|[12][0-9]|30)$ ]] \
    || { echo 'Preview duration must be 5–30 seconds' >&2; return 2; }
  test $# -le 2 && test -f "$library" && ! test -L "$library" || return 2
  test "$(id -u)" = 0 || { echo 'Preview requires root, as does the stock GUI' >&2; return 1; }
  # Developer-only, and it can hang the mixer: arm, check, then run.
  arm_power_recovery \
    || { echo 'GUI experiment not started: power recovery could not be armed. After a power cycle the mixer would stay off until its button is pressed.' >&2; return 1; }
  test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot \
    || { echo 'Refusing GUI experiment during first/production boot' >&2; return 1; }
  test "$(tr -d '\0' < /proc/device-tree/model)" = 'RODECaster Pro II' \
    || { echo 'Only the qualified Pro II is admitted' >&2; return 1; }
  printf '%s  %s\n' \
    "$gui_sha" /usr/bin/rc_gui_juce \
    36900f80c8b094bba2e67280302191ea3855e0d6a97008642c62bdea58ade600 /usr/bin/display-start.sh \
    | sha256sum -c - >/dev/null || return 1
  command -v timeout taskset renice flock setsid >/dev/null || return 1
  mixer=$(launched_mixer) && test "$(mixer_age "$mixer")" -ge "$mixer_settle" \
    || { echo 'Audio must already be settled under its stock launcher' >&2; return 1; }
  # Never stack display experiments. This descriptor is inherited only by the
  # independent watchdog, not by the injected GUI.
  umask 077
  exec 9>/run/rc2fx-ui.lock
  flock -n 9 || { echo 'Another UI preview owns the display experiment lock' >&2; return 1; }
  gui=$(pgrep -x rc_gui_juce)
  [[ "$gui" =~ ^[0-9]+$ ]] && test "$(readlink "/proc/$gui/exe")" = /usr/bin/rc_gui_juce || return 1
  gui_start=$(start_time "$gui")
  parent=$(awk '/^PPid:/ {print $2}' "/proc/$gui/status")
  tr '\0' '\n' < "/proc/$parent/cmdline" | grep -Fxq /usr/bin/display-start.sh || return 1
  test "$(cat "/proc/$parent/wchan")" = do_wait || { echo 'Display launcher is not waiting on its GUI' >&2; return 1; }
  parent_start=$(start_time "$parent")
  local -a environment
  mapfile -d '' -t environment < "/proc/$gui/environ"
  # The shipped partial-conversion module may already be in place (the GUI
  # shim injects it at every start); replacing exactly that one is the
  # experiment's job. Anything else injected is refused.
  local own_module="$home/librc2fx_gui.so"
  for variable in "${environment[@]}"; do
    case "$variable" in
      "LD_AUDIT=$own_module") ;;
      LD_AUDIT=*|LD_PRELOAD=*) echo 'Refusing to replace an already injected GUI' >&2; return 1;;
    esac
  done
  gui_cwd=$(readlink "/proc/$gui/cwd")
  gui_affinity=$(awk '/^Cpus_allowed_list:/ {print $2}' "/proc/$gui/status")
  gui_umask=$(awk '/^Umask:/ {print $2}' "/proc/$gui/status")
  gui_cpuset=$(cat "/proc/$gui/cpuset")
  gui_nice=$(ps -p "$gui" -o ni=); gui_nice=${gui_nice//[[:space:]]/}
  [[ "$gui_affinity" =~ ^[0-9,-]+$ && "$gui_umask" =~ ^[0-7]{4}$ && "$gui_nice" =~ ^-?[0-9]+$ ]] || return 1
  test -d "$gui_cwd" && test -w "/sys/fs/cgroup/cpuset$gui_cpuset/tasks" || return 1
  dir=$(mktemp -d /run/rc2fx-ui.XXXXXX)
  local -a injected
  ui_injected_environment "$dir" "$library" > "$dir/injected" || return 1
  mapfile -d '' -t injected < "$dir/injected"
  test "${#injected[@]}" -ge 3 || return 1
  printf '%s\n' "$dir" > "$(dirname "$library")/session-directory"
  printf '%s %s\n' "$$" "$(start_time "$$")" > "$dir/owner"
  printf '%s %s\n' "$parent" "$parent_start" > "$dir/launcher"
  printf '%s %s\n' "$gui" "$gui_start" > "$dir/original"
  printf '%s\n' "$(( $(uptime_s) + 30 + seconds + 12 ))" > "$dir/deadline"
  exec 8>"$dir/transition.lock"
  flock -x 8
  setsid /bin/bash "$self" ui-watch "$dir" < /dev/null > "$dir/watch.log" 2>&1 &
  guard=$!
  # From this point every ordinary exit asks the independent watchdog to
  # retire our child, then resume the exact stock supervisor process.
  trap "touch -- $(printf '%q' "$dir/done"); flock -u 8; wait $guard 2>/dev/null || true; if ! test -e $(printf '%q' "$dir/restored") && ! test -e $(printf '%q' "$dir/recovery-failed"); then ui_preview_cleanup $(printf '%q' "$dir"); fi" EXIT
  trap 'exit 130' INT TERM HUP
  for ((n=0;n<30;n++)); do test -e "$dir/watch-ready" && break; sleep 0.1; done
  test -e "$dir/watch-ready" || { echo 'Independent UI watchdog did not start' >&2; return 1; }
  say "Preview directory $dir; GUI restarts twice; audio process $mixer stays running"
  alive "$guard" && test ! -e "$dir/done" || return 1
  kill -STOP "$parent"
  # SIGSTOP is asynchronous; wait for the exact supervisor to stop.
  for ((n=0;n<10;n++)); do
    state=$(awk '/^State:/ {print $2}' "/proc/$parent/status")
    test "$state" = T && break
    sleep 0.1
  done
  test "$state" = T && test "$(start_time "$parent")" = "$parent_start" || return 1
  test "$(start_time "$gui")" = "$gui_start" || return 1
  kill -TERM "$gui"
  for ((n=0;n<30;n++)); do alive "$gui" || break; sleep 0.1; done
  # Do not force-kill the original stock UI just to make an experiment work.
  alive "$gui" && { echo 'Stock GUI did not exit; aborting preview' >&2; return 1; }
  alive "$guard" && test ! -e "$dir/done" || return 1
  touch "$dir/launching"
  (
    exec 8>&- 9>&-
    # Publish stable identity before exec; cleanup can close the fork/exec gap.
    child_pid=$BASHPID
    printf '%s %s\n' "$child_pid" "$(start_time "$child_pid")" > "$dir/child"
    touch "$dir/child-recorded"
    test ! -e "$dir/done" || exit 0
    cd "$gui_cwd"
    umask "$gui_umask"
    printf '%s\n' "$child_pid" > "/sys/fs/cgroup/cpuset$gui_cpuset/tasks"
    renice -n "$gui_nice" -p "$child_pid" >/dev/null
    exec taskset -c "$gui_affinity" env -i "${environment[@]}" "${injected[@]}" /usr/bin/rc_gui_juce
  ) < /dev/null > "$dir/gui.log" 2>&1 &
  local child=$! child_start began=$(uptime_s)
  child_start=$(start_time "$child" 2>/dev/null) || child_start=
  printf '%s\n' "$(( began + 30 + seconds + 5 ))" > "$dir/deadline.new"
  mv "$dir/deadline.new" "$dir/deadline"
  flock -u 8
  local visible_at=0 now renewed=$began
  while alive "$child" && alive "$guard" && ! test -e "$dir/close"; do
    now=$(uptime_s)
    if test "$visible_at" = 0 && ui_ready "$dir"; then
      visible_at=$now
      say 'RC2FX launcher is visible; tap RC2FX in the top bar to open the panel'
    fi
    if test "$visible_at" = 0 && test "$(( now - began ))" -ge 30; then
      say 'The injected GUI did not become ready within 30 seconds; restoring stock UI'
      break
    fi
    if test "$keep" = 1; then
      # A detached RAM session is supervised continuously. If its owner hangs
      # or dies, renewal stops and the independent watchdog restores stock.
      if test "$(( now - renewed ))" -ge 2; then
        printf '%s\n' "$(( now + 15 ))" > "$dir/deadline.new"
        mv "$dir/deadline.new" "$dir/deadline"
        renewed=$now
      fi
    elif test "$visible_at" != 0; then
      test "$(( now - visible_at ))" -lt "$seconds" || break
      if test "$(( now - renewed ))" -ge 2; then
        printf '%s\n' "$(( visible_at + seconds + 5 ))" > "$dir/deadline.new"
        mv "$dir/deadline.new" "$dir/deadline"; renewed=$now
      fi
    else
      test "$(( now - began ))" -lt 30 || break
    fi
    sleep 0.1
  done
  # Own unreaped child: bounded retirement even if the independent guard died.
  ui_retire_process "$child" "$child_start" || touch "$dir/recovery-failed"
  touch "$dir/done"
  wait "$guard" || status=$?
  trap - EXIT INT TERM HUP
  if ! test -e "$dir/restored" && ! test -e "$dir/recovery-failed"; then
    ui_preview_cleanup "$dir" || status=1
  fi
  test ! -e "$dir/recovery-failed" || { echo "Preview recovery failed; see $dir/watch.log" >&2; return 1; }
  alive "$child" && { echo 'Preview child still running; refusing an unbounded wait' >&2; return 1; }
  wait "$child" 2>/dev/null || true
  # Display diagnostics only; stock UI/audio logs can contain private data.
  ui_summary "$dir"
  test ! -e "$dir/recovery-failed" || return 1
  test -e "$dir/restored" || return 1
  say "Preview ended; diagnostics in $dir (RAM only)"
  return "$status"
}

ui_stop() {
  test ! -e /usr/bin/first-boot || { echo "Refusing UI operation during first boot" >&2; return 1; }
  local directory=${1:-} pid started
  if test -z "$directory"; then read -r directory < "$home/session-directory" || return 1; fi
  case "$directory" in /run/rc2fx-ui.*) ;; *) return 2;; esac
  read -r pid started < "$directory/owner" || return 1
  if alive "$pid" && test "$(start_time "$pid" 2>/dev/null)" = "$started"; then
    kill -TERM "$pid"
    say "UI session asked to stop; its watchdog will restore stock GUI"
  else
    say "UI session has already stopped"
  fi
}


case "${1:-}" in
  ui-stop) shift; ui_stop "$@" ;;
  ui-run) shift; test $# -eq 2 || exit 2; ui_preview "$@" keep ;;
  ui-preview) shift; ui_preview "$@" ;;
  ui-watch) shift; ui_watch "$@" ;;
  *) echo 'Usage: ui.sh ui-preview|ui-run|ui-stop|ui-watch ...' >&2; exit 2 ;;
esac
