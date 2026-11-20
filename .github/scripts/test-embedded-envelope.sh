#!/bin/sh
# Helpers sourced by the Linux package check scripts.
# expect_embedded_envelope validates either download-mode markers or a single
# embedded payload envelope (magic header, length, and matching SHA-256).

expect_embedded_envelope() {
    executable=$1
    test -f "$executable" || { echo "missing executable: $executable" >&2; exit 1; }
    if grep -aq 'RC2FX-PAYLOAD-MODE:download:1' "$executable"; then
        if grep -aq 'RC2FX-PAYLOAD-MODE:embedded:1' "$executable"; then
            echo 'executable contains conflicting mixer payload modes' >&2
            exit 1
        fi
        return 0
    fi
    magic='RC2FX-EMBEDDED-PAYLOAD 1'
    occurrences=$(grep -aob "$magic" "$executable" | wc -l | tr -d ' ')
    if [ "$occurrences" -ne 1 ]; then
        echo "expected exactly one embedded payload in $executable, found $occurrences" >&2
        exit 1
    fi
    offset=$(grep -aob "$magic" "$executable" | head -1 | cut -d: -f1)
    # Skip past the magic line and its trailing newline.
    header_at=$((offset + ${#magic} + 1))
    headers=$(dd if="$executable" bs=1 skip="$header_at" count=128 2>/dev/null || true)
    length=$(printf '%s\n' "$headers" | sed -n '1s/^length //p')
    digest=$(printf '%s\n' "$headers" | sed -n '2s/^sha256 //p')
    case "$length" in
        ''|*[!0-9]*)
            echo "embedded payload length is missing or not a number" >&2
            exit 1
            ;;
    esac
    case "$digest" in
        *[!0-9a-f]*|'')
            echo "embedded payload digest is missing or malformed" >&2
            exit 1
            ;;
    esac
    if [ "${#digest}" -ne 64 ]; then
        echo "embedded payload digest is not 64 hexadecimal characters" >&2
        exit 1
    fi
    if [ "$length" -gt 268435456 ]; then
        echo "embedded payload is implausibly large: $length" >&2
        exit 1
    fi
    body_at=$((header_at + 7 + ${#length} + 1 + 7 + 64 + 1))
    actual=$(dd if="$executable" bs=1 skip="$body_at" count="$length" 2>/dev/null \
        | sha256sum | awk '{ print $1 }')
    if [ "$actual" != "$digest" ]; then
        echo "embedded payload does not match its SHA-256 header" >&2
        exit 1
    fi
}

# Invoke --installer-helper with no arguments; it should exit 2 immediately
# without touching devices, files, or GUI (never pass ROOT/WORK here).
expect_installer_helper_exit_2() {
    out=$(mktemp)
    err=$(mktemp)
    rc=0
    timeout 30 "$@" >"$out" 2>"$err" || rc=$?
    if [ "$rc" -ne 2 ]; then
        echo "expected --installer-helper to exit 2, got $rc: $*" >&2
        rm -f "$out" "$err"
        return 1
    fi
    if [ -s "$out" ] || [ -s "$err" ]; then
        # Shell syntax/unbound-var errors also exit 2, whereas the real helper exits silently.
        echo "expected --installer-helper to be silent, but it wrote:" >&2
        cat "$out" "$err" >&2
        rm -f "$out" "$err"
        return 1
    fi
    rm -f "$out" "$err"
}
