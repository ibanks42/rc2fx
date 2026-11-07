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
#   rc2fx.sh ui-preview --restart-gui /run/.../librc2fx_ui.so [SECONDS]
#                                    RAM-only touchscreen preview; audio stays up
#   rc2fx.sh ui-run --restart-gui /run/.../librc2fx_ui.so
#                                    keep the launcher running from RAM
#   rc2fx.sh developer-ssh status|enable|disable
#                                    RC2FX's own developer SSH key; called by
#                                    rc2fxd for the desktop app over USB 1
#
# developer-ssh adds or removes one marked line in root's authorized_keys.
# arm-power-recovery explicitly sets the bootloader's one-shot power flag.
# Other commands do not change firmware or stock files.
# The launcher (audio-start.sh) runs `rc_audio_mixer` by name with /usr/sbin ahead of /usr/bin, so the shim
# installed as /usr/sbin/rc_audio_mixer runs `launch` as the launcher's own
# child and execs the stock executable, injected unless this boot has fallen
# back to stock. Every start and stop is one of the launcher's own mixer
# restarts: it restarts JACK and the screen UI exactly as it does after a
# stock crash.
set -euo pipefail
# GitHub release packages rewrite this line to `release` (xtask package with
# RC2FX_RELEASE_BUILD=1); development builds default to verbose GUI
# diagnostics, releases to quiet ones.
rc2fx_build=development
home=$(cd "$(dirname "$0")" && pwd)
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
show=${RC2FX_STOCK_SHOW:-}
profile_id=${RC2FX_FIRMWARE_PROFILE:-}
# Only an opted-in Duo image carries this marker. It admits experimental
# runtime behavior, never a different board or a Pro II firmware image.
RC2FX_EXPERIMENTAL_DUO=0
if [[ -f "$home/experimental-duo" && $(< "$home/experimental-duo") == 1 ]]; then
  RC2FX_EXPERIMENTAL_DUO=1
fi
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

# Keep RODE's descriptor byte-for-byte up to its final End Collection and
# declare RC2FX's output report 5 and input report 6 inside that one
# collection, under RODE's global items. Windows lists each top-level
# collection as its own HID device, and RODE's Virtual Device Driver opens
# whichever HID device on the mixer Windows lists first, so a second collection
# can hide the mixer from it. RODE's software does not use IDs 5 and 6. The USB
# controller has no free IN endpoint for a separate interface. Stage only in
# /run. The stock script, audio layout, endpoints and boot files stay
# unchanged. The native system hook substitutes this copy at normal start.
# The kernel caps real-time threads at 950 of every 1000 ms per core and, when
# a core crosses it, suspends every real-time thread there for the rest of the
# second, the USB gadget's interrupt thread on CPU 0 included: three times
# (2026-10-30) that left USB 1 dead until a reboot. The suppression worker
# runs below every RØDE real-time thread, so, as real-time audio systems do,
# RC2FX lifts the cap for the mixer's lifetime. Stock restores the default.
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

# The vendor's HID thread opens an existing /dev/hidg0 before startup replaces
# the gadget. Its open lacks O_CLOEXEC, so the teardown shell can inherit that
# handle and close it AFTER the old kernel character device has been freed.
# This firmware's driver does not pin that allocation for open descriptors.
# Retire only the devtmpfs name before a full mixer restart. Existing handles
# keep working until their owners exit; vendor gadget setup creates a fresh
# name. Never remove the gadget or its descriptors while a process holds it.
# The mixer's short-lived `sh -c` children inherit the handle too; wait for
# those to exit rather than refuse on the instant one happens to be running.
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
  say 'Retired old USB HID node before mixer restart'
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

# No wait after the kill: a blocked reap once left stock audio paused. Zombies
# are reaped by their parent.
stop_owned() {
  local target=$1
  alive "$target" || return 0
  kill -TERM "$target" 2>/dev/null || true
  for ((n=0; n<50; n++)); do alive "$target" || return 0; sleep 0.1; done
  kill -KILL "$target" 2>/dev/null || true
  for ((n=0; n<30; n++)); do alive "$target" || return 0; sleep 0.1; done
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
# HID node is retired first; a foreign owner refuses the restart.
keep_usb=0
restart_mixer() {
  local mixer=$1; shift
  test "$keep_usb" = 1 || retire_usb_node "$mixer" "$@" || return 1
  local pid
  for pid in "$@"; do stop_owned "$pid" || true; done
  stop_owned "$mixer"
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
# At each RC2FX mixer start the launch below replaces the running jackd once,
# inside the mixer launch seam where audio is already restarting: same
# executable, argv and environment, plus LD_PRELOAD of the bundle's
# checksummed librc2fx_jackerr.so. The replacement starts the way
# audio-start.sh's restartJack starts jackd (from the root cpuset, not
# pinned), and then each of its threads gets the cpuset
# and CPUs its role had in the old server's inventory; it must then match that
# inventory exactly, have the same PCM devices open and leave the audio IRQs
# as they were. A server whose threads a stock start would not reproduce
# (policy, priority, nice) is left alone. Any failure after the stop starts a
# plain jackd the same way, verified the same way, and marks the attempt
# failed. The next ordinary
# mixer restart (the vendor loop's restartJack) brings back stock jackd.
# Never rerun audio-start.sh: its top level can flash controllers and change
# boot settings. Nothing under /usr, no mount, no vendor file changes.
blackbox_proc=/proc
blackbox_cpuset=/sys/fs/cgroup/cpuset
blackbox_jackd=/usr/bin/jackd
blackbox_callme=/etc/init.d/callme-t-service
blackbox_owner=0
# The audio controller's IRQs: recorded before the replacement; if a JACK
# start moved them they are put back exactly, or the server is not kept.
blackbox_irqs='24 27'
# The persistent recorder: the bundle's own librc2fx_jackerr.so, preloaded
# into jackd at every RC2FX mixer start (the boot's first included) unless
# $recorder_off exists. Its rings and logs live in RAM in $recorder_dir; it
# never deletes evidence. recorder_evidence_limit is an admission check only:
# past that many bytes under the root a start does not replace jackd. It is
# not a runtime cap: a running server's ring is fixed in size, but its stdout
# (jackd.log) can still grow, and nothing here rotates, truncates or limits
# it while the real-time server runs.
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
# Bytes of evidence in directory $1: ring directories the host has not
# archived (no `acked` file) and the two server logs (always; they keep
# growing). Nothing is ever removed; an archived ring only stops counting.
bb_unacked_bytes() {
  local dir=$1 d total=0 size log
  for d in "$dir"/jackerr.*; do
    test -d "$d" && test ! -e "$d/acked" || continue
    size=$(du -sb "$d" 2>/dev/null | cut -f1) || size=0
    total=$((total + ${size:-0}))
  done
  for log in "$dir/jackd.log" "$dir/fallback-jackd.log"; do
    test -f "$log" && total=$((total + $(stat -c %s "$log")))
  done
  echo "$total"
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

# Replaces jackd $6 with one preloading library $2, its rings and logs in
# directory $1, readiness checked with snapshot tool $3, refused past $4
# bytes of unarchived evidence; $5 names it in the log. The caller has
# checked the library and that $6 is the stock jackd without it.
bb_replace_jack() {
  local bb_dir=$1 lib=$2 snapshot=$3 limit=$4 bb_label=$5 pid=$6
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
  local bb_inventory_old bb_task_count bb_pcm_list bb_irq_old ambiguous unacked
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
  unacked=$(bb_unacked_bytes "$dir")
  if ((unacked > limit)); then
    say "$bb_label not started: $unacked bytes of evidence in $dir not yet archived by the host (limit $limit; nothing deleted)" >> "$logs/rc2fx.log"
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
  if bb_replace_jack "$attempt" "$lib" "$snapshot" "$recorder_evidence_limit" 'JACK recorder' "$pid"; then
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

# Runs as the launcher's child in place of the stock executable, so it must
# end in an exec of it: on any failure the stock mixer runs unchanged.
launch() {
  local fallback="exec $stock_mixer"
  if test $# -gt 0; then fallback+=$(printf ' %q' "$@"); fi
  trap "$fallback" EXIT
  mkdir -p -m 700 "$boot" "$data" "$logs"
  # Records that the shim ran for this start, whatever it decides.
  uptime_s > "$boot/seen"
  local why= selected old n
  if test -e /opt/rc2fx/disabled; then why='autostart is disabled'
  elif test -e /usr/bin/first-boot; then why='first boot after a firmware update'
  elif test -e "$boot/stock"; then why="$(cat "$boot/stock")"
  elif ! (cd "$home" && sha256sum -c SHA256SUMS > /dev/null 2>&1); then why="binaries in $home fail their checksums"
  elif ! selected=$("$home/rc2fx-firmware" check-device "$home/firmware-profiles.json" "$stock_mixer" /usr/bin/audio-start.sh 2>> "$logs/rc2fx.log"); then why='this firmware is not a supported profile'
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
  for ((n=0; n<50; n++)); do old=$(supervisor) || break; sleep 0.1; done
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
  # The mixer runs as this shell's child so that its exit status or signal
  # gets logged: the launcher records neither. No taskset: stock starts
  # unpinned and pins its own threads. RC2FX's diagnostics go to stderr.
  trap - EXIT
  relax_rt_throttle >> "$logs/rc2fx.log"
  jack_recorder || true
  # Explicit RAM-only measurement switch. Never inherit a caller's probe flag;
  # removal takes effect at the next mixer start, reboot clears the marker.
  local client_timing=0 gauges=0
  test -f /run/rc2fx-client-timing && client_timing=1
  test -f /run/rc2fx-gauges && gauges=1
  env RC2FX_CLIENT_TIMING="$client_timing" RC2FX_GAUGES="$gauges" LD_AUDIT="$home/librc2fx_native.so" \
    RC2FX_NATIVE_PROBE_REPORT_SELECTED_ONLY=1 \
    RC2FX_FIRMWARE_PROFILE="$profile_id" RC2FX_FIRMWARE_PROFILES="$home/firmware-profiles.json" \
    RC2FX_NATIVE_PROBE_PLAN="$run/plan.json" RC2FX_USB_SOCKET="$run/usb.sock" \
    RC2FX_NATIVE_PROBE_CONTROL_SOCKET="$run/control.sock" RC2FX_NATIVE_PROBE_REPORT="$run/audit.json" \
    RC2FX_NATIVE_PROBE_EVENTS="$logs/audio-events.log" \
    "$stock_mixer" "$@" 2>> "$run/mixer.log" &
  local mixer=$!
  printf '%s\n' "$mixer" > "$run/native.pid"
  setsid env RC2FX_DATA="$data" RC2FX_LOGS="$logs" \
    RC2FX_FIRMWARE_PROFILE="$profile_id" RC2FX_STOCK_SHOW="$show" \
    RC2FX_MIXER="$mixer $(start_time "$mixer")" \
    /bin/bash "$home/rc2fx.sh" supervise < /dev/null >> "$logs/rc2fx.log" 2>&1 &
  printf '%s\n' "$!" > "$run/supervisor.pid"
  say "RC2FX mixer started (PID $mixer) from $home under the launcher" >> "$logs/rc2fx.log"
  # Whoever ends this shell ends the mixer.
  trap 'kill -TERM "$mixer" 2>/dev/null' TERM INT HUP
  local status=0
  wait "$mixer" || status=$?
  while alive "$mixer"; do wait "$mixer" || status=$?; done
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

# RØDE pins about fifteen SCHED_OTHER threads of the mixer process to CPU 0
# only (its main thread, HIDThread, UartRunThread, JUCE Timer, SysMonThread
# and others). CPU 0 is where the RR5 suppression worker runs, and the audio
# path waits on non-PI locks those threads hold: moving them to CPUs 1 and 3
# removed the fader-triggered callback stalls (device test, 2026-11-01).
# Only threads the mixer owns are touched, only SCHED_OTHER ones, and only
# those whose affinity is exactly CPU 0; RT threads and any other affinity are
# left alone. Races with a thread exiting are ignored.
relocate_housekeeping() {
  local pid=$1 proc=${RC2FX_PROC:-/proc} task tid comm moved='' names='' policy rest record text
  local affinity_line=$'(^|\n)[ \t]*Cpus_allowed_list:[ \t]+([^ \t\n]*)'
  local -a batch=() stat_fields=()
  for task in "$proc/$pid"/task/*/; do
    task=${task%/}
    tid=${task##*/}
    [[ "$tid" =~ ^[0-9]+$ ]] || continue
    IFS= read -r comm 2>/dev/null < "$task/comm" || continue
    case "$comm" in rc2fx*) continue;; esac
    # One read of the whole file: procfs regenerates it on every read, and a
    # per-line loop costs about 165 syscalls per thread against 3. The read
    # returns nonzero at EOF as well, so an open that failed shows as no text.
    text=
    IFS= read -r -d '' text 2>/dev/null < "$task/status" || true
    [[ "$text" =~ $affinity_line ]] || continue
    test "${BASH_REMATCH[2]}" = 0 || continue
    # Field 39 is the policy; strip up to the LAST ") " so a comm with spaces
    # or a ')' does not shift the fields.
    IFS= read -r record 2>/dev/null < "$task/stat" || continue
    rest=${record##*) }
    read -r -a stat_fields <<< "$rest"
    policy=${stat_fields[38]:-}
    test "$policy" = 0 || continue
    taskset -p 0xa "$tid" >/dev/null 2>&1 || continue
    batch+=("$comm")
  done
  moved=${#batch[@]}
  if test "$moved" -gt 0; then
    names=$(printf '%s;' "${batch[@]}")
    say "Moved $moved RØDE housekeeping threads off CPU 0 (CPU 0 is the suppression worker's): ${names%;}" \
      >> "$logs/rc2fx.log"
  fi
  return 0
}

# Started by launch beside the mixer it watches. Starts the controller once
# the mixer opens its control socket, and ends the mixer so that the launcher
# restarts it (stock, when it records a reason in $boot/stock) when the
# controller exits, the mixer never becomes ready, or a stop is requested.
supervise() {
  # Globals, not locals: the EXIT trap runs cleanup after this function returns.
  controller_pid='' tick=0
  read -r native_pid native_start <<< "$RC2FX_MIXER"
  cleanup() {
    trap - EXIT INT TERM HUP
    set +e
    say 'Supervisor stopping'
    test -z "$controller_pid" || stop_owned "$controller_pid"
    {
      tail -n 30 "$logs/rc2fx.log"
      echo '--- controller'; tail -n 40 "$run/controller.log"
      echo '--- mixer'; tail -n 40 "$run/mixer.log" | redact
      echo '--- vendor'; tail -n 40 "$vendor_log" | redact
      echo '--- kernel'; dmesg | tail -n 40
    } > "$logs/last-exit.log" 2>&1
    # Only this supervisor's own record: the next launch may have written its.
    test "$(cat "$run/supervisor.pid" 2>/dev/null)" != "$$" || rm -f "$run/supervisor.pid"
    sync
  }
  mixer_alive() {
    local record rest
    local -a fields=()
    IFS= read -r record 2>/dev/null < "/proc/$native_pid/stat" || return 1
    rest=${record##*) }; read -r -a fields <<< "$rest"
    test "${fields[0]:-}" != Z && test "${fields[19]:-}" = "$native_start"
  }
  # Ends the mixer for the launcher to restart; $1 is the reason, recorded
  # for stock when $2 is `stock`.
  end_mixer() {
    say "$1"
    if test "${2:-}" = stock; then echo "$1" > "$boot/stock"; fi
    if ! mixer_alive; then return 0; fi
    # Only a restart into RC2FX asks to keep the gadget; stock rebuilds it.
    keep_usb=0
    if test "${2:-}" != stock && test -e "$run/keep-usb"; then keep_usb=1; fi
    if ! restart_mixer "$native_pid" ${controller_pid:+"$controller_pid"}; then
      say 'Leaving the mixer running without RC2FX control; stop it with rc2fx.sh stop'
      return 1
    fi
    controller_pid=
  }
  trap cleanup EXIT INT TERM HUP
  umask 077
  mkdir -p -m 700 "$boot"
  say "Supervising RC2FX mixer $native_pid from $home"
  local stock=() ui=() versions=() help age count
  test -r "$show" && stock=(--stock-config "$show")
  # Older bundles remain valid USB-only controllers. Never turn an optional
  # UI endpoint into an unknown-argument exit/audio fallback on upgrade.
  help=$("$home/rc2fxd" --help) || help=
  if grep -F -- '--ui-socket' >/dev/null <<< "$help"; then
    ui=(--ui-socket "$run/ui.sock")
  fi
  # What GET api/v1/version reports: the installed release manifest and the
  # profile launch() selected after check-device verified the stock mixer.
  # A bundle without the manifest reports its version as unknown.
  if grep -F -- '--release' >/dev/null <<< "$help"; then
    test -f "$home/mixer-release.json" && versions+=(--release "$home/mixer-release.json")
    test -n "$profile_id" && versions+=(--firmware-catalog "$home/firmware-profiles.json" --firmware-profile "$profile_id")
  fi
  # Developer SSH over USB 1, handled by this script's developer-ssh.
  if grep -F -- '--developer-ssh' >/dev/null <<< "$help"; then
    versions+=(--developer-ssh "$home/rc2fx.sh")
  fi
  while true; do
    if ! mixer_alive; then
      age=$(( $(uptime_s) - native_start / $(getconf CLK_TCK) ))
      say "Mixer exited after $age s (its launch shell logs the status)"
      # Two early exits in one boot: the launcher's next restart is stock.
      if test "$age" -lt 120; then
        echo "$(uptime_s)" >> "$boot/exits"
        count=$(( $(wc -l < "$boot/exits") ))
        if test "$count" -ge 2; then
          echo "mixer exited early $count times this boot" > "$boot/stock"
          say 'Falling back to stock audio for the rest of this boot'
        fi
      fi
      break
    fi
    if test -e "$run/stop"; then end_mixer 'Stop requested' || true; break; fi
    # A mixer that never opens its control socket would leave a blank screen.
    if test -z "$controller_pid" && test "$tick" -ge 60 && ! test -S "$run/control.sock"; then
      end_mixer 'Mixer not ready after 60 s; restoring stock audio' stock || true; break
    fi
    if test -z "$controller_pid" && test -S "$run/control.sock"; then
      "$home/rc2fxd" --data "$data" --native-socket "$run/control.sock" --usb-socket "$run/usb.sock" "${ui[@]}" "${stock[@]}" "${versions[@]}" \
        < /dev/null > "$run/controller.log" 2>&1 &
      controller_pid=$!
      say "Controller started (PID $controller_pid)"
    fi
    if test -n "$controller_pid" && ! alive "$controller_pid"; then
      status=0; wait "$controller_pid" || status=$?
      controller_pid=
      end_mixer "Controller exited with status $status; restoring stock audio" stock || true; break
    fi
    # RØDE's CPU0-pinned housekeeping threads hold locks the audio path waits
    # on; move them once the mixer has started its threads, then keep it so
    # (a restart brings fresh, pinned ones back).
    if mixer_alive && test "$tick" -ge 10 && test $(( (tick - 10) % 5 )) -eq 0; then
      relocate_housekeeping "$native_pid" || true
    fi
    tick=$((tick + 1))
    sleep 1
  done
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
  test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot \
    || { echo 'Refusing GUI experiment during first/production boot' >&2; return 1; }
  test "$(tr -d '\0' < /proc/device-tree/model)" = 'RODECaster Pro II' \
    || { echo 'Only the qualified Pro II is admitted' >&2; return 1; }
  printf '%s  %s\n' \
    aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca /usr/bin/rc_gui_juce \
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
  setsid /bin/bash "$home/rc2fx.sh" ui-watch "$dir" < /dev/null > "$dir/watch.log" 2>&1 &
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

# display-start.sh starts the GUI as soon as `pidof rc_audio_mixer` succeeds,
# but an RC2FX mixer needs about 9 s to open its control socket. A GUI started
# that early misses its meter setup (device 2026-11-01: a stereo channel showed
# only its left bar, and no master L/R, until the next GUI restart, while the
# launch state was identical). So wait until the mixer reports callbacks, but
# only when RC2FX audio is in charge this boot, and never longer than 30 s
# (RC2FX_GUI_WAIT_MAX, for tests).
# Never fails: audit.json may not exist yet when the socket does, and this
# runs under set -e -o pipefail.
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

# Developer SSH access without reflashing. RØDE's sshd already runs with
# key-only root login and an empty authorized_keys; this adds or removes ONE
# line, marked with $ssh_marker, and keeps every other byte of the file. A
# missing final newline is kept too, except that one is added before a new
# managed line. Removing the line stops new logins; it does not end sessions
# already open, and sshd is never restarted. `status` changes nothing. rc2fxd
# validates the key first and serializes calls; every check is repeated here.
# Exit status: 0 ok, 2 bad input, 3 unsafe path, 4 first boot after an update,
# 1 anything else.
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
    if [[ $line =~ ^(ssh-ed25519|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384|ecdsa-sha2-nistp521|ssh-rsa)\ [A-Za-z0-9+/=]{16,1024}$ ]]; then
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

# Opt-in flight recorder with exclusive ownership of an otherwise idle tracer.
# This device kernel routes uprobe traceoff to the global tracing buffer even
# when attached to an instance. Use the global buffer and restore its settings.
# No files or syscalls are added to an audio callback.
xrun_trace() {
  local base=/sys/kernel/debug/tracing instance=/sys/kernel/debug/tracing
  local dir=/run/rc2fx-xrun-trace action=${1:-} pid jack started lib offset sha token tid main_tid gui f filter profile
  shift || true
  case "$action" in
    prepare)
      test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || return 4
      touch /run/rc2fx-client-timing
      echo 'Client identity probe enabled for next audio start; restart explicitly'
      ;;
    prune-saved)
      # Only delete generated payloads whose exact bytes are backed up on host.
      for sha in "$@"; do [[ $sha =~ ^[0-9a-f]{64}$ ]] || return 2; done
      filter=" $* "
      for f in /run/rc2fx-xrun-trace.saved.*/trace.txt.gz /run/rc2fx-xrun-trace.saved.*/jack.snapshot; do
        test -f "$f" && test ! -L "$f" || continue
        sha=$(sha256sum "$f" | cut -d ' ' -f 1)
        case "$filter" in *" $sha "*) rm -- "$f";; esac
      done
      ;;
    unprepare)
      rm -f /run/rc2fx-client-timing
      ;;
    gauges)
      # Bench-only wrappers, independent of client timing. Like prepare, the
      # marker takes effect at the next mixer start and reboot clears it.
      case "${1:-}" in
        on)
          test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || return 4
          touch /run/rc2fx-gauges
          echo 'Bench gauges enabled for next audio start; restart explicitly'
          ;;
        off) rm -f /run/rc2fx-gauges /run/rc2fx-gauge-epoch ;;
        epoch)
          [[ ${2:-} =~ ^[0-9]+$ ]] || return 2
          (umask 077; printf '%s\n' "$2" > /run/rc2fx-gauge-epoch.tmp)
          mv /run/rc2fx-gauge-epoch.tmp /run/rc2fx-gauge-epoch
          ;;
        *) echo 'xrun-trace gauges on|off|epoch N' >&2; return 2 ;;
      esac
      ;;
    status)
      test -d "$dir" || { echo absent; return; }
      printf 'tracing_on='; cat "$instance/tracing_on"
      cat "$dir/identity"
      ;;
    calibrate)
      test -d "$dir" && test "$(cat "$instance/tracing_on")" = 1 || return 1
      /run/rc2fx-trace-clock > "$dir/clock-calibration"
      echo 0 > "$instance/tracing_on"
      echo calibration > "$dir/stopped-manually"
      ;;
    avoid-driver)
      test -d "$dir" && test $# -gt 0 || return 1
      pid=$(sed -n '1s/mixer=\([0-9]*\).*/\1/p' "$dir/identity")
      for tid in "$@"; do
        [[ $tid =~ ^[1-9][0-9]*$ ]] && test -d "/proc/$pid/task/$tid" || return 2
        test "$(cat "/proc/$tid/comm")" = USBOutJckThread || return 2
        filter=$(awk '/^Cpus_allowed:/ {gsub(",", "", $2); print $2}' "/proc/$tid/status")
        gui=$(printf '%x' "$((16#$filter & 8))")
        test "$gui" != 0 || return 2
        printf '%s %s %s %s\n' "$tid" "$filter" "$(awk '{print $22}' "/proc/$tid/stat")" "$pid" >> "$dir/saved-affinity"
        taskset -p "$gui" "$tid" >&2
      done
      jack=$(sed -n '1s/.*jack=\([0-9]*\) callback.*/\1/p' "$dir/identity")
      tid=$(awk -v pid="$jack" '$1==pid && $3=="FF" && $4==51 {print $2}' "$dir/arm-threads")
      [[ $tid =~ ^[1-9][0-9]*$ ]] && test -d "/proc/$jack/task/$tid" || return 2
      filter=$(awk '/^Cpus_allowed:/ {gsub(",", "", $2); print $2}' "/proc/$tid/status")
      test "$((16#$filter & 2))" != 0 || return 2
      printf '%s %s %s %s\n' "$tid" "$filter" "$(awk '{print $22}' "/proc/$tid/stat")" "$jack" >> "$dir/saved-affinity"
      taskset -p 2 "$tid" >&2
      echo 'driver_cpu=1 usb_output_cpu=3 main_callback_cpu=2' > "$dir/candidate-policy"
      ;;
    self-test)
      test $# -eq 1 && [[ $1 =~ ^[0-9]+$ ]] || return 2
      test -d "$dir" && test "$(cat "$instance/tracing_on")" = 1 || return 1
      test -x /run/rc2fx-trace-hog || return 1
      # The injector enrolls its own PID before running. Keep the same PID
      # selection path as the actual bench; never use event field filters.
      # Pin just the measured thread for the bounded test, restoring the exact
      # original mask even if the injector fails. Otherwise it can migrate away.
      tid=$(sed -n '1s/.*callback_tid=//p' "$dir/identity")
      (
        local allowed
        allowed=$(awk '/^Cpus_allowed_list:/ {print $2}' "/proc/$tid/status")
        trap 'taskset -pc "$allowed" "$tid" >/dev/null || true' EXIT
        printf 'RC2FX_SELFTEST tid=%s cpu=%s bounded_hog_ms=10\n' "$tid" "$1" > "$instance/trace_marker"
        taskset -pc "$1" "$tid" >/dev/null
        /run/rc2fx-trace-hog "$1" --trace-pid
      )
      ;;
    arm)
      test $# -ge 5 && test $# -le 6 || { echo 'arm requires fault-hook offset, SHA256, callback TID, arm token, main callback TID' >&2; return 2; }
      offset=$1 sha=$2 tid=$3 token=$4 main_tid=$5 profile=${6:-minimal}
      case "$profile" in minimal|deep) ;; *) return 2;; esac
      [[ $main_tid =~ ^[1-9][0-9]*$ && $offset =~ ^0x[0-9a-f]+$ && $sha =~ ^[0-9a-f]{64}$ && $tid =~ ^[1-9][0-9]*$ && $token =~ ^[1-9][0-9]{0,18}$ ]] || return 2
      test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || return 4
      test ! -e "$dir" || { echo 'Existing capture must be collected and explicitly cleared first' >&2; return 1; }
      ! grep -q 'rc2fx_xrun/' "$base/uprobe_events" || { echo 'Reserved probe names already exist; clear explicitly' >&2; return 1; }
      pid=$(cat /run/rc2fx/native.pid)
      test -d "/proc/$pid/task/$tid" || { echo 'Callback TID is no longer in the mixer' >&2; return 1; }
      read -r f jack started < /run/rc2fx-recorder/active
      test "$(readlink "/proc/$jack/exe")" = /usr/bin/jackd || return 1
      lib=$(awk '$NF ~ /librc2fx_jackerr.so$/ {print $NF; exit}' "/proc/$jack/maps")
      test -n "$lib" && test "$(sha256sum "$lib" | cut -d ' ' -f 1)" = "$sha" || { echo 'JACK recorder binary identity mismatch' >&2; return 1; }
      test "$(cat "$base/current_tracer")" = nop && test "$(cat "$base/events/enable")" = 0 || { echo 'Global tracer is in use; refusing to replace it' >&2; return 1; }
      test -z "$(ls -A "$base/instances")" || { echo 'Another tracing instance exists; refusing shared tracing' >&2; return 1; }
      mkdir -m 700 "$dir"
      for f in tracing_on buffer_size_kb options/overwrite; do
        cat "$base/$f" > "$dir/original-${f//\//-}"
      done
      sed -n 's/.*\[\([^]]*\)\].*/\1/p' "$base/trace_clock" > "$dir/original-clock"
      # Leave a failed setup stopped and recoverable via explicit clear.
      echo 0 > "$instance/tracing_on"
      echo nop > "$instance/current_tracer"
      echo mono_raw > "$instance/trace_clock"
      echo 4096 > "$instance/buffer_size_kb"
      echo 1 > "$instance/options/overwrite"
      echo > "$instance/trace"
      test -x /run/rc2fx-trace-clock || { echo 'Install diagnostic clock_probe first' >&2; return 1; }
      echo 1 > "$instance/tracing_on"
      /run/rc2fx-trace-clock > "$dir/clock-calibration"
      echo 0 > "$instance/tracing_on"
      cat "$instance/trace" > "$dir/clock-trace"
      echo > "$instance/trace"
      echo "p:rc2fx_xrun/fault $lib:$offset kind=%x0:u64 client=+0(%x1):string mono_ns=%x2:u64" >> "$base/uprobe_events"
      echo 'traceoff:1 if kind >= 1 && kind <= 10' > "$instance/events/rc2fx_xrun/fault/trigger"
      echo 1 > "$instance/events/rc2fx_xrun/fault/enable"
      cat "$instance/events/sched/sched_switch/filter" > "$dir/original-switch-filter"
      cat "$instance/events/sched/sched_waking/filter" > "$dir/original-waking-filter"
      test -z "$(cat "$instance/set_event_pid")" || { echo 'Existing event PID filter is in use' >&2; return 1; }
      cat "$instance/set_event_pid" > "$dir/original-event-pids"
      echo 0 > "$instance/events/sched/sched_switch/filter"
      echo 0 > "$instance/events/sched/sched_waking/filter"
      filter=
      for gui in "$pid" "$jack" $(pgrep -x callme-t); do
        for f in /proc/"$gui"/task/*; do filter="$filter ${f##*/}"; done
      done
      echo "$filter" > "$instance/set_event_pid"
      echo "$filter" > "$dir/event-pids"
      echo "$profile" > "$dir/profile"
      for f in sched/sched_switch sched/sched_waking sched/sched_wakeup sched/sched_wakeup_new; do
        echo 1 > "$instance/events/$f/enable"
      done
      # Keep syscall probes on the deadline path. GUI and vendor pool threads
      # remain visible as scheduler competitors/wakers, without tracing every
      # syscall they execute. This bounds observer overhead on the audio CPUs.
      filter="common_pid == $tid"
      for f in /proc/"$jack"/task/*; do filter="$filter || common_pid == ${f##*/}"; done
      for gui in $(pgrep -x callme-t); do
        cp "/proc/$gui/maps" "$dir/jack-client-maps.$gui"
        for f in /proc/"$gui"/task/*; do filter="$filter || common_pid == ${f##*/}"; done
      done
      for f in sys_enter sys_exit; do
        cat "$instance/events/raw_syscalls/$f/filter" > "$dir/original-filter-$f"
        echo 0 > "$instance/events/raw_syscalls/$f/filter"
        if test "$profile" = deep; then echo 1 > "$instance/events/raw_syscalls/$f/enable"; fi
      done
      printf 'mixer=%s jack=%s callback_tid=%s\n' "$pid" "$jack" "$tid" > "$dir/identity"
      cat /proc/sys/kernel/random/boot_id >> "$dir/identity"
      cat "/proc/$pid/stat" "/proc/$jack/stat" >> "$dir/identity"
      cp /run/rc2fx/audit.json "$dir/arm-audit.json"
      cp "/proc/$pid/maps" "$dir/mixer-maps"
      cp /proc/interrupts "$dir/arm-interrupts"
      ps -eLo pid,tid,cls,rtprio,psr,comm > "$dir/arm-threads"
      cat "$instance/trace_clock" > "$dir/trace-clock"
      cat "$base/uprobe_events" > "$dir/probes"
      cat "$instance/events/rc2fx_xrun/fault/trigger" > "$dir/trigger"
      cat /proc/uptime > "$dir/armed-uptime"
      printf '%s\n' "$token" > "$dir/flight-token"
      (umask 077; printf '%s\n' "$token" > /run/rc2fx-flight-arm.tmp)
      mv /run/rc2fx-flight-arm.tmp /run/rc2fx-flight-arm
      sleep 2
      echo 1 > "$instance/tracing_on"
      echo 'Armed: exclusive rolling trace freezes on first JACK fault'
      ;;
    stop)
      test -d "$dir" || return 1
      echo 0 > "$instance/tracing_on"
      echo manual > "$dir/stopped-manually"
      ;;
    collect)
      test -d "$dir" && test "$(cat "$instance/tracing_on")" = 0 || { echo 'Trace is still recording; stop it explicitly before collection' >&2; return 1; }
      if test ! -e "$dir/trace.txt.gz"; then
      : > "$dir/buffer-stats"
      for f in "$instance"/per_cpu/cpu*/stats; do
        { echo "${f%/stats}"; cat "$f"; } >> "$dir/buffer-stats"
      done
      cp /run/rc2fx/audit.json "$dir/collected-audit.json"
      cat /proc/uptime > "$dir/collected-uptime"
      cp /proc/interrupts "$dir/collected-interrupts"
      ps -eLo pid,tid,cls,rtprio,psr,comm > "$dir/collected-threads"
      cat "$base/uprobe_profile" > "$dir/uprobe-profile"
      read -r f jack started < /run/rc2fx-recorder/active
      lib=$(cat /run/rc2fx/home)
      test -e "$dir/jack.snapshot" || "$lib/rc2fx_jackerr_snapshot" "$(cat "$f/jackerr-dir")" "$dir/jack.snapshot" >&2
      gzip -c "$instance/trace" > "$dir/trace.txt.gz.tmp"
      mv "$dir/trace.txt.gz.tmp" "$dir/trace.txt.gz"
      fi
      tar -czf - -C "$dir" .
      ;;
    clear)
      rm -f /run/rc2fx-flight-arm
      if test -d "$dir"; then
        pid=0
        if test -f "$dir/identity"; then pid=$(sed -n '1s/mixer=\([0-9]*\).*/\1/p' "$dir/identity"); fi
        if test -f "$dir/saved-affinity"; then
          while read -r tid filter token gui; do
            gui=${gui:-$pid}
            if test -d "/proc/$gui/task/$tid" && test "$(awk '{print $22}' "/proc/$tid/stat")" = "$token"; then taskset -p "$filter" "$tid" >&2; fi
          done < "$dir/saved-affinity"
        fi
        echo 0 > "$instance/tracing_on"
        echo 0 > "$instance/events/enable"
        if test -f "$dir/original-event-pids"; then echo > "$instance/set_event_pid"; fi
        if test -f "$instance/events/rc2fx_xrun/fault/trigger" && grep -q '^traceoff' "$instance/events/rc2fx_xrun/fault/trigger"; then
          echo '!traceoff' > "$instance/events/rc2fx_xrun/fault/trigger"
        fi
        for f in switch waking; do
          if test -f "$dir/original-$f-filter"; then
            filter=$(cat "$dir/original-$f-filter"); test "$filter" != none || filter=0
            echo "$filter" > "$base/events/sched/sched_$f/filter"
          fi
        done
        for f in sys_enter sys_exit; do
          if test -f "$dir/original-filter-$f"; then
            filter=$(cat "$dir/original-filter-$f")
            test "$filter" != none || filter=0
            echo "$filter" > "$base/events/raw_syscalls/$f/filter"
          fi
        done
        for f in tracing_on buffer_size_kb options/overwrite; do
          cat "$dir/original-${f//\//-}" > "$base/$f"
        done
        cat "$dir/original-clock" > "$base/trace_clock"
      fi
      for f in fault callback_enter callback_exit; do
        if grep -q "rc2fx_xrun/$f " "$base/uprobe_events"; then echo "-:rc2fx_xrun/$f" >> "$base/uprobe_events"; fi
      done
      if test -d "$dir"; then mv "$dir" "$dir.saved.$(date +%s).$$"; fi
      ;;
    *) echo 'xrun-trace prepare|gauges|arm|status|stop|collect|clear' >&2; return 2 ;;
  esac
}

case "${1:-}" in
  xrun-trace) shift; xrun_trace "$@" ;;
  arm-power-recovery) arm_power_recovery ;;
  gui-launch) shift; gui_launch "$@" ;;
  ui-stop) shift; ui_stop "$@" ;;
  ui-run) shift; test $# -eq 2 || exit 2; ui_preview "$@" keep ;;
  ui-preview) shift; ui_preview "$@" ;;
  ui-watch) shift; ui_watch "$@" ;;
  start) shift; start "$@" ;;
  restart) shift; start --force "$@" ;;
  stop) stop ;;
  status) status ;;
  logs) logs ;;
  launch) shift; launch "$@" ;;
  usb-quiesce) usb_quiesce ;;
  supervise) supervise ;;
  developer-ssh) shift; developer_ssh "$@" ;;
  *) sed -n '2,22s/^# \{0,1\}//p' "$0"; exit 2 ;;
esac
