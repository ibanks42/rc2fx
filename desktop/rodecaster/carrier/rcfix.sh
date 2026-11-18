#!/bin/bash
# rcfix carrier for the RODECaster Pro II and Duo.
#
# Started once per boot from /etc/rc5.d, before RODE's audio service. It
# grows partition 1 of the eMMC into the unused gap behind it, so RODE's
# updater formats that slot with 4 KB blocks from then on; it reports what it
# did in RCFIX.TXT on the USB update drive and then deletes itself.
#
# Phase A (blocking, at boot): check the table and fix it, or hop to the
# intact slot 2 first when this copy runs from slot 1.
# Phase B (detached): wait for RODE's update drive, write the marker, remove
# every file this script put on the system. When the app asked for it
# (RCFIX_USB_REPORT=1, set by the app when it builds the script), phase B
# first offers the marker on a small disk of its own in the empty disk slot
# of RODE's normal-mode USB gadget; the app reads it there, writes RCFIX.ACK
# onto it and ejects it, which tells this script to finish. Without that
# acknowledgement phase B takes its disk back and writes the marker to the
# update drive as before.
#
# The flash that carried this file may have zero-filled its tail. Everything
# below is inside functions and the call to rcfix_main is the second-to-last
# line, so a copy cut short never runs; a copy cut after that call fails its
# own check (the last line holds an end token and the SHA-256 of all lines
# before it) and exits without doing anything. Three copies are installed.

RCFIX_VERSION=4

# Overridable so a test can point every path and tool somewhere harmless.
RCFIX_PATH=${RCFIX_PATH:-/usr/sbin:/usr/bin:/sbin:/bin}
RCFIX_SYSROOT=${RCFIX_SYSROOT:-/}
RCFIX_DISK=${RCFIX_DISK:-/dev/mmcblk1}
RCFIX_CMDLINE=${RCFIX_CMDLINE:-/proc/cmdline}
RCFIX_MOUNTS=${RCFIX_MOUNTS:-/proc/mounts}
RCFIX_BOOT_ID=${RCFIX_BOOT_ID:-/proc/sys/kernel/random/boot_id}
RCFIX_KMSG=${RCFIX_KMSG:-/dev/kmsg}
RCFIX_IMAGE=${RCFIX_IMAGE:-/Application/update-staging/usb-update.img}
RCFIX_MNT_P2=${RCFIX_MNT_P2:-/run/rcfix-p2}
RCFIX_MNT_IMG=${RCFIX_MNT_IMG:-/run/rcfix-img}
RCFIX_POLL_SECS=${RCFIX_POLL_SECS:-2}
RCFIX_QUIET_POLLS=${RCFIX_QUIET_POLLS:-5}
RCFIX_FALLBACK_POLLS=${RCFIX_FALLBACK_POLLS:-60}
RCFIX_CONFIRM_POLLS=${RCFIX_CONFIRM_POLLS:-30}
RCFIX_WAIT_POLLS=${RCFIX_WAIT_POLLS:-300}
RCFIX_MAX_BOOTS=${RCFIX_MAX_BOOTS:-5}
RCFIX_REBOOT_WAIT=${RCFIX_REBOOT_WAIT:-120}
RCFIX_FOREGROUND=${RCFIX_FOREGROUND:-0}
# The USB report (see phase B). The app sets the default to 1 only when it
# can read the report disk, write its acknowledgement and eject it; every
# other value leaves it off.
RCFIX_USB_REPORT=${RCFIX_USB_REPORT:-0}
RCFIX_GADGET=${RCFIX_GADGET:-/sys/kernel/config/usb_gadget/g124}
RCFIX_ACK_IMG=${RCFIX_ACK_IMG:-/run/rcfix-ack.img}
RCFIX_MNT_ACK=${RCFIX_MNT_ACK:-/run/rcfix-ack}
# Monotonic seconds since boot, as the first field of the file.
RCFIX_UPTIME=${RCFIX_UPTIME:-/proc/uptime}
# The gadget must have been bound this long, without a break, before the
# report disk goes into its slot (each time). On the owner's mixer RODE's
# program emptied the slot once, 2.4 to 3.0 s after binding the gadget, and
# left it alone afterwards (measured 2026-11-18); a disk emptied anyway is
# offered again.
RCFIX_BOUND_SECS=${RCFIX_BOUND_SECS:-6}
# At most this many attaches, all within this many seconds of the start of
# the USB report. The app's wait after the flash is sized from these.
RCFIX_ATTACH_TRIES=${RCFIX_ATTACH_TRIES:-3}
RCFIX_REPORT_SECS=${RCFIX_REPORT_SECS:-120}
RCFIX_ACK_POLL_SECS=${RCFIX_ACK_POLL_SECS:-1}
# Evidence for development: the app's development builds set the default to
# 1 when asked to (see rcfix_debug_start); every other value leaves it off.
RCFIX_DEBUG=${RCFIX_DEBUG:-0}
RCFIX_DEBUG_DIR=${RCFIX_DEBUG_DIR:-/Application/update-staging/rcfix-debug}
RCFIX_TRACE_SECS=${RCFIX_TRACE_SECS:-0.2}

# Paths relative to a system root; the Go side lists the same ones.
REL_INIT=etc/init.d
REL_RC=etc/rc5.d
REL_STATE=var/lib/rcfix
NAMES="a b c"
MARKER=RCFIX.TXT
ACK=RCFIX.ACK
EOF_TOKEN="# rcfix-eof sha256="

# RODE's factory layout in 512-byte sectors; the fix gives partition 1 the
# size of partition 2. Partition 4 fills the rest of the eMMC.
P1_START=8192
P1_FACTORY=1039458
P1_GROWN=1193984
P2_START=2000896
P2_SIZE=1193984
P3_START=3223552
P3_SIZE=1710080
P4_START=4941824
PART_TYPE=83
GAP_START=1047650
GAP_COUNT=154526

# Set by rcfix_setup and the phases.
PART=
STATE_DIR=
SELF=
RESULT=
REASON=
HOPPED=0
SECTOR0_HEX=
DEBUG_FILE=
DEBUG_NAME=
TRACE_PID=

rcfix_log() {
	local msg="rcfix[$$]: $*"
	if [ -n "$STATE_DIR" ] && [ -d "$STATE_DIR" ]; then
		printf '%s\n' "$msg" 2>/dev/null >>"$STATE_DIR/log"
	fi
	if [ -n "$DEBUG_FILE" ]; then
		printf '%s %s\n' "$(rcfix_uptime)" "$msg" 2>/dev/null >>"$DEBUG_FILE"
	fi
	if [ -n "$RCFIX_KMSG" ] && [ -w "$RCFIX_KMSG" ]; then
		printf '<5>%s\n' "$msg" 2>/dev/null >>"$RCFIX_KMSG"
	fi
	return 0
}

# Every command that changes the disk, the U-Boot environment, a mount or
# the power state goes through here, so it is logged and a test can stub it.
rcfix_act() {
	rcfix_log "run: $*"
	"$@"
}

rcfix_setup() {
	case $RCFIX_SYSROOT in
	*/) ;;
	*) RCFIX_SYSROOT=$RCFIX_SYSROOT/ ;;
	esac
	PART=${RCFIX_DISK}p
	STATE_DIR=$RCFIX_SYSROOT$REL_STATE
}

# One printable line, no '|' (the marker joins table lines with it).
rcfix_clean() {
	printf '%s' "$1" | tr -c '[:print:]' ' ' | tr '|' '/' | cut -c1-300
}

# True when $1 is a complete, unmodified copy of this script: no NUL byte, a
# final newline, a last line with the end token and the SHA-256 of all lines
# before it.
rcfix_self_ok() {
	local self=$1 size lastbyte last want got
	[ -f "$self" ] && [ -r "$self" ] || return 1
	size=$(stat -L -c %s "$self" 2>/dev/null) || return 1
	case $size in '' | *[!0-9]*) return 1 ;; esac
	[ "$size" -gt 200 ] || return 1
	# shellcheck disable=SC2094 # both only read it
	tr -d '\000' <"$self" | cmp -s - "$self" || return 1
	lastbyte=$(dd if="$self" bs=1 skip=$((size - 1)) count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
	[ "$lastbyte" = 0a ] || return 1
	last=$(sed -n '$p' "$self") || return 1
	case $last in
	"$EOF_TOKEN"*) ;;
	*) return 1 ;;
	esac
	want=${last#"$EOF_TOKEN"}
	[ "${#want}" -eq 64 ] || return 1
	case $want in *[!0-9a-f]*) return 1 ;; esac
	got=$(sed '$d' "$self" | sha256sum) || return 1
	got=${got%% *}
	[ "$got" = "$want" ]
}

rcfix_boot_id() {
	local id
	id=$(cat "$RCFIX_BOOT_ID" 2>/dev/null) || id=
	printf '%s\n' "${id:-unknown}"
}

# The rc5.d links run one after another, so the first intact copy takes the
# lock and the others find it held for this boot and stop. A lock from an
# earlier boot is stale.
rcfix_take_lock() {
	local now held
	now=$(rcfix_boot_id)
	if [ -f "$STATE_DIR/lock" ]; then
		held=$(cat "$STATE_DIR/lock" 2>/dev/null) || held=
		[ "$held" = "$now" ] && return 1
	fi
	printf '%s\n' "$now" 2>/dev/null >"$STATE_DIR/lock" || return 1
	return 0
}

rcfix_count_boot() {
	local b
	b=$(cat "$STATE_DIR/boots" 2>/dev/null) || b=0
	case $b in '' | *[!0-9]*) b=0 ;; esac
	[ "${#b}" -le 4 ] || b=0
	b=$((10#$b + 1))
	printf '%s\n' "$b" 2>/dev/null >"$STATE_DIR/boots"
	BOOTS=$b
}

rcfix_read_boots() {
	local b
	b=$(cat "$STATE_DIR/boots" 2>/dev/null) || b=0
	case $b in '' | *[!0-9]*) b=0 ;; esac
	[ "${#b}" -le 4 ] || b=0
	printf '%s\n' "$((10#$b))"
}

# 1 or 2 from root= on the kernel command line, 0 when it names neither.
rcfix_slot() {
	local cmdline
	cmdline=$(cat "$RCFIX_CMDLINE" 2>/dev/null) || cmdline=
	case " $cmdline " in
	*" root=${PART}1 "*) echo 1 ;;
	*" root=${PART}2 "*) echo 2 ;;
	*) echo 0 ;;
	esac
}

# True when $1 is a mounted device or a mount point. An unreadable mount
# table counts as mounted, so nothing is written on a guess.
rcfix_is_mounted() {
	local dev mp rest
	[ -r "$RCFIX_MOUNTS" ] || return 0
	while read -r dev mp rest; do
		if [ "$dev" = "$1" ] || [ "$mp" = "$1" ]; then
			return 0
		fi
	done <"$RCFIX_MOUNTS"
	return 1
}

rcfix_why() {
	[ -n "$TABLE_WHY" ] || TABLE_WHY=$1
}

rcfix_field() {
	[ "$2" = "$3" ] && return 0
	rcfix_why "$1 is $2, expected $3"
	return 1
}

# Reads `sfdisk -d`. Sets TABLE_LINES (the raw partition lines), T_START,
# T_SIZE and T_TYPE by partition number, TABLE_CLASS (factory, grown or
# other) and TABLE_WHY (why it is other). Fails only when sfdisk fails.
rcfix_read_table() {
	local dump line rest k label=0 re n
	TABLE_LINES=()
	T_START=()
	T_SIZE=()
	T_TYPE=()
	TABLE_CLASS=other
	TABLE_WHY=
	if ! dump=$(sfdisk -d "$RCFIX_DISK" 2>/dev/null); then
		TABLE_WHY="sfdisk could not read the partition table"
		return 1
	fi
	re='^([^ ]+) : start= *([0-9]+), size= *([0-9]+), type=([0-9a-fA-F]+)$'
	# Split in the shell: a here-string may need a temporary file.
	rest=$dump$'\n'
	while [ -n "$rest" ]; do
		line=${rest%%$'\n'*}
		rest=${rest#*$'\n'}
		case $line in
		'label: dos') label=1 ;;
		'sector-size: '*)
			[ "$line" = 'sector-size: 512' ] || rcfix_why "the sector size is not 512 bytes"
			;;
		*' : '*)
			TABLE_LINES+=("$(rcfix_clean "$line")")
			k=${#TABLE_LINES[@]}
			if [[ $line =~ $re ]] && [ "${BASH_REMATCH[1]}" = "$PART$k" ]; then
				T_START[k]=${BASH_REMATCH[2]}
				T_SIZE[k]=${BASH_REMATCH[3]}
				T_TYPE[k]=${BASH_REMATCH[4],,}
			else
				rcfix_why "partition line $k is not in the expected form"
			fi
			;;
		esac
	done
	[ "$label" = 1 ] || rcfix_why "the disk does not have an MBR (dos) partition table"
	[ "${#TABLE_LINES[@]}" = 4 ] || rcfix_why "the table has ${#TABLE_LINES[@]} partitions, expected 4"
	[ -z "$TABLE_WHY" ] || return 0
	for n in 1 2 3 4; do
		rcfix_field "partition $n type" "${T_TYPE[n]}" "$PART_TYPE"
	done
	rcfix_field "partition 1 start" "${T_START[1]}" "$P1_START"
	rcfix_field "partition 2 start" "${T_START[2]}" "$P2_START"
	rcfix_field "partition 2 size" "${T_SIZE[2]}" "$P2_SIZE"
	rcfix_field "partition 3 start" "${T_START[3]}" "$P3_START"
	rcfix_field "partition 3 size" "${T_SIZE[3]}" "$P3_SIZE"
	rcfix_field "partition 4 start" "${T_START[4]}" "$P4_START"
	[ "${T_SIZE[4]}" != 0 ] || rcfix_why "partition 4 is empty"
	[ -z "$TABLE_WHY" ] || return 0
	case ${T_SIZE[1]} in
	"$P1_FACTORY") TABLE_CLASS=factory ;;
	"$P1_GROWN") TABLE_CLASS=grown ;;
	*) rcfix_why "partition 1 has ${T_SIZE[1]} sectors, expected $P1_FACTORY or $P1_GROWN" ;;
	esac
	return 0
}

rcfix_table_joined() {
	local IFS='|'
	printf '%s' "${TABLE_LINES[*]}"
}

# Saves the phase A result where phase B, maybe on a later boot, finds it.
rcfix_record() {
	RESULT=$1
	REASON=$(rcfix_clean "$2")
	case $RESULT in
	unrecognized | failed) [ -n "$REASON" ] || REASON="no reason was recorded" ;;
	esac
	rcfix_log "result: $RESULT${REASON:+ ($REASON)}"
	if printf 'result=%s\nhopped=%s\nreason=%s\nsector0_before=%s\nend=1\n' \
		"$RESULT" "$HOPPED" "$REASON" "$SECTOR0_HEX" 2>/dev/null >"$STATE_DIR/result.new" &&
		sync && mv -f "$STATE_DIR/result.new" "$STATE_DIR/result"; then
		sync
		return 0
	fi
	rcfix_log "could not save the result"
	return 1
}

rcfix_load_result() {
	local f=$STATE_DIR/result line k v end=0
	RESULT=
	REASON=
	SECTOR0_HEX=
	[ -f "$f" ] || return 1
	while IFS= read -r line; do
		k=${line%%=*}
		v=${line#*=}
		case $k in
		result) RESULT=$v ;;
		hopped) HOPPED=$v ;;
		reason) REASON=$v ;;
		sector0_before) SECTOR0_HEX=$v ;;
		end) [ "$v" = 1 ] && end=1 ;;
		esac
	done <"$f"
	[ "$end" = 1 ] || return 1
	case $HOPPED in 0 | 1) ;; *) return 1 ;; esac
	case $RESULT in
	fixed | already-fixed | unrecognized | failed) return 0 ;;
	esac
	return 1
}

# True when the sectors partition 1 grows into hold only zeros.
rcfix_gap_is_zero() {
	local out
	out=$(
		dd if="$RCFIX_DISK" bs=512 skip="$GAP_START" count="$GAP_COUNT" 2>/dev/null |
			tr -d '\000' | od -An -N1 -tx1
		printf 'dd=%s\n' "${PIPESTATUS[0]}"
	)
	case $out in
	dd=0) return 0 ;;
	dd=*) GAP_WHY="could not read the space partition 1 would grow into" ;;
	*) GAP_WHY="the space partition 1 would grow into is not empty" ;;
	esac
	return 1
}

rcfix_save_sector0() {
	local f=$STATE_DIR/sector0.bin size hex
	rm -f "$f" || return 1
	dd if="$RCFIX_DISK" of="$f" bs=512 count=1 2>/dev/null || return 1
	size=$(stat -c %s "$f" 2>/dev/null) || return 1
	[ "$size" = 512 ] || return 1
	sync
	hex=$(od -An -v -tx1 "$f" | tr -d ' \n') || return 1
	[ "${#hex}" -eq 1024 ] || return 1
	case $hex in *55aa) ;; *) return 1 ;; esac
	SECTOR0_HEX=$hex
}

rcfix_le32() {
	printf '%02x%02x%02x%02x' $(($1 & 255)) $((($1 >> 8) & 255)) $((($1 >> 16) & 255)) $((($1 >> 24) & 255))
}

# After sfdisk: only entry 1 (bytes 446..461) may differ from the saved
# sector 0, and its start, count and type must be the grown ones.
rcfix_sector0_changes_ok() {
	local after=$STATE_DIR/sector0.after diffs=$STATE_DIR/cmp off rest rc hex
	S0_WHY=
	rm -f "$after" "$diffs"
	if ! dd if="$RCFIX_DISK" of="$after" bs=512 count=1 2>/dev/null; then
		S0_WHY="could not read sector 0 back"
		return 1
	fi
	cmp -l "$STATE_DIR/sector0.bin" "$after" 2>/dev/null >"$diffs"
	rc=$?
	case $rc in
	0 | 1) ;;
	*)
		S0_WHY="could not compare sector 0 with the saved copy"
		return 1
		;;
	esac
	while read -r off rest; do
		case $off in '' | *[!0-9]*)
			S0_WHY="could not compare sector 0 with the saved copy"
			return 1
			;;
		esac
		# cmp counts bytes from 1.
		if [ "$off" -lt 447 ] || [ "$off" -gt 462 ]; then
			S0_WHY="sfdisk changed byte $((off - 1)) of sector 0, outside partition entry 1"
			return 1
		fi
	done <"$diffs"
	hex=$(od -An -v -tx1 "$after" | tr -d ' \n')
	if [ "${hex:900:2}" != "$PART_TYPE" ] ||
		[ "${hex:908:16}" != "$(rcfix_le32 "$P1_START")$(rcfix_le32 "$P1_GROWN")" ]; then
		S0_WHY="partition entry 1 does not hold the expected start and size"
		return 1
	fi
	rm -f "$after" "$diffs"
	return 0
}

# Puts the saved sector 0 back (and the kernel's idea of partition 1 when
# resizepart already ran), then records the failure.
rcfix_restore() {
	local kernel=$1 why=$2 got
	if ! rcfix_act dd if="$STATE_DIR/sector0.bin" of="$RCFIX_DISK" bs=512 count=1 conv=notrunc,fsync 2>>"$STATE_DIR/log"; then
		rcfix_record failed "$why; writing the saved sector 0 back also failed"
		return
	fi
	sync
	if ! dd if="$RCFIX_DISK" bs=512 count=1 2>/dev/null | cmp -s - "$STATE_DIR/sector0.bin"; then
		rcfix_record failed "$why; sector 0 does not match the saved copy after writing it back"
		return
	fi
	if [ "$kernel" = 1 ]; then
		rcfix_act resizepart "$RCFIX_DISK" 1 "$P1_FACTORY" >>"$STATE_DIR/log" 2>&1
		got=$(blockdev --getsz "${PART}1" 2>/dev/null)
		if [ "$got" != "$P1_FACTORY" ]; then
			rcfix_record failed "$why; the table was put back but the running system still sees the larger partition 1 until it restarts"
			return
		fi
	fi
	rcfix_record failed "$why; the partition table was put back as it was"
}

# Factory table, running from slot 2: grow partition 1 with the commands
# proven on a mixer.
rcfix_resize() {
	local rc p4 why='' got
	if rcfix_is_mounted "${PART}1"; then
		rcfix_record failed "partition 1 is mounted, so it was not changed"
		return
	fi
	if ! rcfix_gap_is_zero; then
		rcfix_record failed "$GAP_WHY"
		return
	fi
	if ! rcfix_save_sector0; then
		SECTOR0_HEX=
		rcfix_record failed "could not save a copy of sector 0, so nothing was changed"
		return
	fi
	p4=${T_SIZE[4]}
	printf '%s,%s\n' "$P1_START" "$P1_GROWN" |
		rcfix_act sfdisk --no-reread --no-tell-kernel -N 1 "$RCFIX_DISK" >>"$STATE_DIR/log" 2>&1
	rc=$?
	sync
	if [ "$rc" != 0 ]; then
		why="sfdisk failed with status $rc"
	elif ! rcfix_read_table; then
		why="the table could not be read back"
	elif [ "$TABLE_CLASS" != grown ]; then
		why="the table did not read back as expected: ${TABLE_WHY:-partition 1 has ${T_SIZE[1]} sectors}"
	elif [ "${T_SIZE[4]}" != "$p4" ]; then
		why="partition 4 changed size"
	elif ! rcfix_sector0_changes_ok; then
		why=$S0_WHY
	fi
	if [ -n "$why" ]; then
		rcfix_restore 0 "$why"
		return
	fi
	if ! rcfix_act resizepart "$RCFIX_DISK" 1 "$P1_GROWN" >>"$STATE_DIR/log" 2>&1; then
		rcfix_restore 1 "resizepart failed"
		return
	fi
	got=$(blockdev --getsz "${PART}1" 2>/dev/null)
	if [ "$got" != "$P1_GROWN" ]; then
		rcfix_restore 1 "the kernel reports partition 1 as ${got:-unknown} sectors after resizepart"
		return
	fi
	sync
	rcfix_record fixed ""
}

# Copies this (verified) script to the three names under root $1 with their
# links, plus the state file that says the copy arrived by a hop.
rcfix_install_into() {
	local root=$1 n f l
	for n in $NAMES; do
		f=$root$REL_INIT/rcfix-$n
		l=$root$REL_RC/S02rcfix-$n
		rm -f "$f.new" || return 1
		cp "$SELF" "$f.new" || return 1
		chmod 0755 "$f.new" || return 1
		cmp -s "$SELF" "$f.new" || return 1
		mv -f "$f.new" "$f" || return 1
		rm -f "$l" || return 1
		ln -s "../init.d/rcfix-$n" "$l" || return 1
	done
	mkdir -p "$root$REL_STATE" || return 1
	printf 'from=1\n' 2>/dev/null >"$root$REL_STATE/hopped" || return 1
	sync
	for n in $NAMES; do
		[ -L "$root$REL_RC/S02rcfix-$n" ] || return 1
		rcfix_self_ok "$root$REL_RC/S02rcfix-$n" || return 1
	done
	[ -f "$root$REL_STATE/hopped" ]
}

# Removes every file and link this script installs under root $1, and its
# state directory.
rcfix_uninstall() {
	local root=$1 n bad=0
	for n in $NAMES; do
		rm -f "$root$REL_RC/S02rcfix-$n" "$root$REL_INIT/rcfix-$n" "$root$REL_INIT/rcfix-$n.new" || bad=1
	done
	rm -rf "$root$REL_STATE" || bad=1
	sync
	return "$bad"
}

rcfix_env_set() {
	rcfix_act fw_setenv "$@" >>"$STATE_DIR/log" 2>&1
}

# $2 is the variable's earlier `name=value` line, empty when it was unset.
rcfix_env_put() {
	if [ -n "$2" ]; then
		rcfix_env_set "$1" "${2#*=}"
	else
		rcfix_env_set "$1"
	fi
}

# Points U-Boot at slot 2 exactly as RODE's updater does, arms the one-shot
# power-on bypass and reads all three back. On any mismatch puts the old
# values back.
rcfix_env_hop() {
	local alt="setenv devplist 1; run bootcmd"
	ENV_WHY=
	if ! rcfix_env_set devplist 2 || ! rcfix_env_set altbootcmd "$alt" ||
		! rcfix_env_set reboot_pwr_button_bypass 1; then
		ENV_WHY="fw_setenv failed"
	elif [ "$(fw_printenv devplist 2>/dev/null)" != "devplist=2" ] ||
		[ "$(fw_printenv altbootcmd 2>/dev/null)" != "altbootcmd=$alt" ] ||
		[ "$(fw_printenv reboot_pwr_button_bypass 2>/dev/null)" != "reboot_pwr_button_bypass=1" ]; then
		ENV_WHY="the U-Boot variables did not read back as written"
	fi
	[ -z "$ENV_WHY" ] && return 0
	rcfix_env_put devplist "$OLD_DEV"
	rcfix_env_put altbootcmd "$OLD_ALT"
	rcfix_env_put reboot_pwr_button_bypass "$OLD_BYP"
	return 1
}

# Factory table, running from slot 1 (the 1 KB slot this flash may have
# damaged): nothing here writes the partition table. Copy the script into
# slot 2, the previous intact system, switch U-Boot to it and restart.
rcfix_hop() {
	local mnt=$RCFIX_MNT_P2 p2=${PART}2 why='' installed=0
	if [ -e "$STATE_DIR/hop-attempted" ]; then
		rcfix_record failed "a switch to slot 2 was already tried and the mixer still runs from slot 1"
		return
	fi
	if ! printf '1\n' 2>/dev/null >"$STATE_DIR/hop-attempted"; then
		rcfix_record failed "could not write to this system, so it did not switch to slot 2"
		return
	fi
	sync
	if rcfix_is_mounted "$p2" || rcfix_is_mounted "$mnt"; then
		rcfix_record failed "slot 2 is already mounted, so it was not changed"
		return
	fi
	OLD_DEV=$(fw_printenv devplist 2>/dev/null) || OLD_DEV=
	if [ "$OLD_DEV" != devplist=1 ]; then
		rcfix_record failed "U-Boot's devplist is '${OLD_DEV#devplist=}', expected 1 while running from slot 1"
		return
	fi
	OLD_ALT=$(fw_printenv altbootcmd 2>/dev/null) || OLD_ALT=
	OLD_BYP=$(fw_printenv reboot_pwr_button_bypass 2>/dev/null) || OLD_BYP=
	if ! mkdir -p "$mnt"; then
		rcfix_record failed "could not create a mount point for slot 2"
		return
	fi
	if ! rcfix_act mount -t ext4 -o rw "$p2" "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rmdir "$mnt" 2>/dev/null
		rcfix_record failed "slot 2 could not be mounted"
		return
	fi
	if [ ! -d "$mnt/$REL_RC" ] || [ ! -d "$mnt/$REL_INIT" ]; then
		why="slot 2 has no /$REL_RC or /$REL_INIT"
	elif [ -e "$mnt/$REL_STATE" ]; then
		why="slot 2 already holds rcfix state"
	else
		installed=1
		if ! rcfix_install_into "$mnt/"; then
			why="copying rcfix to slot 2 failed"
		elif ! rcfix_env_hop; then
			why=$ENV_WHY
		fi
	fi
	if [ -n "$why" ]; then
		[ "$installed" = 0 ] || rcfix_uninstall "$mnt/"
		sync
		rcfix_act umount "$mnt" >>"$STATE_DIR/log" 2>&1 || rcfix_log "could not unmount slot 2"
		rmdir "$mnt" 2>/dev/null
		rcfix_record failed "$why"
		return
	fi
	sync
	if ! rcfix_act umount "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rcfix_log "could not unmount slot 2; the restart unmounts it"
	fi
	rmdir "$mnt" 2>/dev/null
	rcfix_log "switching to slot 2 and restarting"
	rcfix_uninstall "$RCFIX_SYSROOT" || rcfix_log "could not remove every rcfix file from slot 1"
	sync
	if ! rcfix_act reboot; then
		rcfix_log "reboot failed; slot 2 starts at the next power-on"
	fi
	# Hold the boot here until init stops this script for the restart.
	sleep "$RCFIX_REBOOT_WAIT"
	exit 0
}

rcfix_phase_a() {
	local slot
	SECTOR0_HEX=
	HOPPED=0
	[ -f "$STATE_DIR/hopped" ] && HOPPED=1
	slot=$(rcfix_slot)
	rcfix_log "phase A: slot $slot, hopped $HOPPED"
	if ! rcfix_read_table; then
		rcfix_record failed "$TABLE_WHY"
		return
	fi
	case $TABLE_CLASS in
	grown)
		rcfix_record already-fixed ""
		return
		;;
	other)
		rcfix_record unrecognized "$TABLE_WHY"
		return
		;;
	esac
	case $slot in
	2) rcfix_resize ;;
	1)
		if [ "$HOPPED" = 1 ]; then
			rcfix_record failed "arrived from slot 1 but is running from slot 1 again"
		else
			rcfix_hop
		fi
		;;
	*) rcfix_record failed "could not tell which system slot is running" ;;
	esac
}

rcfix_image_sig() {
	stat -c '%s %y %z' "$RCFIX_IMAGE" 2>/dev/null
}

rcfix_marker_text() {
	local slot p1 table
	slot=$(rcfix_slot)
	rcfix_read_table
	p1=${T_SIZE[1]:-}
	table=$(rcfix_table_joined)
	printf 'rcfix=1\n'
	printf 'result=%s\n' "$RESULT"
	printf 'slot=%s\n' "$slot"
	printf 'hopped=%s\n' "$HOPPED"
	if [ -n "$p1" ]; then printf 'p1_sectors=%s\n' "$p1"; fi
	if [ -n "$table" ]; then printf 'table=%s\n' "$table"; fi
	if [ -n "$REASON" ]; then printf 'reason=%s\n' "$REASON"; fi
	if [ -n "$SECTOR0_HEX" ]; then printf 'sector0_before=%s\n' "$SECTOR0_HEX"; fi
	printf 'script=%s\n' "$RCFIX_VERSION"
	printf 'end=1\n'
}

# Writes the marker text to the state directory, from the saved result.
rcfix_prepare_marker() {
	if ! rcfix_load_result; then
		RESULT=failed
		REASON="the result of the first step could not be read"
		SECTOR0_HEX=
	fi
	rcfix_marker_text 2>/dev/null >"$STATE_DIR/marker"
}

rcfix_write_marker() {
	local mnt=$RCFIX_MNT_IMG ok=0
	rcfix_prepare_marker || return 1
	[ -f "$RCFIX_IMAGE" ] || return 1
	if rcfix_is_mounted "$mnt"; then
		rcfix_log "$mnt is already mounted"
		return 1
	fi
	mkdir -p "$mnt" || return 1
	if ! rcfix_act mount -t vfat -o rw "$RCFIX_IMAGE" "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rmdir "$mnt" 2>/dev/null
		return 1
	fi
	if cp "$STATE_DIR/marker" "$mnt/$MARKER" && sync && cmp -s "$STATE_DIR/marker" "$mnt/$MARKER"; then
		ok=1
	fi
	sync
	if ! rcfix_act umount "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rcfix_log "could not unmount the update image"
		return 1
	fi
	rmdir "$mnt" 2>/dev/null
	[ "$ok" = 1 ]
}

# RODE's main program wipes the update image when it starts, and how long
# after starting is not known. So after writing the marker, watch the image
# for a while: if it changes, the marker may be gone and is written again.
rcfix_marker_stays() {
	local sig0 i=0
	sig0=$(rcfix_image_sig)
	[ -n "$sig0" ] || return 1
	while [ "$i" -lt "$RCFIX_CONFIRM_POLLS" ]; do
		sleep "$RCFIX_POLL_SECS"
		[ "$(rcfix_image_sig)" = "$sig0" ] || return 1
		i=$((i + 1))
	done
	return 0
}

# ---- Time ------------------------------------------------------------------

# Prints the seconds since boot as the kernel gives them ("123.45"), or "?".
rcfix_uptime() {
	local up
	up=$(cat "$RCFIX_UPTIME" 2>/dev/null) || up=
	up=${up%% *}
	printf '%s' "${up:-?}"
}

# Prints a decimal number of seconds ("20", "0.5", "123.45") in hundredths;
# fails on anything else.
rcfix_cs() {
	local s=$1 int frac=
	case $s in '' | .* | *[!0-9.]* | *.*.*) return 1 ;; esac
	int=${s%%.*}
	case $s in *.*) frac=${s#*.} ;; esac
	frac=${frac}00
	frac=${frac:0:2}
	[ "${#int}" -le 9 ] || return 1
	printf '%s\n' "$((10#$int * 100 + 10#$frac))"
}

# Prints the monotonic time since boot in hundredths of a second; fails when
# it cannot be read.
rcfix_now() {
	rcfix_cs "$(rcfix_uptime)"
}

# Sleeps $1 seconds; a sleep that cannot take fractions sleeps a second, so
# no loop that uses it ever spins.
rcfix_sleep() {
	sleep "$1" 2>/dev/null || sleep 1
}

# ---- The USB report ----------------------------------------------------------
#
# RODE's normal-mode gadget has a removable mass-storage LUN with no file in
# it. The report puts a small writable FAT image holding the marker there
# and watches the LUN. The app reads the marker, writes RCFIX.ACK next to it
# and ejects the disk, which empties the LUN. An empty LUN alone proves
# nothing: on a mixer something empties it within seconds of the gadget
# being bound. So the script then looks in its own image: RCFIX.ACK there is
# the app's acknowledgement; without it the disk is offered again, at most
# RCFIX_ATTACH_TRIES times within RCFIX_REPORT_SECS. The script never binds
# or unbinds the gadget, never adds or removes gadget functions, never
# changes the LUN's read-only setting, and attaches only to an empty LUN of
# a gadget that has been bound, without a break, for RCFIX_BOUND_SECS.

rcfix_lun() {
	printf '%s\n' "$RCFIX_GADGET/functions/mass_storage.usb0/lun.0"
}

# Prints a configfs attribute's value without its newline; fails when it
# cannot be read.
rcfix_attr() {
	cat "$1" 2>/dev/null
}

rcfix_set_attr() {
	printf '%s\n' "$2" 2>/dev/null >"$1"
}

# True when the LUN is free: the gadget is bound to the USB controller,
# which RODE's program does once it runs, and the LUN holds no file.
rcfix_lun_free() {
	local lun udc file
	lun=$(rcfix_lun)
	udc=$(rcfix_attr "$RCFIX_GADGET/UDC") || return 1
	[ -n "$udc" ] || return 1
	file=$(rcfix_attr "$lun/file") || return 1
	[ -z "$file" ]
}

# Waits until the gadget has been bound to the same controller, without a
# break, for $2 hundredths of a second and the LUN is empty. False when
# time $1 (rcfix_now) comes first.
rcfix_wait_slot() {
	local limit=$1 bound=$2 lun udc prev='' since='' now file
	lun=$(rcfix_lun)
	while now=$(rcfix_now) && [ "$now" -lt "$limit" ]; do
		udc=$(rcfix_attr "$RCFIX_GADGET/UDC") || udc=
		if [ -z "$udc" ] || [ "$udc" != "$prev" ]; then
			since=
		fi
		if [ -n "$udc" ] && [ -z "$since" ]; then
			since=$now
		fi
		prev=$udc
		if [ -n "$since" ] && [ $((now - since)) -ge "$bound" ] &&
			file=$(rcfix_attr "$lun/file") && [ -z "$file" ]; then
			return 0
		fi
		rcfix_sleep "$RCFIX_ACK_POLL_SECS"
	done
	return 1
}

# Makes the report disk: a 1 MiB FAT image in RAM holding the marker,
# checked while mounted and unmounted again.
rcfix_build_report() {
	local img=$RCFIX_ACK_IMG mnt=$RCFIX_MNT_ACK ok=0
	if rcfix_is_mounted "$mnt" || rcfix_is_mounted "$img"; then
		rcfix_log "$mnt is already mounted"
		return 1
	fi
	rm -f "$img" || return 1
	dd if=/dev/zero of="$img" bs=1024 count=1024 2>/dev/null || return 1
	rcfix_act mkfs.vfat -n RCFIX "$img" >>"$STATE_DIR/log" 2>&1 || return 1
	mkdir -p "$mnt" || return 1
	if ! rcfix_act mount -t vfat -o rw "$img" "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rmdir "$mnt" 2>/dev/null
		return 1
	fi
	if cp "$STATE_DIR/marker" "$mnt/$MARKER" && sync && cmp -s "$STATE_DIR/marker" "$mnt/$MARKER"; then
		ok=1
	fi
	sync
	if ! rcfix_act umount "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rcfix_log "could not unmount the report disk"
		return 1
	fi
	rmdir "$mnt" 2>/dev/null
	[ "$ok" = 1 ]
}

# Removes the report disk's files, once it is detached or was never attached.
rcfix_drop_report() {
	if rcfix_is_mounted "$RCFIX_MNT_ACK"; then
		rcfix_act umount "$RCFIX_MNT_ACK" >>"$STATE_DIR/log" 2>&1
	fi
	rmdir "$RCFIX_MNT_ACK" 2>/dev/null
	rm -f "$RCFIX_ACK_IMG"
}

# Attaches the report disk to the free LUN, leaving its read-only setting
# as it is. What happens to the LUN afterwards, even at once, is for
# rcfix_watch_report to see: an emptied LUN means offering it again, one
# that names another file is RODE's.
rcfix_attach_report() {
	local lun
	lun=$(rcfix_lun)
	rcfix_lun_free || return 1
	rcfix_act rcfix_set_attr "$lun/file" "$RCFIX_ACK_IMG"
}

# Watches the attached report disk until time $1. REPORT is set to "emptied"
# (the LUN went empty: maybe the computer ejected it), "taken" (the LUN
# names another file, or cannot be read: RODE's program has it) or
# "timeout".
rcfix_watch_report() {
	local limit=$1 lun file now
	lun=$(rcfix_lun)
	while :; do
		rcfix_sleep "$RCFIX_ACK_POLL_SECS"
		if ! file=$(rcfix_attr "$lun/file"); then
			REPORT=taken
			return
		fi
		case $file in
		"$RCFIX_ACK_IMG") ;;
		'')
			REPORT=emptied
			return
			;;
		*)
			REPORT=taken
			return
			;;
		esac
		if ! now=$(rcfix_now) || [ "$now" -ge "$limit" ]; then
			REPORT=timeout
			return
		fi
	done
}

# Looks for the app's RCFIX.ACK in the report disk, mounted read-only here
# while no LUN holds it. Returns 0 when it is there, 1 when it is not, 2
# when the disk could not be looked at.
rcfix_check_ack() {
	local img=$RCFIX_ACK_IMG mnt=$RCFIX_MNT_ACK rc=1
	if rcfix_is_mounted "$mnt" || rcfix_is_mounted "$img"; then
		return 2
	fi
	mkdir -p "$mnt" || return 2
	if ! rcfix_act mount -t vfat -o ro "$img" "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rmdir "$mnt" 2>/dev/null
		return 2
	fi
	[ -f "$mnt/$ACK" ] && rc=0
	if ! rcfix_act umount "$mnt" >>"$STATE_DIR/log" 2>&1; then
		rcfix_log "could not unmount the report disk"
		return 2
	fi
	rmdir "$mnt" 2>/dev/null
	return "$rc"
}

# Detaches the report disk after the time limit, only while the LUN still
# names it. True when the LUN is empty afterwards.
rcfix_detach_report() {
	local lun
	lun=$(rcfix_lun)
	[ "$(rcfix_attr "$lun/file")" = "$RCFIX_ACK_IMG" ] || return 1
	rcfix_act rcfix_set_attr "$lun/file" "" || return 1
	[ -z "$(rcfix_attr "$lun/file")" ]
}

# Offers the marker on the report disk. True when the app acknowledged it:
# the app has the marker, and the update drive must be left alone from now
# on, because the app is about to put firmware on it. False in every other
# case, with the LUN given back (or left to RODE's program), so the
# update-drive path runs as before.
rcfix_usb_report() {
	local lun start limit bound tries=0 built=0 ack
	[ "$RCFIX_USB_REPORT" = 1 ] || return 1
	lun=$(rcfix_lun)
	if [ ! -e "$lun/file" ] || [ ! -e "$lun/ro" ] || [ ! -e "$RCFIX_GADGET/UDC" ]; then
		rcfix_log "no USB disk slot for the report"
		return 1
	fi
	case $RCFIX_ATTACH_TRIES in '' | *[!0-9]*) RCFIX_ATTACH_TRIES=0 ;; esac
	if ! start=$(rcfix_now) || ! limit=$(rcfix_cs "$RCFIX_REPORT_SECS") ||
		! bound=$(rcfix_cs "$RCFIX_BOUND_SECS"); then
		rcfix_log "the USB report's timing could not be set up"
		return 1
	fi
	limit=$((start + limit))
	while [ "$tries" -lt "$RCFIX_ATTACH_TRIES" ]; do
		if ! rcfix_wait_slot "$limit" "$bound"; then
			rcfix_log "the USB disk slot was not free, with the gadget bound for $RCFIX_BOUND_SECS s, in time"
			break
		fi
		if [ "$(rcfix_attr "$lun/ro")" != 0 ]; then
			rcfix_log "the USB disk slot is read-only, so the app could not acknowledge the report"
			break
		fi
		if [ "$built" = 0 ]; then
			if ! rcfix_prepare_marker || ! rcfix_build_report; then
				rcfix_log "could not make the report disk"
				break
			fi
			built=1
		fi
		# Never offer the disk while this system has it mounted.
		if rcfix_is_mounted "$RCFIX_MNT_ACK" || rcfix_is_mounted "$RCFIX_ACK_IMG"; then
			rcfix_log "the report disk is still mounted here"
			break
		fi
		tries=$((tries + 1))
		if ! rcfix_attach_report; then
			rcfix_log "could not attach the report disk"
			break
		fi
		rcfix_log "report disk attached ($tries of $RCFIX_ATTACH_TRIES); waiting for the app"
		rcfix_watch_report "$limit"
		case $REPORT in
		emptied)
			rcfix_check_ack
			ack=$?
			case $ack in
			0)
				rcfix_log "the app acknowledged the report"
				rcfix_drop_report
				return 0
				;;
			1) rcfix_log "the USB disk slot was emptied without the app's acknowledgement" ;;
			*)
				rcfix_log "could not look for the app's acknowledgement"
				break
				;;
			esac
			;;
		taken)
			# The app may have acknowledged the disk and sent the mixer to
			# update mode before this script looked again: RODE's program
			# then puts its own image in the slot. The LUN no longer holds
			# the report disk, so its acknowledgement can be looked for.
			if rcfix_check_ack; then
				rcfix_log "the app acknowledged the report; RODE's program has the USB disk slot now"
				rcfix_drop_report
				return 0
			fi
			rcfix_log "RODE's program took the USB disk slot"
			break
			;;
		*)
			if rcfix_detach_report; then
				rcfix_log "the report was not acknowledged in time; detached it"
			else
				rcfix_log "the report was not acknowledged in time and could not be detached"
			fi
			break
			;;
		esac
	done
	[ "$tries" -lt "$RCFIX_ATTACH_TRIES" ] || rcfix_log "the report was offered $tries times without an acknowledgement"
	rcfix_drop_report
	return 1
}

# ---- Evidence for development ------------------------------------------------
#
# With RCFIX_DEBUG=1 phase B keeps a log in RCFIX_DEBUG_DIR, named after the
# boot: the log so far, every later log line, and every change of the USB
# disk slot (its file and ro) and of the gadget's UDC, sampled every
# RCFIX_TRACE_SECS, all with the time since boot. Nothing removes it.

# Appends every change of the slot until process $1 (phase B) has ended.
rcfix_trace() {
	local parent=$1 lun line prev=''
	lun=$(rcfix_lun)
	while kill -0 "$parent" 2>/dev/null; do
		line="file=$(rcfix_attr "$lun/file" || printf '?') ro=$(rcfix_attr "$lun/ro" || printf '?') udc=$(rcfix_attr "$RCFIX_GADGET/UDC" || printf '?')"
		if [ "$line" != "$prev" ]; then
			printf '%s trace %s\n' "$(rcfix_uptime)" "$line" 2>/dev/null >>"$DEBUG_FILE"
			prev=$line
		fi
		rcfix_sleep "$RCFIX_TRACE_SECS"
	done
}

rcfix_debug_start() {
	local id
	[ "$RCFIX_DEBUG" = 1 ] || return 0
	id=$(rcfix_boot_id)
	id=$(printf '%s' "$id" | tr -c 'a-zA-Z0-9-' '_')
	DEBUG_NAME=${id:-unknown}.log
	# Kept in RAM until phase B ends: RODE's start-up script empties the
	# folder under /Application/update-staging and mounts the staging
	# partition over it, possibly after phase B has begun.
	DEBUG_FILE=${RCFIX_ACK_IMG%/*}/rcfix-debug.tmp
	rm -f "$DEBUG_FILE"
	{
		printf '%s rcfix %s: phase B starts; the log so far:\n' "$(rcfix_uptime)" "$RCFIX_VERSION"
		cat "$STATE_DIR/log" 2>/dev/null
		printf '%s from here on, log lines and USB disk slot changes\n' "$(rcfix_uptime)"
	} 2>/dev/null >>"$DEBUG_FILE"
	rcfix_trace "$$" </dev/null >/dev/null 2>&1 &
	TRACE_PID=$!
}

# Ends the trace and copies the log to RCFIX_DEBUG_DIR, which by now is on
# the mounted staging partition: phase B has seen RODE's update image there.
rcfix_debug_stop() {
	[ -n "$TRACE_PID" ] || return 0
	kill "$TRACE_PID" 2>/dev/null
	wait "$TRACE_PID" 2>/dev/null
	TRACE_PID=
	printf '%s phase B ended\n' "$(rcfix_uptime)" 2>/dev/null >>"$DEBUG_FILE"
	if mkdir -p "$RCFIX_DEBUG_DIR" 2>/dev/null &&
		cat "$DEBUG_FILE" 2>/dev/null >>"$RCFIX_DEBUG_DIR/$DEBUG_NAME"; then
		sync
		rm -f "$DEBUG_FILE"
	fi
}

# Waits for RODE's main program to have wiped the update image (it changes
# from what it was before RODE started, then stays unchanged), writes the
# marker into it, and removes everything. The USB report is tried once, at
# the first point the update drive is ready: the report must not reach the
# app before RODE's wipe, which could otherwise erase what the app then
# stages on the update drive.
rcfix_deliver() {
	local start_sig=$1 sig prev='' quiet=0 polls=0 changed=0 running=0 reported=0
	rcfix_log "phase B: waiting for the update drive"
	while [ "$polls" -lt "$RCFIX_WAIT_POLLS" ]; do
		sig=$(rcfix_image_sig)
		if [ -n "$sig" ] && [ "$sig" != "$start_sig" ]; then
			changed=1
		fi
		if pgrep -x rc_audio_mixer >/dev/null 2>&1; then
			running=$((running + 1))
		else
			running=0
		fi
		if [ "$running" -gt 0 ] && [ -n "$sig" ] && [ "$sig" = "$prev" ]; then
			quiet=$((quiet + 1))
		else
			quiet=0
		fi
		prev=$sig
		if [ "$quiet" -ge "$RCFIX_QUIET_POLLS" ] &&
			{ [ "$changed" = 1 ] || [ "$running" -ge "$RCFIX_FALLBACK_POLLS" ]; }; then
			if [ "$reported" = 0 ]; then
				reported=1
				if rcfix_usb_report; then
					rcfix_log "result delivered over USB; removing rcfix"
					rcfix_uninstall "$RCFIX_SYSROOT" || rcfix_log "could not remove every rcfix file"
					return 0
				fi
				# Look at the update drive afresh before writing to it.
				quiet=0
				prev=
				polls=$((polls + 1))
				sleep "$RCFIX_POLL_SECS"
				continue
			fi
			if rcfix_write_marker; then
				if rcfix_marker_stays; then
					rcfix_log "marker written; removing rcfix"
					rcfix_uninstall "$RCFIX_SYSROOT" || rcfix_log "could not remove every rcfix file"
					return 0
				fi
				rcfix_log "the update image changed after the marker was written; writing it again"
			else
				rcfix_log "could not write the marker yet"
			fi
			quiet=0
		fi
		polls=$((polls + 1))
		sleep "$RCFIX_POLL_SECS"
	done
	if [ "$(rcfix_read_boots)" -ge "$RCFIX_MAX_BOOTS" ]; then
		rcfix_log "the update drive never became ready; giving up for good"
		rcfix_uninstall "$RCFIX_SYSROOT" || rcfix_log "could not remove every rcfix file"
	else
		rcfix_log "the update drive did not become ready; trying again at the next start"
	fi
	return 0
}

rcfix_phase_b() {
	rcfix_debug_start
	rcfix_deliver "$1"
	rcfix_debug_stop
}

rcfix_main() {
	local mode=${1:-start} sig
	set -u
	umask 022
	LC_ALL=C
	PATH=$RCFIX_PATH
	export LC_ALL PATH
	SELF=${BASH_SOURCE[0]}
	rcfix_setup
	if ! rcfix_self_ok "$SELF"; then
		rcfix_log "$SELF is damaged; this copy does nothing"
		exit 0
	fi
	case $mode in
	start) ;;
	phase-b)
		rcfix_phase_b "${2:-}"
		exit 0
		;;
	*) exit 0 ;;
	esac
	if ! mkdir -p "$STATE_DIR"; then
		rcfix_log "could not create $STATE_DIR"
		exit 0
	fi
	rcfix_take_lock || exit 0
	rcfix_count_boot
	if [ "$BOOTS" -gt "$RCFIX_MAX_BOOTS" ]; then
		rcfix_log "started on $BOOTS boots without finishing; giving up for good"
		rcfix_uninstall "$RCFIX_SYSROOT"
		exit 0
	fi
	rcfix_load_result || rcfix_phase_a
	# Taken before RODE starts, so phase B can tell when RODE has wiped it.
	sig=$(rcfix_image_sig)
	if [ "$RCFIX_FOREGROUND" = 1 ]; then
		rcfix_phase_b "$sig"
	elif command -v setsid >/dev/null 2>&1; then
		setsid nohup "$BASH" "$SELF" phase-b "$sig" </dev/null >/dev/null 2>&1 &
	else
		nohup "$BASH" "$SELF" phase-b "$sig" </dev/null >/dev/null 2>&1 &
	fi
	exit 0
}

rcfix_main "$@"
