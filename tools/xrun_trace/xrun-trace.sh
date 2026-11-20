#!/bin/bash
# Developer-only xrun flight recorder commands, piped over SSH by tools/xrun_trace/run.py.
# Kept out of production mixer/scripts/rc2fx.sh so mixers do not ship ftrace tooling.
set -euo pipefail

# Opt-in flight recorder with exclusive ownership of an otherwise idle tracer.
# This device kernel routes uprobe traceoff to the global tracing buffer even
# when attached to an instance. Use the global buffer and restore its settings.
# No files or syscalls are added to an audio callback.
xrun_trace() {
  local base=/sys/kernel/debug/tracing
  local dir=/run/rc2fx-xrun-trace action=${1:-} pid jack started lib offset sha token tid main_tid gui f filter profile
  shift || true
  case "$action" in
    prepare)
      test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || return 4
      touch /run/rc2fx-client-timing
      echo 'Client identity probe enabled for next audio start; restart explicitly'
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
      printf 'tracing_on='; cat "$base/tracing_on"
      cat "$dir/identity"
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
      test -d "$dir" && test "$(cat "$base/tracing_on")" = 1 || return 1
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
        printf 'RC2FX_SELFTEST tid=%s cpu=%s bounded_hog_ms=10\n' "$tid" "$1" > "$base/trace_marker"
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
      echo 0 > "$base/tracing_on"
      echo nop > "$base/current_tracer"
      echo mono_raw > "$base/trace_clock"
      echo 4096 > "$base/buffer_size_kb"
      echo 1 > "$base/options/overwrite"
      echo > "$base/trace"
      test -x /run/rc2fx-trace-clock || { echo 'Install diagnostic clock_probe first' >&2; return 1; }
      echo 1 > "$base/tracing_on"
      /run/rc2fx-trace-clock > "$dir/clock-calibration"
      echo 0 > "$base/tracing_on"
      cat "$base/trace" > "$dir/clock-trace"
      echo > "$base/trace"
      echo "p:rc2fx_xrun/fault $lib:$offset kind=%x0:u64 client=+0(%x1):string mono_ns=%x2:u64" >> "$base/uprobe_events"
      echo 'traceoff:1 if kind >= 1 && kind <= 10' > "$base/events/rc2fx_xrun/fault/trigger"
      echo 1 > "$base/events/rc2fx_xrun/fault/enable"
      cat "$base/events/sched/sched_switch/filter" > "$dir/original-switch-filter"
      cat "$base/events/sched/sched_waking/filter" > "$dir/original-waking-filter"
      test -z "$(cat "$base/set_event_pid")" || { echo 'Existing event PID filter is in use' >&2; return 1; }
      cat "$base/set_event_pid" > "$dir/original-event-pids"
      echo 0 > "$base/events/sched/sched_switch/filter"
      echo 0 > "$base/events/sched/sched_waking/filter"
      filter=
      for gui in "$pid" "$jack" $(pgrep -x callme-t); do
        for f in /proc/"$gui"/task/*; do filter="$filter ${f##*/}"; done
      done
      echo "$filter" > "$base/set_event_pid"
      echo "$filter" > "$dir/event-pids"
      echo "$profile" > "$dir/profile"
      for f in sched/sched_switch sched/sched_waking sched/sched_wakeup sched/sched_wakeup_new; do
        echo 1 > "$base/events/$f/enable"
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
        cat "$base/events/raw_syscalls/$f/filter" > "$dir/original-filter-$f"
        echo 0 > "$base/events/raw_syscalls/$f/filter"
        if test "$profile" = deep; then echo 1 > "$base/events/raw_syscalls/$f/enable"; fi
      done
      printf 'mixer=%s jack=%s callback_tid=%s\n' "$pid" "$jack" "$tid" > "$dir/identity"
      cat /proc/sys/kernel/random/boot_id >> "$dir/identity"
      cat "/proc/$pid/stat" "/proc/$jack/stat" >> "$dir/identity"
      cp /run/rc2fx/audit.json "$dir/arm-audit.json"
      cp "/proc/$pid/maps" "$dir/mixer-maps"
      cp /proc/interrupts "$dir/arm-interrupts"
      ps -eLo pid,tid,cls,rtprio,psr,comm > "$dir/arm-threads"
      cat "$base/trace_clock" > "$dir/trace-clock"
      cat "$base/uprobe_events" > "$dir/probes"
      cat "$base/events/rc2fx_xrun/fault/trigger" > "$dir/trigger"
      cat /proc/uptime > "$dir/armed-uptime"
      printf '%s\n' "$token" > "$dir/flight-token"
      (umask 077; printf '%s\n' "$token" > /run/rc2fx-flight-arm.tmp)
      mv /run/rc2fx-flight-arm.tmp /run/rc2fx-flight-arm
      sleep 2
      echo 1 > "$base/tracing_on"
      echo 'Trace armed: exclusive rolling trace freezes on first JACK fault'
      ;;
    stop)
      test -d "$dir" || return 1
      echo 0 > "$base/tracing_on"
      echo manual > "$dir/stopped-manually"
      ;;
    collect)
      test -d "$dir" && test "$(cat "$base/tracing_on")" = 0 || { echo 'Trace is still recording; stop it explicitly before collection' >&2; return 1; }
      if test ! -e "$dir/trace.txt.gz"; then
      : > "$dir/buffer-stats"
      for f in "$base"/per_cpu/cpu*/stats; do
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
      gzip -c "$base/trace" > "$dir/trace.txt.gz.tmp"
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
        echo 0 > "$base/tracing_on"
        echo 0 > "$base/events/enable"
        if test -f "$dir/original-event-pids"; then echo > "$base/set_event_pid"; fi
        if test -f "$base/events/rc2fx_xrun/fault/trigger" && grep -q '^traceoff' "$base/events/rc2fx_xrun/fault/trigger"; then
          echo '!traceoff' > "$base/events/rc2fx_xrun/fault/trigger"
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
    src-mode)
      case "${1:-}" in
        fast) rm -f /run/rc2fx-src-mode ;;
        verify|stock) echo "$1" > /run/rc2fx-src-mode ;;
        *) echo 'xrun-trace src-mode fast|verify|stock' >&2; return 2 ;;
      esac
      echo "Resampler mode $1 for next audio start; restart explicitly"
      ;;
    *) echo 'xrun-trace prepare|unprepare|gauges|src-mode|self-test|arm|status|avoid-driver|stop|collect|clear' >&2; return 2 ;;
  esac
}


case "${1:-}" in
  xrun-trace) shift; xrun_trace "$@" ;;
  *) xrun_trace "$@" ;;
esac
