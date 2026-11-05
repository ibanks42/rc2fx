#!/bin/sh
# Shared by the Linux package check scripts. Source it, then call
# expect_embedded_envelope PATH.
#
# This verifies the embedded installer envelope properly: exactly one marker,
# its length and SHA-256 headers, and that the body matches the digest. A
# marker string alone can occur in ordinary compiled data and is not proof by
# itself.

expect_embedded_envelope() {
    executable=$1
    test -f "$executable" || { echo "missing executable: $executable" >&2; exit 1; }
    magic='RC2FX-EMBEDDED-PAYLOAD 1'
    occurrences=$(grep -aob "$magic" "$executable" | wc -l | tr -d ' ')
    if [ "$occurrences" -ne 1 ]; then
        echo "expected exactly one embedded payload in $executable, found $occurrences" >&2
        exit 1
    fi
    offset=$(grep -aob "$magic" "$executable" | head -1 | cut -d: -f1)
    # MAGIC ends with a newline; the first header starts after it.
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

# Run the shipped program's helper entry with no arguments. It exits 2 before
# any device, file or GUI work; never pass ROOT/WORK, which would start the
# privileged installer path.
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
        # A shell syntax/unbound-variable error also exits 2. A real helper
        # invocation is silent, so any output means the program did not reach
        # the helper entry.
        echo "expected --installer-helper to be silent, but it wrote:" >&2
        cat "$out" "$err" >&2
        rm -f "$out" "$err"
        return 1
    fi
    rm -f "$out" "$err"
}
