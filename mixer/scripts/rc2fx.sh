#!/bin/bash
# Runs on the RODECaster Pro II. Has RØDE's audio launcher start the stock
# mixer with the RC2FX native library injected, from the first start of a boot,
# and hands audio back to stock when RC2FX stops.
#
#   rc2fx.sh start [--wait SECONDS]   start RC2FX (restarts the mixer once)
#   rc2fx.sh restart [--wait SECONDS] the same, even if this bundle is running
#   rc2fx.sh stop                     stop RC2FX; the launcher restarts stock audio
#   rc2fx.sh status                   show what is running
#   rc2fx.sh logs                     print the current and last-exit logs
#   rc2fx.sh arm-power-recovery       explicitly arm one-shot power-on after
#                                    outlet recovery; never part of startup
#   rc2fx.sh launch                   entry used by /usr/sbin/rc_audio_mixer
#   rc2fx.sh gui-launch               entry used by /usr/sbin/rc_gui_juce: the
#                                    stock GUI, with the partial-conversion
#                                    module when its checks pass
#   rc2fx.sh developer-ssh status|enable|disable
#                                    RC2FX's own developer SSH key; called by
#                                    rc2fxd for the desktop app over USB 1
#   rc2fx.sh update-commit ID         restart, verify and commit (or roll back)
#                                    a signed in-place update; started
#                                    detached by rc2fxd, which it outlives
#   rc2fx.sh image-check              check RØDE's files against the image the
#                                    installer flashed; started by launch
#
# developer-ssh toggles one marked line in root's authorized_keys.
# arm-power-recovery sets the bootloader's one-shot power flag (never called at
# startup; developer tools arm and verify it before running).
# image-check repairs damaged RØDE files only from a hash-matched copy on the
# other root partition.
# /usr/sbin/rc_audio_mixer shadows /usr/bin/rc_audio_mixer in audio-start.sh so
# `launch` runs as the launcher's child and execs the stock binary (injected
# unless this boot fell back to stock).
set -euo pipefail
# Rewritten to `release` by `./xtask package` when RC2FX_RELEASE_BUILD=1
# (dev builds use verbose GUI diagnostics; releases use quiet mode).
rc2fx_build=development
# A detached copy of this script (rc2fx.sh update-commit) names the bundle it
# acts on; every other caller runs the bundle it is stored in.
home=${RC2FX_HOME:-$(cd "$(dirname "$0")" && pwd)}
data=${RC2FX_DATA:-/opt/rc2fx/data}
logs=${RC2FX_LOGS:-/opt/rc2fx/logs}
run=/run/rc2fx
# Per-boot state shared by every launch: a stock request and early exits.
boot=/run/rc2fx-boot
stock_mixer=${RC2FX_STOCK_MIXER:-/usr/bin/rc_audio_mixer}
audio_shim=${RC2FX_AUDIO_SHIM:-/usr/sbin/rc_audio_mixer}
# The display: display-start.sh starts rc_gui_juce by name with /usr/sbin
# ahead of /usr/bin, so the GUI shim runs gui-launch in the loop's own
# environment. Its state and log live here, never in the audio's.
gui_home=${RC2FX_GUI_HOME:-/run/rc2fx-gui}
stock_gui=${RC2FX_STOCK_GUI:-/usr/bin/rc_gui_juce}
gui_shim=${RC2FX_GUI_SHIM:-/usr/sbin/rc_gui_juce}
gui_sha=aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca
vendor_log=/var/volatile/log/rc_audio_mixer
# Only an opted-in Duo image carries this marker. It admits experimental
# runtime behavior, never a different board or a Pro II firmware image.
# The installer writes it to the installed bundle, and the rollback copy an
# unfinished update starts (/opt/.rc2fx-previous) has none of its own, so it
# is read from that fixed place first, then beside this script.
duo_marker=${RC2FX_DUO_MARKER:-/opt/rc2fx/experimental-duo}
RC2FX_EXPERIMENTAL_DUO=0
for marker in "$duo_marker" "$home/experimental-duo"; do
  if [[ -f "$marker" && $(< "$marker") == 1 ]]; then
    RC2FX_EXPERIMENTAL_DUO=1
  fi
done
export RC2FX_EXPERIMENTAL_DUO
# Restart a mixer only once it has run this long: its USB audio gadget comes
# about 42 s into a run, and a rebuild under a build in progress is unsafe.
mixer_settle=60

stamp() { date -u +%FT%TZ; }
say() { printf '%s %s\n' "$(stamp)" "$*"; }
start_time() { local record rest; local -a fields=(); IFS= read -r record 2>/dev/null < "/proc/$1/stat" || return 1; rest=${record##*) }; read -r -a fields <<< "$rest"; test "${#fields[@]}" -gt 19 || return 1; printf '%s\n' "${fields[19]}"; }
alive() { local record rest; IFS= read -r record 2>/dev/null < "/proc/$1/stat" || return 1; rest=${record##*) }; test "${rest%% *}" != Z; }
uptime_s() { cut -d. -f1 /proc/uptime; }
supervisor() { local pid; pid=$(cat "$run/supervisor.pid" 2>/dev/null) && alive "$pid" && printf '%s\n' "$pid"; }
# The bundle the shim starts next: a build in RAM wins over the installed one.
bundle_home() { local d; for d in /run/rc2fx-dev /opt/rc2fx; do test -f "$d/rc2fx.sh" && { printf '%s\n' "$d"; return 0; }; done; return 1; }

# Keep RODE's HID descriptor intact up to its final End Collection and add
# RC2FX output report 5 / input report 6 inside that same collection. A second
# top-level collection shows up as a separate HID device on Windows and can
# break RODE's Virtual Device Driver; the USB controller also has no spare IN
# endpoint. Staged in /run only and substituted by the native hook.
#
# Lift RT throttling while RC2FX runs (restored on exit): hitting the 950 ms/s
# kernel cap previously stalled CPU 0's USB gadget IRQ thread and wedged USB 1
# until reboot. The suppression worker runs below all RØDE RT threads.
rt_throttle=/proc/sys/kernel/sched_rt_runtime_us
rt_throttle_default=950000
relax_rt_throttle() {
  test -w "$rt_throttle" || { say 'Real-time throttle: cannot write the sysctl, left as is'; return 0; }
  echo -1 > "$rt_throttle" && say "Real-time throttle off (sched_rt_runtime_us -1, was $rt_throttle_default)"
}
restore_rt_throttle() {
  test -w "$rt_throttle" && echo "$rt_throttle_default" > "$rt_throttle" 2>/dev/null || true
}

prepare_usb() {
  local script=/etc/init.d/uac2_gadget.sh descriptor=/etc/init.d/rc-hid-descriptor.bin
  if ! printf '%s  %s\n' \
    18d1860f3fd67bf19797804c77eb4f9411cf144ba1da7d71ed1ff1cf0b74bec2 "$script" \
    0aabe2ce89359fa5f6c5f45d2419e82051111c9c4f45ea5ded5068e9b9bc7647 "$descriptor" \
    | sha256sum -c - >/dev/null 2>&1; then
    say 'USB control unavailable: vendor gadget files do not match the qualified layout'
    return 0
  fi
  # The qualified descriptor is 46 bytes and ends with End Collection (c0).
  head -c 45 "$descriptor" > "$run/usb-report.bin"
  printf '\x09\x06\x85\x05\x95\xff\x91\x02\x09\x07\x85\x06\x95\xff\x81\x02\xc0' >> "$run/usb-report.bin"
  sed "s|^HID_REPORT_DESC=/etc/init.d/rc-hid-descriptor.bin |HID_REPORT_DESC=$run/usb-report.bin |" "$script" > "$run/usb-gadget.sh"
  # Without both staged scripts the native hook leaves stock USB setup alone.
  if ! grep -q "^HID_REPORT_DESC=$run/usb-report.bin " "$run/usb-gadget.sh"; then
    rm -f "$run/usb-report.bin" "$run/usb-gadget.sh"
    say 'USB control unavailable: the vendor gadget script did not take the new descriptor'
    return 0
  fi
  printf '#!/bin/bash\n/bin/bash %q usb-quiesce || exit\nexec /bin/bash /etc/init.d/uac2_gadget.sh "$@"\n' "$home/rc2fx.sh" > "$run/usb-stop.sh"
  chmod 600 "$run/usb-report.bin" "$run/usb-gadget.sh" "$run/usb-stop.sh"
}

# A vendor USB mode change also rebuilds the gadget. Retiring the name prevents
# new response handles; give an in-flight bounded write time to close before
# stock teardown. Never unbind the gadget while the controller holds it open.
usb_quiesce() {
  local native controller owner fd target busy n
  native=$(cat "$run/native.pid" 2>/dev/null || true)
  owner=$(supervisor || true)
  controller=$(test -z "$owner" || pgrep -P "$owner" -x rc2fxd || true)
  retire_usb_node "$native" "$controller" || return 1
  for ((n=0; n<40; n++)); do
    busy=0
    if test -n "$controller"; then
      for fd in /proc/"$controller"/fd/*; do
        target=$(readlink "$fd" 2>/dev/null) || continue
        case "$target" in /dev/hidg0|'/dev/hidg0 (deleted)') busy=1;; esac
      done
    fi
    test "$busy" != 0 || return 0
    sleep 0.1
  done
  say 'Refusing USB teardown: controller still holds the HID device' >&2
  return 1
}

# Vendor's HID thread opens /dev/hidg0 without O_CLOEXEC, so teardown shells
# can inherit the fd and close it after the kernel frees the old cdev. Unlink
# the devtmpfs node before a full mixer restart (waiting briefly for transient
# `sh -c` children); existing fds keep working and gadget setup recreates it.
retire_usb_node() {
  test -e /dev/hidg0 || return 0
  if test -L /dev/hidg0 || ! test -c /dev/hidg0; then
    say 'Refusing restart: /dev/hidg0 is not the expected character device' >&2
    return 1
  fi
  local fd target owner permitted pid foreign comm n
  for ((n=0; n<30; n++)); do
    foreign=
    for fd in /proc/[0-9]*/fd/*; do
      target=$(readlink "$fd" 2>/dev/null) || continue
      test "$target" = /dev/hidg0 || continue
      owner=${fd#/proc/}; owner=${owner%%/*}
      permitted=0
      for pid in "$@"; do test "$owner" != "$pid" || permitted=1; done
      test "$permitted" = 1 && continue
      comm=$(cat "/proc/$owner/comm" 2>/dev/null || true)
      foreign="$owner${comm:+ $comm}"
    done
    test -n "$foreign" || break
    sleep 0.1
  done
  if test -n "$foreign"; then
    say "Refusing restart: another process ($foreign) holds the USB HID device" >&2
    return 1
  fi
  rm -- /dev/hidg0
  # The node is gone; a log write that fails must not refuse the restart.
  say 'Retired old USB HID node before mixer restart' || true
}

# The mixer (stock or RC2FX) running under the launcher, which is waiting on
# it; prints its PID. Between the launcher's restarts there is none.
launched_mixer() {
  local found= candidate parent
  for candidate in $(pgrep -x rc_audio_mixer); do
    alive "$candidate" || continue
    test -z "$found" || return 1
    found=$candidate
  done
  test -n "$found" || return 1
  test "$(readlink "/proc/$found/exe")" = "$stock_mixer" || return 1
  parent=$(sed -n 's/^PPid:[[:space:]]*//p' "/proc/$found/status")
  # An RC2FX mixer is the child of its launch shell, itself the launcher's.
  if grep -qa 'rc2fx.sh' "/proc/$parent/cmdline" 2>/dev/null; then
    parent=$(sed -n 's/^PPid:[[:space:]]*//p' "/proc/$parent/status")
  fi
  grep -qa 'audio-start.sh' "/proc/$parent/cmdline" || return 1
  # The launcher restarts USB, JACK and the mixer as one sequence, then waits
  # on the mixer. A mixer it is not waiting on is not one it would restart.
  test "$(cat "/proc/$parent/wchan" 2>/dev/null)" = do_wait || return 1
  printf '%s\n' "$found"
}

mixer_age() { echo $(( $(uptime_s) - $(start_time "$1") / $(getconf CLK_TCK) )); }

# $2, when known, is the process's start time. A caller that is not its parent
# needs it: the PID can have gone to another process since it last looked.
alive_as() { alive "$1" && { test -z "${2:-}" || test "$(start_time "$1" 2>/dev/null)" = "$2"; }; }

# No wait after the kill: a blocked reap once left stock audio paused. Zombies
# are reaped by their parent.
stop_owned() {
  local target=$1 started=${2:-}
  alive_as "$target" "$started" || return 0
  kill -TERM "$target" 2>/dev/null || true
  for ((n=0; n<50; n++)); do alive_as "$target" "$started" || return 0; sleep 0.1; done
  alive_as "$target" "$started" || return 0
  kill -KILL "$target" 2>/dev/null || true
  for ((n=0; n<30; n++)); do alive_as "$target" "$started" || return 0; sleep 0.1; done
  say "Process $target would not exit; power-cycle the mixer if audio does not return"
  return 1
}

# The gadget is bound with RC2FX's descriptor. A restarted RC2FX mixer keeps
# it (the native hook makes the vendor's rebuild a no-op), so the host never
# sees a disconnect and the HID node must stay for it. Stock rebuilds the
# gadget, so a handback retires the node first.
gadget_kept() {
  # Not cmp -s: it trusts file sizes, and configfs reports none.
  test -c /dev/hidg0 && ! test -L /dev/hidg0 \
    && test "$(cat /sys/kernel/config/usb_gadget/g124/UDC 2>/dev/null)" = ff400000.usb \
    && cmp /sys/kernel/config/usb_gadget/g124/functions/hid.usb0/report_desc "$run/usb-report.bin" > /dev/null 2>&1
}

# Ends the mixer so that the launcher restarts it. Unless $keep_usb is 1, the
# HID node is retired first; a foreign owner refuses the restart. $mixer_start
# is the mixer's start time, for a caller that is not its parent.
keep_usb=0
mixer_start=
restart_mixer() {
  local mixer=$1; shift
  test "$keep_usb" = 1 || retire_usb_node "$mixer" "$@" || return 1
  local pid
  for pid in "$@"; do stop_owned "$pid" || true; done
  stop_owned "$mixer" "$mixer_start"
}

# Waits up to $1 s for the launcher to start a mixer other than $2 and for
# that start to register a supervisor. Exit 3: the launcher started the stock
# mixer without running the shim. Bash remembers a command's path after its
# first lookup, so a launcher already running when the shim was added keeps
# starting /usr/bin/rc_audio_mixer until the mixer is power-cycled.
wait_for_launch() {
  local deadline=$(( $(uptime_s) + $1 )) old=${2:-} pid mixer seen
  while test "$(uptime_s)" -lt "$deadline"; do
    if pid=$(supervisor) && test "$(cat "$run/native.pid" 2>/dev/null)" != "$old"; then
      echo "RC2FX starting from $(cat "$run/home" 2>/dev/null) (supervisor $pid); the desktop app connects over USB 1"
      return 0
    fi
    if mixer=$(launched_mixer) && test "$mixer" != "$old" && test "$(mixer_age "$mixer")" -ge 3 \
      && ! grep -qa 'LD_AUDIT=' "/proc/$mixer/environ"; then
      seen=$(cat "$boot/seen" 2>/dev/null || echo -1)
      if test "$seen" -ge $(( $(start_time "$mixer") / $(getconf CLK_TCK) - 1 )); then
        echo "RC2FX fell back to stock: $(tail -n 1 "$logs/rc2fx.log" 2>/dev/null)" >&2
        return 1
      fi
      echo 'The launcher started the stock mixer by the path it remembered from before the shim existed. Power-cycle the mixer; RC2FX then starts with it.' >&2
      return 3
    fi
    sleep 1
  done
  echo 'The launcher did not start RC2FX; see rc2fx.sh status' >&2
  return 1
}

# ---- JACK error recorder ----------------------------------------------------
# Replaces jackd once per RC2FX mixer start (inside the launch seam where audio
# is already restarting) with the same argv/env plus LD_PRELOAD of checksummed
# librc2fx_jackerr.so. Restores exact thread cpusets, CPU affinities, PCM fds,
# and audio IRQs; falls back to plain jackd on any mismatch. Never reruns
# audio-start.sh (its top level can flash controllers or touch boot settings).
blackbox_proc=/proc
blackbox_cpuset=/sys/fs/cgroup/cpuset
blackbox_jackd=/usr/bin/jackd
blackbox_callme=/etc/init.d/callme-t-service
blackbox_owner=0
# The audio controller's IRQs: recorded before the replacement; if a JACK
# start moved them they are put back exactly, or the server is not kept.
blackbox_irqs='24 27'
# Preloaded into jackd at every RC2FX start unless $recorder_off exists.
# Evidence stays in RAM under $recorder_dir and is never deleted;
# recorder_evidence_limit is an admission check only, not a runtime log cap.
recorder_dir=/run/rc2fx-recorder
recorder_off=$data/jack-recorder-off
recorder_evidence_limit=8388608

# One thread ($2) of server $1: role|comm|policy|priority|nice|cpuset|cpus.
# The role is "main" for the thread whose id is the pid, else "thread";
# with comm, policy and priority it is the key two servers are matched by
# (a fresh server's main thread is in /audio while its real-time and other
# threads are still in the root cpuset). Fails if anything is unreadable.
bb_thread_line() {
  local task="$blackbox_proc/$1/task/$2" role=thread record rest fields comm cpuset cpus
  test "$1" = "$2" && role=main
  test -d "$task" || return 1
  comm=$(cat "$task/comm" 2>/dev/null) && test -n "$comm" || return 1
  cpuset=$(awk -F: '$2=="cpuset" {print $3}' "$task/cgroup" 2>/dev/null) && test -n "$cpuset" || return 1
  cpus=$(awk '/^Cpus_allowed_list:/ {print $2}' "$task/status" 2>/dev/null) && test -n "$cpus" || return 1
  record=$(cat "$task/stat" 2>/dev/null) && test -n "$record" || return 1
  rest=${record##*) }; read -r -a fields <<< "$rest"
  ((${#fields[@]} > 38)) || return 1
  printf '%s|%s|%s|%s|%s|%s|%s\n' "$role" "$comm" "${fields[38]}" "${fields[37]}" "${fields[16]}" "$cpuset" "$cpus"
}
# Every thread of $1, sorted, so two servers compare by role, not thread id.
# Fails (no output) if any thread cannot be read completely.
bb_inventory() {
  local task line lines=
  for task in "$blackbox_proc/$1"/task/*; do
    line=$(bb_thread_line "$1" "${task##*/}") || return 1
    lines+="$line"$'\n'
  done
  test -n "$lines" || return 1
  sort <<< "${lines%$'\n'}"
}
# The role keys alone (role|comm|policy|priority|nice), with repeats.
bb_roles() { cut -d'|' -f1-5 <<< "$1" | sort; }
# Role keys with more than one placement: they cannot be matched.
bb_ambiguous() { cut -d'|' -f1-4,6-7 <<< "$1" | sort -u | cut -d'|' -f1-4 | uniq -d; }
bb_tasks() { local t n=0; for t in "$blackbox_proc/$1"/task/*; do test -e "$t" && n=$((n + 1)); done; echo "$n"; }
# The PCM devices $1 has open, sorted.
bb_pcms() {
  local fd target
  for fd in "$blackbox_proc/$1"/fd/*; do
    target=$(readlink "$fd" 2>/dev/null) || continue
    case "$target" in */snd/pcmC*) printf '%s\n' "$target";; esac
  done | sort -u
}
# Processes other than $1 (jackd) and callme-t that map libjack.
bb_foreign_clients() {
  local maps pid comm
  for maps in "$blackbox_proc"/[0-9]*/maps; do
    pid=${maps%/maps}; pid=${pid##*/}
    test "$pid" = "$1" && continue
    grep -q 'libjack\.so' "$maps" 2>/dev/null || continue
    comm=$(cat "$blackbox_proc/$pid/comm" 2>/dev/null) || continue
    test "$comm" = callme-t || printf '%s(%s)\n' "$pid" "$comm"
  done
}
# The IRQ threads' placement (for the log and the before/after check).
bb_irq_placement() {
  local irq
  for irq in $blackbox_irqs; do
    printf 'irq %s %s\n' "$irq" "$(cat "$blackbox_proc/irq/$irq/smp_affinity_list" 2>/dev/null || echo ?)"
  done
}
# Where the kernel actually delivers them (metadata only: read, never set).
bb_irq_effective() {
  local irq
  for irq in $blackbox_irqs; do
    printf 'effective %s %s\n' "$irq" "$(cat "$blackbox_proc/irq/$irq/effective_affinity_list" 2>/dev/null || echo ?)"
  done
}
# Put the audio IRQs back exactly as recorded in $1 (lines "irq N CPUS");
# fails unless they then read back the same.
bb_irq_restore() {
  local tag irq cpus
  while read -r tag irq cpus; do
    test "$tag" = irq || continue
    test "$(cat "$blackbox_proc/irq/$irq/smp_affinity_list" 2>/dev/null)" = "$cpus" && continue
    printf '%s\n' "$cpus" > "$blackbox_proc/irq/$irq/smp_affinity_list" 2>/dev/null || true
  done <<< "$1"
  test "$(bb_irq_placement)" = "$(grep '^irq ' <<< "$1")"
}
# Seams for moving one thread; tests replace them.
bb_move_task() { printf '%s\n' "$1" > "$blackbox_cpuset$2/tasks"; }
bb_set_cpus() { taskset -p -c "$2" "$1" > /dev/null; }
# Stop $1 if it is still the process that started at $2: TERM, then KILL.
bb_stop() {
  local pid=$1 started=$2 n
  test "$(start_time "$pid")" = "$started" || return 1
  kill -TERM "$pid" 2>/dev/null || return 1
  for ((n=0; n<50; n++)); do alive "$pid" || return 0; sleep .1; done
  test "$(start_time "$pid" 2>/dev/null)" = "$started" && kill -KILL "$pid" 2>/dev/null
  for ((n=0; n<20; n++)); do alive "$pid" || return 0; sleep .1; done
  return 1
}
# Start jackd as restartJack does: same argv/environment, from the root
# cpuset with its CPUs, ordinary policy; $1 is the log, $2 a file for its
# birth token ("pid start-time", taken at once), the rest extra environment
# (the preload). Prints the token; every later check and stop uses it, so a
# server that died is never confused with whatever reuses its pid.
bb_start_server() {
  local log=$1 token=$2; shift 2
  local root_cpus pid started n
  root_cpus=$(cat "$blackbox_cpuset/cpuset.cpus" 2>/dev/null) || return 1
  (
    printf '%s\n' "$BASHPID" > "$blackbox_cpuset/tasks" || exit 1
    exec taskset -c "$root_cpus" chrt -o 0 env -i "${bb_env[@]}" "$@" "$blackbox_jackd" "${bb_argv[@]:1}"
  ) >> "$log" 2>&1 < /dev/null &
  pid=$!
  for ((n=0; n<20; n++)); do started=$(start_time "$pid" 2>/dev/null) && break; sleep .05; done
  test -n "${started:-}" || return 1
  printf '%s %s\n' "$pid" "$started" > "$token"
  printf '%s %s\n' "$pid" "$started"
}
# Whether the server with birth token "$1 $2" is still that process.
bb_alive_as() { alive "$1" && test "$(start_time "$1" 2>/dev/null)" = "$2"; }
# Wait (bounded) until $1 has as many threads and the same PCM devices as the
# old server, then give each thread its role's cpuset and CPUs and require
# the old inventory exactly.
bb_ready_and_placed() {
  local pid=$1 started=$2 n tid line key want roles
  roles=$(bb_roles "$bb_inventory_old")
  # Ready: as many threads with the same roles (policies come from -P and
  # are compared, never set), and the same PCM devices open.
  for ((n=0; n<50; n++)); do
    bb_alive_as "$pid" "$started" || return 1
    if test "$(bb_tasks "$pid")" = "$bb_task_count" && test "$(bb_pcms "$pid")" = "$bb_pcm_list" \
       && test "$(bb_roles "$(bb_inventory "$pid")")" = "$roles"; then break; fi
    sleep .1
  done
  test "$(bb_tasks "$pid")" = "$bb_task_count" && test "$(bb_pcms "$pid")" = "$bb_pcm_list" || return 1
  test "$(bb_roles "$(bb_inventory "$pid")")" = "$roles" || {
    say "Diagnostic JACK roles differ: $(bb_inventory "$pid" | tr '\n' ';') vs $(tr '\n' ';' <<< "$bb_inventory_old")" >> "$logs/rc2fx.log"
    return 1
  }
  for tid in "$blackbox_proc/$pid"/task/*; do
    tid=${tid##*/}
    line=$(bb_thread_line "$pid" "$tid") || return 1
    key=$(cut -d'|' -f1-4 <<< "$line")
    # The role's placement (unique: ambiguous roles were refused).
    want=$(awk -F'|' -v k="$key" '$1"|"$2"|"$3"|"$4==k {print $6"|"$7}' <<< "$bb_inventory_old" | sort -u)
    test -n "$want" && test "$(wc -l <<< "$want")" = 1 || return 1
    bb_move_task "$tid" "${want%%|*}" || return 1
    bb_set_cpus "$tid" "${want#*|}" || return 1
  done
  for ((n=0; n<20; n++)); do
    bb_alive_as "$pid" "$started" || return 1
    test "$(bb_inventory "$pid")" = "$bb_inventory_old" && return 0
    sleep .1
  done
  say "Diagnostic JACK placement differs: $(bb_inventory "$pid" | tr '\n' ';') vs $(tr '\n' ';' <<< "$bb_inventory_old")" >> "$logs/rc2fx.log"
  return 1
}
# Whether every thread of inventory $1 is what a stock-style start gives back:
# the nice value this shell passes on, and SCHED_OTHER 0 or the -P priority
# (FIFO or RR) of argv. Otherwise neither the logger nor the fallback could
# restore it, so nothing is stopped.
bb_own_nice() { nice; }
bb_stock_policies() {
  local own priority= line policy prio niceness i
  own=$(bb_own_nice) || return 1
  # Server options only: after -d come the driver's (alsa's -P is a device).
  for ((i=1; i<${#bb_argv[@]}; i++)); do
    case "${bb_argv[i]}" in
      -d|-d?*|--driver|--driver=*) break;;
      -P|--realtime-priority) priority=${bb_argv[i+1]:-};;
      --realtime-priority=*) priority=${bb_argv[i]#*=};;
      -P?*) priority=${bb_argv[i]#-P};;
    esac
  done
  [[ $priority =~ ^[0-9]+$ ]] || return 1
  while IFS='|' read -r _ _ policy prio niceness _; do
    test "$niceness" = "$own" || return 1
    case "$policy:$prio" in 0:0|1:"$priority"|2:"$priority") ;; *) return 1;; esac
  done <<< "$1"
}
# bb_dir and bb_label are the replacing caller's (bb_replace_jack).
bb_fail() {
  touch "$bb_dir/failed"
  say "$bb_label failed: $*" >> "$logs/rc2fx.log"
}
# A plain server the same way; loud if even that fails (the vendor loop's
# restartJack then restarts stock jackd once the mixer exits).
bb_fallback() {
  local plain
  plain=$(bb_start_server "$bb_dir/fallback-jackd.log" "$bb_dir/fallback-jackd.pid") || plain=none
  # shellcheck disable=SC2086 # the token is "pid start-time"
  if test "$plain" != none && bb_ready_and_placed $plain && bb_irq_restore "$bb_irq_old"; then
    say "Plain jackd restored (pid ${plain% *})" >> "$logs/rc2fx.log"
  else
    say "PLAIN JACKD DID NOT COME UP CORRECTLY (pid ${plain%% *}); the vendor loop restarts JACK when the mixer exits" >> "$logs/rc2fx.log"
  fi
  "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
}

# Replaces jackd $5 with one preloading library $2, its rings and logs in
# directory $1, readiness checked with snapshot tool $3; $4 names it in the
# log. The caller has checked the library, the evidence already kept, and
# that $5 is the stock jackd without it.
bb_replace_jack() {
  local bb_dir=$1 lib=$2 snapshot=$3 bb_label=$4 pid=$5
  local dir=$1 value replacement ring_dir started foreign
  local -a bb_argv bb_env
  mapfile -d '' -t bb_argv < "$blackbox_proc/$pid/cmdline" || return 1
  mapfile -d '' -t bb_env < "$blackbox_proc/$pid/environ" || return 1
  for value in "${bb_env[@]}"; do
    case "$value" in LD_PRELOAD=?*|LD_AUDIT=?*) return 1;; esac
  done
  ((${#bb_argv[@]} > 1)) || return 1
  foreign=$(bb_foreign_clients "$pid")
  if test -n "$foreign"; then
    say "$bb_label skipped: other JACK clients $(tr '\n' ' ' <<< "$foreign")" >> "$logs/rc2fx.log"
    return 1
  fi
  started=$(start_time "$pid") && test -n "$started" || return 1
  local bb_inventory_old bb_task_count bb_pcm_list bb_irq_old ambiguous
  # Checked before anything stops: a server this start cannot reproduce, or
  # IRQs that cannot be read, are left alone.
  # Kept before any decision, so a refusal leaves its evidence.
  bb_inventory_old=$(bb_inventory "$pid") || bb_inventory_old=
  { printf 'pid %s\n' "$pid"; printf '%s\n' "${bb_inventory_old:-unreadable}"; bb_irq_placement; bb_irq_effective; } > "$dir/preflight-jack.inventory"
  test -n "$bb_inventory_old" && bb_stock_policies "$bb_inventory_old" || {
    say "$bb_label skipped: jackd's threads are not as a stock start leaves them ($(tr '\n' ';' <<< "$bb_inventory_old"))" >> "$logs/rc2fx.log"
    return 1
  }
  bb_irq_old=$(bb_irq_placement)
  if grep -q ' ?$' <<< "$bb_irq_old"; then
    say "$bb_label skipped: unreadable audio IRQ placement ($(tr '\n' ';' <<< "$bb_irq_old"))" >> "$logs/rc2fx.log"
    return 1
  fi
  # The inventory without callme connected: what the new server is compared
  # with before callme starts again. A failed stop may have stopped part of
  # the service, so it is started again either way.
  if ! "$blackbox_callme" stop >> "$logs/rc2fx.log" 2>&1; then
    say "$bb_label skipped: callme-t did not stop cleanly" >> "$logs/rc2fx.log"
    "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
    return 1
  fi
  bb_inventory_old=$(bb_inventory "$pid") || bb_inventory_old=
  bb_task_count=$(bb_tasks "$pid")
  bb_pcm_list=$(bb_pcms "$pid")
  { printf 'pid %s\n' "$pid"; printf '%s\n' "${bb_inventory_old:-unreadable}"; printf '%s\n' "$bb_pcm_list"; bb_irq_placement; bb_irq_effective; } > "$dir/original-jack.inventory"
  ambiguous=$(test -z "$bb_inventory_old" || bb_ambiguous "$bb_inventory_old")
  local why=
  if test -z "$bb_inventory_old"; then why="an unreadable thread"
  elif test -z "$bb_pcm_list"; then why="no PCM devices open"
  elif test -n "$ambiguous"; then why="roles with two placements: $(tr '\n' ' ' <<< "$ambiguous")"
  elif ! bb_stock_policies "$bb_inventory_old"; then why="policies not as a stock start leaves them"
  elif test "$(bb_irq_placement)" != "$bb_irq_old"; then why="the audio IRQs moved"
  fi
  if test -n "$why"; then
    say "$bb_label skipped: $why ($(tr '\n' ';' <<< "$bb_inventory_old"))" >> "$logs/rc2fx.log"
    "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
    return 1
  fi
  if ! ring_dir=$(mktemp -d "$dir/jackerr.XXXXXX"); then
    say "$bb_label skipped: no ring directory" >> "$logs/rc2fx.log"
    "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
    return 1
  fi
  printf '%s\0' "${bb_argv[@]}" > "$dir/original-jack.argv"
  say "$bb_label: replacing jackd $pid: $(tr '\n' ';' <<< "$bb_inventory_old") pcms $(tr '\n' ' ' <<< "$bb_pcm_list")" >> "$logs/rc2fx.log"
  if ! bb_stop "$pid" "$started"; then
    bb_fail "the old jackd ($pid) did not stop"
    "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
    return 1
  fi
  # The birth token is saved by bb_start_server the moment the server exists.
  replacement=$(bb_start_server "$dir/jackd.log" "$dir/jackd.pid" LD_PRELOAD="$lib" RC2FX_JACKERR_DIR="$ring_dir") || replacement=
  local new_pid=${replacement% *} new_started=${replacement#* }
  if test -z "$replacement" || ! bb_ready_and_placed "$new_pid" "$new_started" \
     || ! grep -Fq "$lib" "$blackbox_proc/$new_pid/maps" \
     || ! "$snapshot" "$ring_dir" "$dir/ready.snapshot" >> "$logs/rc2fx.log" 2>&1 \
     || ! bb_irq_restore "$bb_irq_old"; then
    bb_fail "the logging jackd (${new_pid:-none}) was not ready, placed, recording or with the audio IRQs as before ($(bb_irq_placement | tr '\n' ';'))"
    # Only the process with this birth token; a pid reused since is not ours.
    if test -n "$replacement" && bb_alive_as "$new_pid" "$new_started"; then
      bb_stop "$new_pid" "$new_started" || true
    fi
    bb_fallback
    { bb_irq_placement; bb_irq_effective; } > "$dir/after-jack.irqs"
    return 1
  fi
  { bb_irq_placement; bb_irq_effective; } > "$dir/after-jack.irqs"
  printf '%s\n' "$ring_dir" > "$dir/jackerr-dir"
  "$blackbox_callme" start >> "$logs/rc2fx.log" 2>&1 &
  say "$bb_label running (pid $new_pid, ring $ring_dir)" >> "$logs/rc2fx.log"
  bb_alive_as "$new_pid" "$new_started"
}
# Whether jackd $1 is the server started from attempt directory $3 with
# library $2 and seen ready there: same birth token (pid and start time, so a
# reused pid is not mistaken for it), a ring directory of that attempt, the
# ready snapshot, and the library mapped. A library merely mapped is not that.
recorder_verified() {
  local pid=$1 lib=$2 attempt=$3 token ring
  test -d "$attempt" && test ! -L "$attempt" || return 1
  grep -Fq "$lib" "$blackbox_proc/$pid/maps" || return 1
  read -r token < "$attempt/jackd.pid" 2>/dev/null || return 1
  test "$token" = "$pid $(start_time "$pid" 2>/dev/null)" || return 1
  read -r ring < "$attempt/jackerr-dir" 2>/dev/null || return 1
  test "${ring%/*}" = "$attempt" && test -d "$ring" && test ! -L "$ring" || return 1
  test -f "$attempt/ready.snapshot"
}
# The recorder's state for `rc2fx.sh status` and the log: $recorder_dir/status
# holds one line, "<state> <detail>"; it is control state, rewritten each start.
recorder_note() {
  local state=$1; shift
  say "JACK recorder $state: $*" >> "$logs/rc2fx.log"
  test -d "$recorder_dir" && test ! -L "$recorder_dir" || return 0
  printf '%s %s\n' "$state" "$*" > "$recorder_dir/status.new" && mv -f "$recorder_dir/status.new" "$recorder_dir/status"
}
# The persistent recorder (see recorder_dir). Each replacement attempt gets a
# new private directory $recorder_dir/run.XXXXXX holding all its evidence
# (inventories, server logs, birth tokens, rings, snapshots); nothing is ever
# overwritten or removed. The root holds only control state: `status`,
# `active` ("<attempt> <pid> <start time>" of the verified recording server)
# and the boot's `failed` latch. Succeeds only while jackd is the recording
# server it verified; every other outcome leaves audio as it is (or, after a
# failed replacement, plain jackd).
jack_recorder() {
  local lib=$home/librc2fx_jackerr.so snapshot=$home/rc2fx_jackerr_snapshot root=$recorder_dir
  local name listed=0 pid attempt bytes active_attempt active_pid active_started started
  if test -L "$root" || { test -e "$root" && test ! -d "$root"; }; then
    say "JACK recorder not started: $root is not a directory; plain JACK" >> "$logs/rc2fx.log"
    return 1
  fi
  test -d "$root" || mkdir -m 700 "$root" || return 1
  if test "$(stat -c %u:%a "$root")" != "$blackbox_owner:700"; then
    say "JACK recorder not started: $root is not private; plain JACK" >> "$logs/rc2fx.log"
    return 1
  fi
  if test -e "$recorder_off"; then
    recorder_note off "$recorder_off exists; plain JACK"
    return 1
  fi
  for name in librc2fx_jackerr.so rc2fx_jackerr_snapshot; do
    grep -Eq "^[0-9a-f]{64}  $name\$" "$home/SHA256SUMS" && listed=$((listed + 1))
  done
  # A bundle built before the recorder lists neither: RC2FX runs as before.
  if test "$listed" = 0; then
    recorder_note not-bundled "this bundle has no recorder; plain JACK"
    return 1
  fi
  # Only the bundle's own checksummed files are ever put into jackd.
  if test "$listed" != 2 || test -L "$lib" || test -L "$snapshot" || test ! -f "$lib" \
     || test ! -f "$snapshot" || test ! -x "$snapshot" \
     || ! (cd "$home" && grep -E '  (librc2fx_jackerr\.so|rc2fx_jackerr_snapshot)$' SHA256SUMS \
           | sha256sum -c --quiet > /dev/null 2>&1); then
    recorder_note corrupt "its bundle files are missing, unlisted or fail their checksums; plain JACK"
    return 1
  fi
  # One attempt per boot: a failed replacement is not repeated at every start.
  if test -e "$root/failed"; then
    recorder_note failed "a replacement failed earlier this boot ($(cat "$root/failed" 2>/dev/null)); plain JACK"
    return 1
  fi
  if ! pid=$(pgrep -x jackd) || ! [[ $pid =~ ^[0-9]+$ ]] \
     || test "$(readlink "$blackbox_proc/$pid/exe")" != "$blackbox_jackd"; then
    recorder_note refused "no single stock jackd to replace"
    return 1
  fi
  # A server that already preloads a JACK error logger is never preloaded
  # again or restarted for this; mapped alone is only "present".
  if grep -q 'librc2fx_jackerr\.so' "$blackbox_proc/$pid/maps"; then
    active_attempt='' active_pid='' active_started=''
    read -r active_attempt active_pid active_started < "$root/active" 2>/dev/null || true
    started=$(start_time "$pid" 2>/dev/null) || started=
    if test "${active_attempt%/*}" = "$root" && test "$active_pid $active_started" = "$pid $started" \
       && recorder_verified "$pid" "$lib" "$active_attempt"; then
      recorder_note active "pid $pid, attempt $active_attempt, ring $(cat "$active_attempt/jackerr-dir")"
      return 0
    else
      recorder_note present-unverified "jackd ($pid) maps a JACK error logger that cannot be verified; left as it is"
    fi
    return 1
  fi
  # Admission only (see recorder_evidence_limit): every byte under the root
  # counts; nothing is deleted, and a running server is never limited.
  bytes=$(du -sb "$root" 2>/dev/null | cut -f1) || bytes=
  if ! [[ $bytes =~ ^[0-9]+$ ]] || ((bytes > recorder_evidence_limit)); then
    recorder_note quota "${bytes:-unreadable} bytes of evidence in $root (admission limit $recorder_evidence_limit); not started, nothing deleted"
    return 1
  fi
  if ! attempt=$(mktemp -d "$root/run.XXXXXX"); then
    recorder_note refused "no attempt directory"
    return 1
  fi
  if bb_replace_jack "$attempt" "$lib" "$snapshot" 'JACK recorder' "$pid"; then
    read -r active_pid active_started < "$attempt/jackd.pid"
    printf '%s %s %s\n' "$attempt" "$active_pid" "$active_started" > "$root/active.new" \
      && mv -f "$root/active.new" "$root/active"
    recorder_note active "pid $active_pid, attempt $attempt, ring $(cat "$attempt/jackerr-dir")"
    return 0
  fi
  if test -e "$attempt/failed"; then
    printf '%s\n' "$attempt" > "$root/failed"
    recorder_note failed "replacement failed in $attempt; plain jackd (see the log)"
  else
    recorder_note refused "jackd left as it was; see $attempt and the log"
  fi
  return 1
}
# For `rc2fx.sh status`: the state the last RC2FX start recorded.
recorder_status_line() {
  if test -f "$recorder_dir/status" && test ! -L "$recorder_dir/status"; then
    echo "JACK recorder: $(head -n 1 "$recorder_dir/status")"
  elif test -e "$recorder_off"; then
    echo 'JACK recorder: off'
  fi
  return 0
}
# ---- end of the JACK error recorder -----------------------------------------

# What the supervisor starts rc2fxd with, into the caller's `controller` array.
# GET api/v1/version reports the installed release manifest and the profile
# launch() selected after check-device verified the stock mixer; a bundle
# without the manifest reports its version as unknown. Developer SSH over USB 1
# is handled by this script's developer-ssh.
controller_arguments() {
  controller=(--data "$data" --native-socket "$run/control.sock" --usb-socket "$run/usb.sock"
    --developer-ssh "$home/rc2fx.sh")
  if test -n "${RC2FX_UI_SOCKET:-}"; then controller+=(--ui-socket "$run/ui.sock"); fi
  if test -r "$show"; then controller+=(--stock-config "$show"); fi
  if test -f "$home/mixer-release.json"; then controller+=(--release "$home/mixer-release.json"); fi
  if test -n "$profile_id"; then
    controller+=(--firmware-catalog "$home/firmware-profiles.json" --firmware-profile "$profile_id")
  fi
}

# The supervisor catches its own panics and failures and exits 0 once it has
# dealt with the mixer, even when it had to leave it running. Any other end is a
# crash or a kill, with the mixer still injected and nothing watching it: go
# back to stock from here, as the supervisor would have. $2 is its exit status.
supervisor_lost() {
  local mixer=$1 reason="Supervisor died with status $2; restoring stock audio" controllers
  say "$reason" >> "$logs/rc2fx.log"
  printf '%s\n' "$reason" > "$boot/stock"
  # It left its controller behind, on the HID node.
  controllers=$(pgrep -x rc2fxd || true)
  # shellcheck disable=SC2086
  restart_mixer "$mixer" $controllers >> "$logs/rc2fx.log" 2>&1 \
    || say 'Leaving the mixer running without RC2FX control; rc2fx.sh start recovers it' >> "$logs/rc2fx.log"
  (supervise_exit) >> "$logs/rc2fx.log" 2>&1
}

# Runs as the launcher's child in place of the stock executable, so it must
# end in an exec of it: on any failure the stock mixer runs unchanged.
launch() {
  local fallback="exec $stock_mixer"
  if test $# -gt 0; then fallback+=$(printf ' %q' "$@"); fi
  trap "$fallback" EXIT
  mkdir -p -m 700 "$boot" "$data" "$logs"
  # Records that the shim ran for this start, whatever it decides.
  uptime_s > "$boot/seen"
  # Checks the flashed image's RØDE files in the background (image check).
  image_check_start || true
  # Wraps RØDE's update script so a flash is checked before RØDE reboots.
  flash_watch_start || true
  # profile_id and show are read by controller_arguments.
  local why= selected n profile_id show
  if test -e /opt/rc2fx/disabled; then why='autostart is disabled'
  elif test -e /usr/bin/first-boot; then why='first boot after a firmware update'
  elif test -e "$boot/stock"; then why="$(cat "$boot/stock")"
  elif ! (cd "$home" && sha256sum -c SHA256SUMS > /dev/null 2>&1); then why="binaries in $home fail their checksums"
  elif ! selected=$("$home/rc2fx-firmware" check-device "$home/firmware-profiles.json" "$stock_mixer" /usr/bin/audio-start.sh 2>> "$logs/rc2fx.log"); then why='this firmware is not a supported profile'
  # Nothing would watch the mixer without it, so prove it runs before injecting.
  elif ! "$home/rc2fx-supervisor" --version > /dev/null 2>> "$logs/rc2fx.log"; then why='the supervisor cannot run'
  fi
  if test -n "$why"; then
    # Stock rebuilds the gadget, and a mixer that finds a live HID node before
    # it has started crashes on early host reports; give it the boot picture.
    retire_usb_node >> "$logs/rc2fx.log" 2>&1 || true
    say "Stock mixer started: $why" >> "$logs/rc2fx.log"
    exec "$stock_mixer" "$@"
  fi
  read -r profile_id show <<< "$selected"
  # The supervisor of the mixer that just exited may still be writing its
  # last-exit log.
  for ((n=0; n<50; n++)); do supervisor > /dev/null || break; sleep 0.1; done
  rm -rf "$run"
  mkdir -p -m 700 "$run"
  test -f "$logs/rc2fx.log" && mv -f "$logs/rc2fx.log" "$logs/rc2fx.prev.log"
  test -f "$logs/audio-events.log" && mv -f "$logs/audio-events.log" "$logs/audio-events.prev.log"
  # The mixer keeps the launcher's stdout (its own log), so nothing here
  # redirects the shell's; each step logs on its own.
  prepare_usb >> "$logs/rc2fx.log" 2>&1
  # Bind every mixer path from the saved settings (defaults if none exist yet).
  if ! "$home/rc2fxd" plan --data "$data" --out "$run/plan.json" >> "$logs/rc2fx.log" 2>&1; then
    say 'Stock mixer started: the launch plan failed' >> "$logs/rc2fx.log"
    exec "$stock_mixer" "$@"
  fi
  printf '%s\n' "$home" > "$run/home"
  cp "$home/SHA256SUMS" "$run/bundle.sums"
  # The supervisor's own copy: an install removes a RAM bundle, or replaces this
  # one, while the supervisor it must then stop is still running.
  cp "$home/rc2fx.sh" "$run/rc2fx.sh"
  # The mixer runs as this shell's child so that its exit status or signal
  # gets logged: the launcher records neither. No taskset: stock starts
  # unpinned and pins its own threads. RC2FX's diagnostics go to stderr.
  trap - EXIT
  # From here on nothing falls back to stock, so no failed step may end this
  # shell: a log write that fails (a full root) would leave no mixer at all.
  set +e
  relax_rt_throttle >> "$logs/rc2fx.log"
  jack_recorder || true
  # Explicit RAM-only measurement switch. Never inherit a caller's probe flag;
  # removal takes effect at the next mixer start, reboot clears the marker.
  local client_timing=0 gauges=0
  local -a controller=()
  controller_arguments
  test -f /run/rc2fx-client-timing && client_timing=1
  test -f /run/rc2fx-gauges && gauges=1
  # fast unless a bench chose verify or stock; see crates/rc2fx-native/src/src_fast.rs.
  local src_mode=fast
  test -f /run/rc2fx-src-mode && src_mode=$(cat /run/rc2fx-src-mode)
  env RC2FX_CLIENT_TIMING="$client_timing" RC2FX_GAUGES="$gauges" RC2FX_SRC_MODE="$src_mode" LD_AUDIT="$home/librc2fx_native.so" \
    RC2FX_NATIVE_PROBE_REPORT_SELECTED_ONLY=1 \
    RC2FX_FIRMWARE_PROFILE="$profile_id" RC2FX_FIRMWARE_PROFILES="$home/firmware-profiles.json" \
    RC2FX_NATIVE_PROBE_PLAN="$run/plan.json" RC2FX_USB_SOCKET="$run/usb.sock" \
    RC2FX_NATIVE_PROBE_CONTROL_SOCKET="$run/control.sock" RC2FX_NATIVE_PROBE_REPORT="$run/audit.json" \
    RC2FX_NATIVE_PROBE_EVENTS="$logs/audio-events.log" \
    "$stock_mixer" "$@" 2>> "$run/mixer.log" &
  local mixer=$!
  printf '%s\n' "$mixer" > "$run/native.pid"
  # The supervisor keeps this PID through setsid and env, so supervisor.pid is
  # its own.
  setsid env RC2FX_DATA="$data" RC2FX_LOGS="$logs" \
    "$home/rc2fx-supervisor" --mixer "$mixer" --mixer-start "$(start_time "$mixer")" \
    --script "$run/rc2fx.sh" --controller "$home/rc2fxd" --run "$run" --boot "$boot" \
    -- "${controller[@]}" < /dev/null >> "$logs/rc2fx.log" 2>&1 &
  local watcher=$!
  printf '%s\n' "$watcher" > "$run/supervisor.pid"
  say "RC2FX mixer started (PID $mixer) from $home under the launcher" >> "$logs/rc2fx.log"
  # Whoever ends this shell ends the mixer.
  trap 'kill -TERM "$mixer" 2>/dev/null' TERM INT HUP
  # Waits for whichever ends first. A trapped signal ends the wait with neither
  # and leaves $ended unset.
  local status=0 ended=
  until test -n "${ended:-}"; do
    status=0
    wait -n -p ended "$mixer" "$watcher" || status=$?
  done
  if test "$ended" = "$watcher"; then
    if test "$status" != 0 && alive "$mixer"; then supervisor_lost "$mixer" "$status"; fi
    status=0
    wait "$mixer" || status=$?
    while alive "$mixer"; do wait "$mixer" || status=$?; done
  fi
  say "Mixer exited with status $status$(test "$status" -le 128 || echo " (signal $((status - 128)))"); jackd now $(pgrep -x jackd | tr '\n' ' ')" >> "$logs/rc2fx.log"
  restore_rt_throttle
  exit "$status"
}

start() {
  local wait=120 mixer= old= n force=0
  if test "${1:-}" = --force; then force=1; shift; fi
  if test "${1:-}" = --wait; then wait=${2:?--wait needs seconds}; fi
  test -x /usr/sbin/rc_audio_mixer || { echo 'RC2FX is not installed: /usr/sbin/rc_audio_mixer is missing' >&2; return 1; }
  (cd "$home" && sha256sum -c SHA256SUMS > /dev/null) || { echo "Binaries in $home fail their checksums" >&2; return 1; }
  "$home/rc2fx-firmware" check-device "$home/firmware-profiles.json" "$stock_mixer" /usr/bin/audio-start.sh > /dev/null || return 1
  mkdir -p -m 700 "$boot"
  rm -f "$boot/stock" "$boot/exits"
  local next; next=$(bundle_home)
  if old=$(supervisor); then
    if test "$force" = 0 && test "$(cat "$run/home" 2>/dev/null)" = "$next" && cmp -s "$next/SHA256SUMS" "$run/bundle.sums"; then
      echo "RC2FX is already running from $next (supervisor $old)"; return 0
    fi
    mixer=$(cat "$run/native.pid")
    # A kept gadget has nothing in progress to wait for.
    local deadline=$(( $(uptime_s) + wait ))
    until gadget_kept || test "$(mixer_age "$mixer")" -ge "$mixer_settle"; do
      if test "$(uptime_s)" -ge "$deadline"; then echo 'The running mixer has not settled yet' >&2; return 1; fi
      sleep 2
    done
    echo "Restarting the mixer into $next"
    if gadget_kept; then
      echo 'The USB gadget stays up across the restart'
      touch "$run/keep-usb"
    else
      retire_usb_node "$mixer" "$(pgrep -P "$old" -x rc2fxd || true)" || return 1
    fi
    touch "$run/stop"
    for ((n=0; n<120; n++)); do alive "$old" || break; sleep 0.25; done
    alive "$old" && { echo "RC2FX supervisor $old did not stop" >&2; return 1; }
    wait_for_launch "$wait" "$mixer"
    return
  fi
  local deadline=$(( $(uptime_s) + wait )) told=
  until mixer=$(launched_mixer) && test "$(mixer_age "$mixer")" -ge "$mixer_settle"; do
    if test "$(uptime_s)" -ge "$deadline"; then
      echo 'Stock audio is not running under its launcher or has not finished starting' >&2
      return 1
    fi
    test -n "$told" || { echo 'Waiting for stock audio to finish starting...'; told=1; }
    sleep 2
  done
  echo "Restarting the stock mixer ($mixer) into $next"
  restart_mixer "$mixer" || return 1
  wait_for_launch "$wait" "$mixer"
}

stop() {
  local pid native controller n
  mkdir -p -m 700 "$boot"
  echo 'stopped by rc2fx.sh stop' > "$boot/stock"
  if ! pid=$(supervisor); then echo 'RC2FX is not running'; return 0; fi
  # Retire the HID node first: the supervisor ends the mixer only after the
  # controller has exited.
  native=$(cat "$run/native.pid" 2>/dev/null || true)
  controller=$(pgrep -P "$pid" -x rc2fxd || true)
  retire_usb_node "$native" "$controller" || return 1
  touch "$run/stop"
  for ((n=0; n<120; n++)); do alive "$pid" || break; sleep 0.25; done
  if alive "$pid"; then echo "RC2FX supervisor $pid did not stop" >&2; return 1; fi
  if test -n "$native" && alive "$native"; then
    echo "RC2FX mixer $native is still running; see $logs/rc2fx.log" >&2
    return 1
  fi
  echo 'RC2FX stopped; the launcher is restarting stock audio'
}

status() {
  local pid native
  if pid=$(supervisor); then
    native=$(cat "$run/native.pid" 2>/dev/null || true)
    echo "RC2FX running from $(cat "$run/home" 2>/dev/null) (supervisor $pid, mixer ${native:-starting})"
    if test -n "$native" && alive "$native"; then grep -E '^(Threads|VmRSS):' "/proc/$native/status"; fi
  else
    echo 'RC2FX is not running; stock audio is in charge'
  fi
  if test -x /usr/sbin/rc_audio_mixer; then
    if test -e /opt/rc2fx/disabled; then echo 'Autostart: installed, disabled'; else echo 'Autostart: installed, enabled'; fi
  else
    echo 'Autostart: not installed'
  fi
  test -e "$boot/stock" && echo "This boot: stock until the next start ($(cat "$boot/stock"))"
  gui_status
  recorder_status_line
  test -f "$logs/rc2fx.log" && { echo '--- recent'; tail -n 8 "$logs/rc2fx.log"; }
  return 0
}

# The stock mixer logs Wi-Fi credentials; never show or keep those lines.
redact() { grep -v -i -E 'password|psk|WPA Configuration' || true; }

logs() {
  for file in "$logs/rc2fx.log" "$run/controller.log" "$run/mixer.log" "$vendor_log" "$logs/audio-events.log" "$logs/last-exit.log"; do
    test -f "$file" || continue
    echo "=== $file"
    tail -n 40 "$file" | redact
  done
}

# rc2fx-supervisor (crates/rc2fx-supervisor) runs beside the
# mixer and calls back here for the rare steps. It starts the controller once
# the mixer opens its control socket, and ends the mixer so that the launcher
# restarts it (stock, when it records a reason in $boot/stock) when the
# controller exits, the mixer never becomes ready, or a stop is requested.

# Ends a mixer for the supervisor: $1 is 1 to keep the USB gadget, then the
# mixer, its start time and any controller.
supervise_restart() {
  keep_usb=$1 mixer_start=$3
  restart_mixer "$2" "${@:4}"
}

# The supervisor's last step: $1 is a controller still running, if any.
supervise_exit() {
  set +e
  say 'Supervisor stopping'
  test -z "${1:-}" || stop_owned "$1"
  {
    tail -n 30 "$logs/rc2fx.log"
    echo '--- controller'; tail -n 40 "$run/controller.log"
    echo '--- mixer'; tail -n 40 "$run/mixer.log" | redact
    echo '--- vendor'; tail -n 40 "$vendor_log" | redact
    echo '--- kernel'; dmesg | tail -n 40
  } > "$logs/last-exit.log" 2>&1
  return 0
}

# Why the stock GUI is starting instead of the module: one line in the GUI's
# own log, so a device log shows every reason without a diagnostic run.
gui_say() { printf '%s %s\n' "$(stamp)" "$*" >> "$gui_home/gui.log"; }

# The bundle whose module the GUI may use: a build in RAM wins, as for audio.
gui_bundle() { bundle_home; }

# Records this start; three within 30 s is a crash loop (the display loop
# restarts a dead GUI after about three seconds), so the module is dropped
# for the rest of the boot. Prints the number of starts in the window.
gui_starts() {
  local now line kept=()
  now=$(uptime_s)
  if test -f "$gui_home/starts"; then
    while read -r line; do
      [[ "$line" =~ ^[0-9]+$ ]] || continue
      test "$line" -ge $((now - 30)) || continue
      kept+=("$line")
    done < "$gui_home/starts"
  fi
  kept+=("$now")
  printf '%s\n' "${kept[@]}" > "$gui_home/starts"
  printf '%s\n' "${#kept[@]}"
}

# display-start.sh launches the GUI as soon as `pidof rc_audio_mixer` succeeds,
# before an RC2FX mixer opens its control socket (~9 s). Starting that early
# breaks meter setup, so wait up to 30 s (RC2FX_GUI_WAIT_MAX) for callbacks
# to advance when RC2FX audio is active. Never fails under set -eo pipefail.
audit_callbacks() {
  { grep -o '"callbacks":[0-9]*' "$run/audit.json" 2>/dev/null || true; } | head -n 1 | tr -dc 0-9
  return 0
}

# Is an RC2FX mixer running, as "$run/native.pid" says?
gui_mixer_in_charge() {
  local pid
  test -f "$run/native.pid" || return 1
  read -r pid < "$run/native.pid" 2>/dev/null || return 1
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  local exe
  exe=$(readlink "/proc/$pid/exe" 2>/dev/null) || return 1
  test "$exe" = "$stock_mixer"
}

# This boot may start one but has not written native.pid yet: the audio shim
# is installed and enabled, this is not the first boot, and the boot is young.
# display-start.sh's `pidof rc_audio_mixer` matches the shim's own bash while
# launch() is still setting the runtime directory up, which is the boot case.
gui_expects_rc2fx() {
  local window=${RC2FX_GUI_BOOT_WINDOW:-180}
  test -x "$audio_shim" || return 1
  test ! -e /opt/rc2fx/disabled || return 1
  test ! -e /usr/bin/first-boot || return 1
  local up
  up=$(uptime_s) || up=0
  [[ "$up" =~ ^[0-9]+$ ]] || up=0
  test "$up" -lt "$window"
}

gui_wait_for_mixer() {
  local max=${RC2FX_GUI_WAIT_MAX:-30} polls=0 last= now ready=0
  test -e "$boot/stock" && return 0
  if gui_expects_rc2fx; then
    : # keep polling: the mixer may still be starting
  elif gui_mixer_in_charge; then
    : # a later GUI start while RC2FX runs
  else
    return 0
  fi
  while test "$polls" -lt $(( max * 2 )); do
    sleep 0.5
    polls=$((polls + 1))
    gui_mixer_in_charge || continue
    test -S "$run/control.sock" || continue
    now=$(audit_callbacks) || now=
    if test -n "$last" && test -n "$now" && test "$now" -gt "$last"; then
      ready=1
      break
    fi
    last=$now
  done
  if test "$ready" = 0; then
    gui_say "GUI started before the mixer reported ready (${max} s)"
  elif test "$polls" -gt 1; then
    gui_say "Waited $(( polls / 2 )) s for the mixer before starting the GUI"
  fi
  return 0
}

# Entry used by /usr/sbin/rc_gui_juce: ALWAYS ends in an exec of the stock
# GUI, with the partial-conversion module only when every check passes.
# The EXIT trap's path: an unexpected abort must never be silent (device
# 2026-11-01: the GUI started stock 2 s after the mixer with nothing logged).
gui_unexpected_exit() {
  gui_say "GUI launch failed unexpectedly; stock GUI started" || true
  exec "$stock_gui" "$@"
}

gui_launch() {
  local fallback="gui_unexpected_exit"
  local argument
  for argument in "$@"; do fallback+=$(printf ' %q' "$argument"); done
  trap "$fallback" EXIT
  # No state (and so no log) means no module: the stock GUI still starts.
  mkdir -p -m 700 "$gui_home" 2>/dev/null || exec "$stock_gui" "$@"
  local why= module= selected= diagnostics=quiet
  local disabled="$data/gui-partial-disabled"
  # 1. The mixer is not fully booted yet.
  if test -e /usr/bin/first-boot; then why='first boot after a firmware update'
  elif test -e /usr/bin/production-boot; then why='production boot'
  # 2. Switched off, by settings or for this boot (the crash guard's file).
  elif test -e "$disabled"; then why='partial conversion is disabled in settings'
  elif test -e "$gui_home/off"; then why='partial conversion is off for this boot'
  elif test -e "$gui_home/stock"; then why="$(cat "$gui_home/stock")"
  # 3. The GUI binary this module was qualified against.
  elif ! printf '%s  %s\n' "$gui_sha" "$stock_gui" | sha256sum -c - >/dev/null 2>&1; then
    why='this GUI is not the qualified one'
  # 4. The bundle's module, checked by the bundle's own manifest.
  elif ! selected=$(gui_bundle) || test ! -f "$selected/librc2fx_gui.so"; then
    why='no bundle module is installed'
  elif ! (cd "$selected" && grep -E '  librc2fx_gui\.so$' SHA256SUMS | sha256sum -c - >/dev/null 2>&1); then
    why="the bundle module fails its checksum"
  fi
  if test -z "$why"; then module=$selected/librc2fx_gui.so; fi
  # 5. Crash guard: recorded for every start, module or not.
  local n
  n=$(gui_starts)
  if test -z "$why" && test "$n" -ge 3; then
    printf 'the GUI module crashed it %s times within 30 s\n' "$n" > "$gui_home/stock"
    chmod 600 "$gui_home/stock"
    why="$(cat "$gui_home/stock")"
  fi
  # No check for a running display experiment: ui_preview SIGSTOPs
  # display-start.sh for the whole session, so its loop cannot start a GUI
  # (and so cannot reach this shim) while an experiment owns the screen.
  if test -n "$why"; then
    gui_say "Stock GUI started: $why"
    gui_wait_for_mixer
    exec "$stock_gui" "$@"
  fi
  diagnostics=$(gui_diagnostics)
  diagnostics=${diagnostics%% *}
  # A quiet log would otherwise grow over many starts: keep the last one.
  test -f "$gui_home/pacing.log" && mv -f "$gui_home/pacing.log" "$gui_home/pacing.prev.log"
  gui_wait_for_mixer
  if test "$diagnostics" = verbose; then
    gui_say "GUI module starting from $module (verbose diagnostics)"
  else
    gui_say "GUI module starting from $module"
  fi
  trap - EXIT
  local -a quiet_env=("RC2FX_GUI_PACING_QUIET=1")
  if test "$diagnostics" = verbose; then quiet_env=(); fi
  exec env LD_AUDIT="$module" RC2FX_GUI_PACING=partial-cw "${quiet_env[@]}" \
    RC2FX_GUI_PACING_LOG="$gui_home/pacing.log" "$stock_gui" "$@"
}

# Verbose or quiet GUI diagnostics, and why: the kind of build.
gui_diagnostics() {
  if test "$rc2fx_build" = release; then echo 'quiet (release build)'
  else echo 'verbose (development build)'
  fi
}

# The GUI half of `status`: module or stock, why, and its last counters line.
gui_status() {
  local module= profile= counters=
  if ! test -e "$gui_shim"; then
    echo 'GUI: not installed'
  elif test -e "$data/gui-partial-disabled"; then
    echo 'GUI: stock (partial conversion is disabled in settings)'
  elif test -e "$gui_home/off"; then
    echo 'GUI: stock until the next boot (partial conversion is off)'
  elif test -e "$gui_home/stock"; then
    echo "GUI: stock until the next boot ($(cat "$gui_home/stock"))"
  elif module=$(gui_bundle) && test -f "$module/librc2fx_gui.so"; then
    profile=$(sha256sum "$module/librc2fx_gui.so" 2>/dev/null | cut -c1-12)
    echo "GUI: partial conversion, module ${profile:-unknown} from $module"
  else
    echo 'GUI: stock (no bundle module is installed)'
  fi
  echo "GUI diagnostics: $(gui_diagnostics)"
  counters=$(tail -n 400 "$gui_home/pacing.log" 2>/dev/null | grep '^counters ' | tail -n 1 || true)
  test -n "$counters" && echo "GUI counters: $counters"
  test -f "$gui_home/gui.log" && { echo '--- GUI'; tail -n 3 "$gui_home/gui.log"; }
  return 0
}

# Developer SSH access without reflashing. Manages a single line marked with
# $ssh_marker in root's authorized_keys while preserving all other bytes; never
# restarts sshd or drops open sessions. All checks from rc2fxd are repeated.
# Exit codes: 0 ok, 2 bad input, 3 unsafe path, 4 first boot, 1 other error.
ssh_dir=${RC2FX_SSH_DIR:-/home/root/.ssh}
ssh_host_dir=${RC2FX_SSH_HOST_DIR:-/etc/ssh}
ssh_first_boot=${RC2FX_FIRST_BOOT:-/usr/bin/first-boot}
ssh_marker=rc2fx-managed-developer-key
# An Ed25519 public key blob: its fixed 19-byte header encodes to these 25
# characters, followed by 43 more for the 32-byte key.
ssh_key_pattern='^ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI[A-Za-z0-9+/]{43}$'
ssh_managed_pattern="^ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI[A-Za-z0-9+/]{43} $ssh_marker\$"
ssh_max_keys_bytes=1048576

ssh_unsafe() { echo "unsafe path: $*" >&2; return 3; }

# `ssh_paths_safe create` may create a missing directory (0700); plain
# `ssh_paths_safe` never changes anything. Refuses links, other owners,
# group- or world-writable modes, extra hard links and oversized files.
ssh_paths_safe() {
  local keys=$ssh_dir/authorized_keys parent me mode owner
  me=$(id -u)
  # Every ancestor: a real directory, owned by root or this user, and not
  # writable by anyone else unless it is root's sticky directory (like /tmp).
  parent=$(dirname "$ssh_dir")
  while :; do
    if test -L "$parent" || ! test -d "$parent"; then ssh_unsafe "$parent"; return; fi
    owner=$(stat -c %u "$parent"); mode=$(stat -c %a "$parent")
    if test "$owner" != 0 && test "$owner" != "$me"; then ssh_unsafe "$parent owner"; return; fi
    if (( 8#$mode & 8#022 )) && ! { test "$owner" = 0 && (( 8#$mode & 8#1000 )); }; then
      ssh_unsafe "$parent is writable by others"; return
    fi
    test "$parent" != / || break
    parent=$(dirname "$parent")
  done
  if test -L "$ssh_dir"; then ssh_unsafe "$ssh_dir is a link"; return; fi
  if ! test -e "$ssh_dir"; then
    test "${1:-}" = create || return 0
    mkdir -m 700 "$ssh_dir" || { ssh_unsafe "cannot create $ssh_dir"; return; }
  fi
  if ! test -d "$ssh_dir"; then ssh_unsafe "$ssh_dir is not a directory"; return; fi
  if test "$(stat -c %u "$ssh_dir")" != "$me"; then ssh_unsafe "$ssh_dir owner"; return; fi
  mode=$(stat -c %a "$ssh_dir")
  if (( 8#$mode & 8#022 )); then ssh_unsafe "$ssh_dir is writable by others"; return; fi
  test -e "$keys" || test -L "$keys" || return 0
  if test -L "$keys" || ! test -f "$keys"; then ssh_unsafe "$keys is not a regular file"; return; fi
  if test "$(stat -c %h "$keys")" != 1; then ssh_unsafe "$keys has other links"; return; fi
  if test "$(stat -c %u "$keys")" != "$me"; then ssh_unsafe "$keys owner"; return; fi
  mode=$(stat -c %a "$keys")
  if (( 8#$mode & 8#022 )); then ssh_unsafe "$keys is writable by others"; return; fi
  if test "$(stat -c %s "$keys")" -gt "$ssh_max_keys_bytes"; then ssh_unsafe "$keys is too large"; return; fi
  # The shell reader cannot keep NUL bytes, so such a file is never rewritten.
  if ! tr -d '\000' < "$keys" | cmp -s - "$keys"; then ssh_unsafe "$keys contains NUL bytes"; return; fi
}

# Print authorized_keys without its managed lines, byte for byte (a last
# line without a newline stays without one). Sets ssh_unmanaged to the number
# of other key lines and ssh_managed to the managed key, if any.
ssh_filter() {
  local keys=$ssh_dir/authorized_keys line
  ssh_unmanaged=0 ssh_managed=
  test -f "$keys" || return 0
  while IFS= read -r line; do ssh_line "$line" $'\n'; done < "$keys" || return 1
  if test -n "$line"; then ssh_line "$line" ''; fi
}
ssh_line() {
  if [[ $1 =~ $ssh_managed_pattern ]]; then
    test -n "$ssh_managed" || ssh_managed=${1% "$ssh_marker"}
    return 0
  fi
  [[ $1 =~ ^[[:space:]]*(#|$) ]] || ssh_unmanaged=$((ssh_unmanaged + 1))
  printf '%s%s' "$1" "$2"
}

# Replace authorized_keys atomically with its unmanaged content, plus the
# managed line `$1` when given.
ssh_write_keys() {
  local tmp
  tmp=$(mktemp "$ssh_dir/.authorized_keys.XXXXXX") || return 1
  if ! { ssh_filter > "$tmp" && ssh_append "$tmp" "${1:-}"; }; then rm -f "$tmp"; return 1; fi
  chmod 600 "$tmp" && mv -f "$tmp" "$ssh_dir/authorized_keys" || { rm -f "$tmp"; return 1; }
}
ssh_append() {
  test -n "$2" || return 0
  if test -s "$1" && test "$(tail -c 1 "$1" | od -An -c | tr -d ' ')" != '\n'; then printf '\n' >> "$1"; fi
  printf '%s\n' "$2" >> "$1"
}

ssh_status() {
  local line pub
  ssh_filter > /dev/null || { echo 'cannot read authorized_keys' >&2; return 1; }
  test -z "$ssh_managed" || echo "managed $ssh_managed"
  echo "unmanaged $ssh_unmanaged"
  for pub in "$ssh_host_dir"/ssh_host_*_key.pub; do
    test -f "$pub" && ! test -L "$pub" || continue
    line=$(head -c 1024 "$pub" | head -n 1 | cut -d' ' -f1-2)
    if [[ $line =~ ^(ssh-ed25519|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384|ecdsa-sha2-nistp521|ssh-rsa)\ [A-Za-z0-9+/=]{16,}$ ]]; then
      echo "host $line"
    fi
  done
  if ! command -v pgrep > /dev/null; then echo 'server unknown'
  elif pgrep -x sshd > /dev/null 2>&1; then echo 'server running'
  else echo 'server stopped'; fi
}

# Leaves a record of which key gained root access. Best effort: the key is
# already in place, so nothing here may fail the request.
ssh_log_added() {
  local print=
  if command -v ssh-keygen > /dev/null; then
    print=$(printf '%s\n' "$1" | ssh-keygen -lf - 2> /dev/null | cut -d' ' -f2) || print=
  fi
  { mkdir -p "$logs" && say "Developer SSH key added: ${print:-fingerprint unavailable}" >> "$logs/rc2fx.log"; } 2> /dev/null || true
}

developer_ssh() {
  local raw= key
  case "${1:-}" in
    status) ssh_paths_safe || return; ssh_status ;;
    enable|disable)
      if test -e "$ssh_first_boot"; then echo 'first boot after an update: try again once it finishes' >&2; return 4; fi
      if test "$1" = enable; then
        # Exactly one key line and at most one newline, nothing else. read
        # succeeds only if it stopped early: at a NUL byte or after 513
        # characters. Both are refused; reaching end of input is the normal case.
        if IFS= read -r -d '' -n 513 raw; then echo 'invalid public key' >&2; return 2; fi
        key=${raw%$'\n'}
        if test "${#raw}" -gt 512 || ! [[ $key =~ $ssh_key_pattern ]]; then echo 'invalid public key' >&2; return 2; fi
        ssh_paths_safe create || return
        ssh_write_keys "$key $ssh_marker" || { echo 'could not write authorized_keys' >&2; return 1; }
        ssh_log_added "$key"
      else
        ssh_paths_safe || return
        ssh_filter > /dev/null || { echo 'cannot read authorized_keys' >&2; return 1; }
        if test -n "$ssh_managed"; then
          ssh_write_keys || { echo 'could not write authorized_keys' >&2; return 1; }
        fi
      fi
      ssh_status ;;
    *) echo 'usage: rc2fx.sh developer-ssh status|enable|disable' >&2; return 2 ;;
  esac
}

# Explicit one-shot recovery authorization; never called by normal startup.
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

# ---- flash evidence ----------------------------------------------------------
# Flashes occasionally leave RØDE files damaged (e.g. zero-filled) on the new
# root. We check twice and save evidence on the persistent settings partition:
# - flash-verify (via a per-boot bind mount over rode-update.sh): syncs, drops
#   caches, verifies the new partition against image.sha256, and re-extracts
#   damaged files from the archive before RØDE reboots.
# - image-check at boot: archives any damaged files before repairing them.
# Never blocks or alters rode-update.sh's exit status.
evidence_root=${RC2FX_EVIDENCE_ROOT:-/Application/update-staging/rc2fx-flash}
rode_update=${RC2FX_RODE_UPDATE:-/usr/bin/rode-update.sh}
update_partition=${RC2FX_UPDATE_PARTITION:-/Application/update-partition}
# A new evidence folder named for now and why, printed; nothing on failure.
evidence_dir() {
  local dir
  dir="$evidence_root/$(date -u +%Y%m%dT%H%M%SZ)-$1"
  mkdir -p "$dir" 2> /dev/null && printf '%s\n' "$dir"
}
# Appends one line to the evidence history.
evidence_note() {
  mkdir -p "$evidence_root" 2> /dev/null && say "$*" >> "$evidence_root/history.log" 2> /dev/null || true
}
# Copies the damaged file root/name into dir and describes it: size, inode,
# how many of its bytes are NUL, and its digest against the one wanted.
evidence_file() {
  local dir=$1 root=$2 name=$3 want=$4 size nonnul
  test -f "$root/$name" || { printf '%s missing\n' "$name" >> "$dir/files.txt"; return 0; }
  size=$(stat -c %s "$root/$name" 2> /dev/null || echo ?)
  nonnul=$(tr -d '\000' < "$root/$name" 2> /dev/null | wc -c || echo ?)
  printf '%s size=%s nul=%s inode=%s mtime=%s got=%s want=%s\n' "$name" "$size" \
    "$(( ${size:-0} - ${nonnul:-0} ))" "$(stat -c %i "$root/$name" 2> /dev/null || echo ?)" \
    "$(stat -c %Y "$root/$name" 2> /dev/null || echo ?)" \
    "$(sha256sum < "$root/$name" 2> /dev/null | cut -d' ' -f1)" "$want" >> "$dir/files.txt"
  # Small enough to keep whole: RØDE's damaged files so far were scripts,
  # headers and small binaries.
  if test "${size:-0}" -le 16777216 2> /dev/null; then
    mkdir -p "$dir/files/$(dirname "$name")" 2> /dev/null \
      && cp "$root/$name" "$dir/files/$name" 2> /dev/null || true
  fi
}
# The storage picture: kernel messages, mounts, the eMMC and both roots'
# superblock times and mount counts.
evidence_storage() {
  local dir=$1 p
  { uptime; cat /proc/cmdline; echo; findmnt -rn -o SOURCE,TARGET,FSTYPE,OPTIONS 2> /dev/null | grep -E 'mmcblk|update' || true
    echo; for p in name type manfid fwrev date; do printf '%s=%s ' "$p" "$(cat "/sys/block/mmcblk1/device/$p" 2> /dev/null)"; done
    printf 'write_cache=%s\n' "$(cat /sys/block/mmcblk1/queue/write_cache 2> /dev/null)"
    # What RC2FX's audio changes in scheduling, as it stands during the flash.
    printf 'sched_rt_runtime_us=%s\n' "$(cat /proc/sys/kernel/sched_rt_runtime_us 2> /dev/null)"
    grep -E 'mmc' /proc/interrupts 2> /dev/null | while read -r p _; do
      p=${p%:}; printf 'irq %s affinity=%s effective=%s\n' "$p" "$(cat "/proc/irq/$p/smp_affinity_list" 2> /dev/null)" "$(cat "/proc/irq/$p/effective_affinity_list" 2> /dev/null)"
    done
    for p in $blackbox_irqs; do printf 'audio irq %s affinity=%s\n' "$p" "$(cat "/proc/irq/$p/smp_affinity_list" 2> /dev/null)"; done
    printf 'rc2fx running=%s\n' "$(pgrep -f '[r]c2fxd' > /dev/null && echo yes || echo no)"
    for p in 1 2; do
      # ext4 superblock at 1024: mount count (0x34), state (0x3A), feature
      # flags (0x5C..), mkfs time (0x108).
      printf 'p%s superblock: %s\n' "$p" "$(dd if=/dev/mmcblk1p$p bs=1024 skip=1 count=1 status=none 2> /dev/null | od -An -tx1 -j52 -N48 | tr -s ' \n' ' ')"
    done
  } > "$dir/storage.txt" 2>&1 || true
  dmesg > "$dir/dmesg.txt" 2> /dev/null || true
}
# Wraps rode-update.sh for this boot (from launch). Idempotent: a later
# launch (a new bundle installed without a reboot) rewrites the wrapper in
# place, which the bind mount shows at once; RØDE's original is copied only
# while nothing is mounted over it.
flash_watch_start() {
  local mounted=0
  test -f "$rode_update" && test -d "$boot" || return 0
  mountpoint -q "$rode_update" 2> /dev/null && mounted=1
  if test $mounted = 0; then
    cp -p "$rode_update" "$boot/rode-update.sh" || return 0
  fi
  test -f "$boot/rode-update.sh" || return 0
  cat > "$boot/rode-update.wrap.sh" << EOF || return 0
#!/bin/sh
# RC2FX: runs RØDE's rode-update.sh unchanged, then checks what it wrote
# (rc2fx.sh flash-verify) before RØDE reboots, rewriting any file that did not
# reach the eMMC intact from the same archive. Its exit status is RØDE's.
# RØDE's script deletes the archive when it is done; a sleeping child keeps it
# open, so it stays readable through /proc until the check is over.
keeper=
if test -r "\${1}archive.tar.gz"; then
  sleep 1800 9< "\${1}archive.tar.gz" > /dev/null 2>&1 &
  keeper=\$!
fi
/bin/sh $(printf %q "$boot/rode-update.sh") "\$@"
status=\$?
archive=
test -n "\$keeper" && archive=/proc/\$keeper/fd/9
timeout 900 /bin/bash $(printf %q "$home/rc2fx.sh") flash-verify "\$status" "\$archive" < /dev/null > /dev/null 2>&1 || true
test -n "\$keeper" && kill "\$keeper" 2> /dev/null
exit \$status
EOF
  chmod 755 "$boot/rode-update.wrap.sh" || return 0
  test $mounted = 1 || mount --bind "$boot/rode-update.wrap.sh" "$rode_update" || return 0
}
# After RØDE's update script: is what it wrote on the eMMC what the image has?
flash_verify() {
  local status=${1:-?} archive=${2:-} dir manifest failed=() line name want started round left
  started=$(uptime_s)
  dir=$(evidence_dir verify) || return 0
  exec > "$dir/verify.log" 2>&1
  say "rode-update.sh exited $status"
  manifest=$update_partition/opt/rc2fx/image.sha256
  if ! test -f "$manifest"; then
    say "No image.sha256 on $update_partition; nothing to check"
    evidence_note "flash verify: no manifest on the new partition (rode-update.sh exit $status)"
    return 0
  fi
  sync
  # Reads must come from the eMMC, not from what the kernel still holds.
  echo 3 > /proc/sys/vm/drop_caches 2> /dev/null || say 'Could not drop the page cache'
  while IFS= read -r line; do
    name=${line%: FAILED*}
    test "$name" != "$line" && failed+=("$name")
  done < <(cd "$update_partition" && sha256sum -c --quiet "$manifest" 2> /dev/null || true)
  for name in "${failed[@]}"; do
    [[ $name == /* || $name == *..* ]] && continue
    want=$(awk -v n="$name" 'substr($0, 67) == n { print substr($0, 1, 64); exit }' "$manifest")
    evidence_file "$dir" "$update_partition" "$name" "$want"
  done
  evidence_storage "$dir"
  say "Checked $(wc -l < "$manifest") files in $(( $(uptime_s) - started )) s: ${#failed[@]} damaged"
  evidence_note "flash verify (before RØDE's reboot): ${#failed[@]} damaged${failed:+: ${failed[*]}} [$(basename "$dir")]"
  test ${#failed[@]} -gt 0 || return 0
  # Writes the damaged files again from the archive RØDE unpacked, as RØDE's
  # tar did, and checks them again, until they read back right.
  if test -z "$archive" || ! test -r "$archive"; then
    say 'The archive is gone; the image check at boot will repair what it can'
    evidence_note "flash repair: no archive to repair from"
    return 0
  fi
  left=("${failed[@]}")
  for round in 1 2 3; do
    # The archive names most files ./path and RØDE's additions path; tar
    # takes the members that exist under either name.
    tar -xzf "$archive" -C "$update_partition" "${left[@]/#/./}" 2> /dev/null || true
    tar -xzf "$archive" -C "$update_partition" "${left[@]}" 2> /dev/null || true
    sync
    echo 3 > /proc/sys/vm/drop_caches 2> /dev/null || true
    failed=()
    for name in "${left[@]}"; do
      want=$(awk -v n="$name" 'substr($0, 67) == n { print substr($0, 1, 64); exit }' "$manifest")
      test "$(sha256sum < "$update_partition/$name" 2> /dev/null | cut -d' ' -f1)" = "$want" || failed+=("$name")
    done
    say "Repair round $round: $(( ${#left[@]} - ${#failed[@]} )) rewritten, ${#failed[@]} still damaged"
    left=("${failed[@]}")
    test ${#left[@]} -eq 0 && break
  done
  evidence_note "flash repair (before RØDE's reboot): ${#left[@]} still damaged after round $round${left:+: ${left[*]}}"
}
# ---- end of flash evidence ----------------------------------------------------

# ---- image check -------------------------------------------------------------
# Verifies RØDE's files against image.sha256 once per boot in the background
# until clean. Repairs damaged files only from a hash-matched copy on the other
# root partition. Writes $data/image-check (served at GET api/v1/image-check).
image_manifest=${RC2FX_IMAGE_MANIFEST:-$home/image.sha256}
image_root=${RC2FX_IMAGE_ROOT:-/}
image_result=$data/image-check
image_other_mount=/run/rc2fx-other-root
image_root_view=/run/rc2fx-root
# Starts the check unless it has passed for this manifest or one is running.
image_check_start() {
  test -f "$image_manifest" || return 0
  test ! -e /usr/bin/first-boot || return 0
  mkdir "$boot/image-check" 2> /dev/null || return 0
  local digest
  digest=$(sha256sum < "$image_manifest" | cut -d' ' -f1)
  grep -qx "manifest=$digest" "$image_result" 2> /dev/null \
    && grep -qx 'status=ok' "$image_result" && return 0
  # Set aside before rc2fxd starts, so the app never reads an earlier
  # result while this boot's check runs (see image_check).
  mv -f "$image_result" "$image_result.previous" 2> /dev/null || true
  # The log stays in RAM: a damaged image can leave the root full, and the
  # check must not stop at its first message.
  setsid nohup /bin/bash "$home/rc2fx.sh" image-check < /dev/null >> "$boot/image-check.log" 2>&1 &
}
# The root partition this boot did not mount, read-only, or nothing.
image_other_root() {
  if test -n "${RC2FX_OTHER_ROOT:-}"; then echo "$RC2FX_OTHER_ROOT"; return; fi
  local root other
  root=$(tr ' ' '\n' < /proc/cmdline | sed -n 's#^root=/dev/mmcblk1p\([12]\)$#\1#p')
  case "$root" in 1) other=2 ;; 2) other=1 ;; *) return 1 ;; esac
  mkdir -p "$image_other_mount"
  mountpoint -q "$image_other_mount" \
    || mount -o ro,noload "/dev/mmcblk1p$other" "$image_other_mount" || return 1
  echo "$image_other_mount"
}
image_check() {
  test -f "$image_manifest" || { echo "no image manifest at $image_manifest"; return 0; }
  # On a full root the lock file may not be creatable; one check per boot is
  # still all `launch` starts.
  { exec 8> "$data/image-check.lock"; } 2> /dev/null || exec 8> /dev/null
  flock -n 8 || { echo 'another image check is running'; return 0; }
  local digest started failed=() repaired=() damaged=() line name want other source tmp status
  digest=$(sha256sum < "$image_manifest" | cut -d' ' -f1)
  # A result from an earlier boot or flash must not answer for this one: the
  # manifest lists only RØDE's files, so a reflash of the same RØDE version
  # has the same digest, and an old "repaired" would ask for a restart this
  # flash never needed. Until this check records its own result the API
  # answers pending. The old record stays beside it for diagnosis.
  mv -f "$image_result" "$image_result.previous" 2> /dev/null || true
  started=$(stamp)
  # The real root's files, not what is mounted over them: this boot's flash
  # watch binds a wrapper over rode-update.sh, which must neither read as
  # damage nor take a repair. A bind of / without its submounts shows the
  # files on the eMMC.
  if test "$image_root" = / && mkdir -p "$image_root_view" \
     && { mountpoint -q "$image_root_view" || mount --bind / "$image_root_view"; }; then
    image_root=$image_root_view
  fi
  say "Checking $(wc -l < "$image_manifest") files listed in $image_manifest"
  # Only the listed files that do not match, as "path: FAILED ...".
  while IFS= read -r line; do
    name=${line%: FAILED*}
    test "$name" != "$line" && failed+=("$name")
  done < <(cd "$image_root" && nice -n 19 ionice -c3 sha256sum -c --quiet "$image_manifest" 2> /dev/null || true)
  # The damaged files as found, before any repair (flash evidence).
  local evidence=
  if test ${#failed[@]} -gt 0 && evidence=$(evidence_dir boot); then
    evidence_storage "$evidence"
    for name in "${failed[@]}"; do
      [[ $name == /* || $name == *..* ]] && continue
      want=$(awk -v n="$name" 'substr($0, 67) == n { print substr($0, 1, 64); exit }' "$image_manifest")
      evidence_file "$evidence" "$image_root" "$name" "$want"
    done
  fi
  evidence_note "image check at boot: ${#failed[@]} damaged${failed:+: ${failed[*]}}${evidence:+ [$(basename "$evidence")]}"
  for name in "${failed[@]}"; do
    # The manifest's own paths, never one that leaves the root.
    if [[ $name == /* || $name == *..* ]]; then damaged+=("$name"); continue; fi
    want=$(awk -v n="$name" 'substr($0, 67) == n { print substr($0, 1, 64); exit }' "$image_manifest")
    source=
    if test -n "$want" && other=$(image_other_root) && test -f "$other/$name" \
       && test "$(sha256sum < "$other/$name" | cut -d' ' -f1)" = "$want"; then
      source=$other/$name
    fi
    if test -z "$source"; then
      say "Damaged and not repairable: $name"
      damaged+=("$name")
      continue
    fi
    # The same size rewrites the file's own blocks, which works on a full
    # root; another size goes through a copy beside it.
    if test -f "$image_root/$name" \
       && test "$(stat -c %s "$image_root/$name")" = "$(stat -c %s "$source")"; then
      dd if="$source" of="$image_root/$name" conv=notrunc,fsync status=none || true
    else
      tmp="$image_root/$name.rc2fx-repair"
      { cp -p "$source" "$tmp" && sync && mv -f "$tmp" "$image_root/$name"; } || rm -f "$tmp"
    fi
    sync
    if test "$(sha256sum < "$image_root/$name" | cut -d' ' -f1)" = "$want"; then
      say "Repaired from the other root partition: $name"
      repaired+=("$name")
    else
      say "Repair did not take: $name"
      damaged+=("$name")
    fi
  done
  if mountpoint -q "$image_other_mount" 2> /dev/null; then umount "$image_other_mount" || true; fi
  if test "$image_root" = "$image_root_view"; then umount "$image_root_view" 2> /dev/null || true; fi
  if test ${#damaged[@]} -gt 0; then status=damaged
  elif test ${#repaired[@]} -gt 0; then status=repaired
  else status=ok
  fi
  tmp=$data/.image-check.$$
  {
    echo schema=1
    echo "status=$status"
    echo "manifest=$digest"
    echo "checked=$started"
    for name in "${repaired[@]}"; do echo "repaired=$name"; done
    for name in "${damaged[@]}"; do echo "damaged=$name"; done
  } > "$tmp" 2> /dev/null && chmod 600 "$tmp" && mv -f "$tmp" "$image_result" \
    || { rm -f "$tmp"; say "Could not record the result in $image_result; is the root full?"; }
  say "Image check: $status (${#repaired[@]} repaired, ${#damaged[@]} damaged)"
}
# ---- end of the image check --------------------------------------------------

# ---- signed in-place updates ------------------------------------------------
# rc2fxd verifies/swaps a signed release and spawns a detached `update-commit ID`
# so the restart doesn't kill the driver. Request file key=value fields:
#   mode=commit|rollback  target=VERSION  from=VERSION  deadline=SECONDS
#   gui=0|1  (GUI shim changed; takes effect next boot)
#   restart=controller|mixer  (controller: reload rc2fxd only, keeping audio up)
update_dir=/run/rc2fx-update

# One record for `GET api/v1/update/last`, in the same key=value form rc2fxd
# writes when it restores a bundle itself.
update_record() {
  local outcome=$1 from=${2:-} to=${3:-} reason=${4:-} gui=${5:-0} dir=/opt/rc2fx/data tmp
  mkdir -p -m 700 "$dir" || return 0
  tmp=$dir/.update-last.$$
  if printf 'schema=1\noutcome=%s\nfrom=%s\nto=%s\nreason=%s\ngui=%s\n' \
      "$outcome" "$from" "$to" "$reason" "$gui" > "$tmp"; then
    chmod 600 "$tmp" && mv -f "$tmp" "$dir/update-last"
  else
    rm -f "$tmp"
  fi
}

# Whether the restarted controller is the target release and its audio is
# advancing. audit.json is the native library's own report.
update_ready() {
  local target=$1 first second
  test -f "$update_dir/started" || return 1
  grep -qx "version=$target" "$update_dir/started" || return 1
  pgrep -x rc2fxd > /dev/null || return 1
  first=$(audit_callbacks); sleep 2; second=$(audit_callbacks)
  test -n "$first" && test -n "$second" && test "$second" -gt "$first"
}

# Applies an update that changed only the controller's side (restart=controller)
# by having the supervisor restart the controller alone: audio keeps running.
# Only when the running mixer was launched from this bundle with the same
# mixer-side files and script and its supervisor can reload. The new
# controller starts only if the library's paths and settings match the saved
# ones (Runtime::connect); if it is not ready within a minute, the caller
# restarts the mixer as for any other update.
update_reload() {
  local target=$1 stop_at=$2 name until
  supervisor > /dev/null || return 1
  grep -qx reload "$run/supervisor.features" 2>/dev/null || return 1
  test "$(cat "$run/home" 2>/dev/null)" = "$home" || return 1
  cmp -s "$run/rc2fx.sh" "$home/rc2fx.sh" || return 1
  for name in librc2fx_native.so rc2fx-supervisor librc2fx_gui.so librc2fx_jackerr.so rc2fx_jackerr_snapshot; do
    test "$(awk -v n="$name" '$2 == n' "$run/bundle.sums" 2>/dev/null)" \
      = "$(awk -v n="$name" '$2 == n' "$home/SHA256SUMS")" || return 1
  done
  say "Restarting the controller alone; audio keeps running" >> "$logs/update.log"
  touch "$run/reload"
  until=$(( $(uptime_s) + 60 ))
  test "$until" -le "$stop_at" || until=$stop_at
  while test "$(uptime_s)" -lt "$until"; do
    if update_ready "$target"; then
      # What the running mixer was launched with, as far as `start` compares.
      cp "$home/SHA256SUMS" "$run/bundle.sums"
      return 0
    fi
    sleep 1
  done
  return 1
}

update_commit() {
  local id=${1:?update session id}
  local req=$update_dir/commit-$id.request
  local mode= target= from= deadline= gui=0 restart=mixer status=0 ok=0 stop_at
  test -f "$req" || { echo "no update request $req" >&2; return 2; }
  while IFS='=' read -r key value; do
    case "$key" in
      mode) mode=$value;;
      target) target=$value;;
      from) from=$value;;
      deadline) deadline=$value;;
      gui) gui=$value;;
      restart) restart=$value;;
    esac
  done < "$req"
  case "$mode" in commit|rollback) ;; *) echo "unknown update mode ${mode:-}" >&2; return 2;; esac
  [[ $target =~ ^[0-9]+(\.[0-9]+)*$ ]] || { echo 'invalid update target' >&2; return 2; }
  [[ $deadline =~ ^[0-9]+$ ]] || deadline=300
  [[ $gui =~ ^[01]$ ]] || gui=0
  mkdir -p -m 700 "$update_dir" "$logs"
  # One driver at a time: the lock is held across the restart.
  exec 9>"$update_dir/driver.lock" || return 1
  flock -n 9 || { echo 'another update driver is running' >&2; return 1; }
  say "Update $id: $mode to $target (from ${from:-unknown})" >> "$logs/update.log"
  if test "$mode" = commit; then
    # The launchers start the new bundle while this runs; without this record
    # they would take the pending marker for a power cut and use the old one.
    printf '%s %s\n' "$$" "$(start_time "$$")" > "$update_dir/driver"
  else
    rm -f "$update_dir/driver"
  fi
  rm -f "$update_dir/started"
  stop_at=$(( $(uptime_s) + deadline ))
  if test "$mode" = commit && test "$restart" = controller; then
    if update_reload "$target" "$stop_at"; then
      ok=1
    else
      say "Update $id: the controller alone could not apply it; restarting the mixer" >> "$logs/update.log"
    fi
  fi
  if test "$ok" = 0; then
    start --force --wait "$deadline" || status=$?
    say "Update $id: restart finished with status $status" >> "$logs/update.log"
    if test "$mode" = commit && test "$status" -eq 0; then
      while test "$(uptime_s)" -lt "$stop_at"; do
        if update_ready "$target"; then ok=1; break; fi
        sleep 2
      done
    fi
  fi
  if test "$ok" = 1; then
    update_record committed "$from" "$target" '' "$gui"
    rm -f /opt/.rc2fx-update-pending
    rm -f "$update_dir/driver"
    # Nothing starts the rollback copy once the marker is gone. Its files are
    # the old release's, kept alive by their hard links, so it would hold that
    # space until the next update. One holding settings or logs is kept.
    if test -d /opt/.rc2fx-previous && ! test -L /opt/.rc2fx-previous \
        && ! test -e /opt/.rc2fx-previous/data && ! test -e /opt/.rc2fx-previous/logs; then
      rm -rf /opt/.rc2fx-previous
    fi
    say "Update $id: committed $target" >> "$logs/update.log"
    return 0
  fi
  say "Update $id: $target did not become ready; restoring the previous version" >> "$logs/update.log"
  if test "$mode" = commit; then
    # With the driver record gone, the launchers prefer .rc2fx-previous; its
    # controller restores the files and records the outcome.
    rm -f "$update_dir/driver"
    status=0
    start --force --wait "$deadline" || status=$?
    say "Update $id: rollback restart finished with status $status" >> "$logs/update.log"
  fi
  # An interrupted rollback is finished by the next boot's controller.
  return 0
}
# ---- end of signed in-place updates ----------------------------------------

case "${1:-}" in
  arm-power-recovery) arm_power_recovery ;;
  update-commit) shift; update_commit "$@" ;;
  gui-launch) shift; gui_launch "$@" ;;
  start) shift; start "$@" ;;
  restart) shift; start --force "$@" ;;
  stop) stop ;;
  status) status ;;
  logs) logs ;;
  launch) shift; launch "$@" ;;
  usb-quiesce) usb_quiesce ;;
  supervise-restart) shift; supervise_restart "$@" ;;
  supervise-exit) shift; supervise_exit "$@" ;;
  developer-ssh) shift; developer_ssh "$@" ;;
  image-check) image_check ;;
  flash-verify) shift; flash_verify "$@" ;;
  *) sed -n '2,24s/^# \{0,1\}//p' "$0"; exit 2 ;;
esac
