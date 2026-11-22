// Package common provides small shared text, hashing, filesystem, and time
// helpers used across desktop packages.
package common

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"strconv"
	"strings"
	"time"
)

// Lines splits text on '\n', stripping a preceding '\r' and omitting any
// trailing empty element after a final '\n'.
func Lines(text string) []string {
	var out []string
	for text != "" {
		line, rest, found := strings.Cut(text, "\n")
		if found {
			line = strings.TrimSuffix(line, "\r")
		}
		out = append(out, line)
		text = rest
		if !found {
			break
		}
	}
	return out
}

// SplitLinesInclusive splits text into lines, retaining each trailing '\n'.
func SplitLinesInclusive(text string) []string {
	var out []string
	for text != "" {
		i := strings.IndexByte(text, '\n')
		if i < 0 {
			out = append(out, text)
			break
		}
		out = append(out, text[:i+1])
		text = text[i+1:]
	}
	return out
}

// IsHex reports whether every byte in text is an ASCII hexadecimal digit.
func IsHex(text string) bool {
	for i := range len(text) {
		c := text[i]
		if !('0' <= c && c <= '9' || 'a' <= c && c <= 'f' || 'A' <= c && c <= 'F') {
			return false
		}
	}
	return true
}

// IsSpace reports whether r is a Unicode White_Space character.
func IsSpace(r rune) bool {
	switch r {
	case '\t', '\n', '\v', '\f', '\r', ' ', 0x85, 0xa0, 0x1680, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000:
		return true
	}
	return r >= 0x2000 && r <= 0x200a
}

// ASCIIEqualFold reports whether a and b are equal ignoring ASCII letter case.
func ASCIIEqualFold(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range len(a) {
		x, y := a[i], b[i]
		if 'A' <= x && x <= 'Z' {
			x += 'a' - 'A'
		}
		if 'A' <= y && y <= 'Z' {
			y += 'a' - 'A'
		}
		if x != y {
			return false
		}
	}
	return true
}

// ParseUint parses an unsigned base-10 integer with an optional leading '+'
// and ASCII digits only.
func ParseUint(text string, bits int) (uint64, bool) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" {
		return 0, false
	}
	for i := range len(digits) {
		if digits[i] < '0' || digits[i] > '9' {
			return 0, false
		}
	}
	value, err := strconv.ParseUint(digits, 10, bits)
	return value, err == nil
}

// NowUnix returns the current Unix timestamp in seconds, clamped at zero.
func NowUnix() uint64 {
	now := time.Now().Unix()
	if now < 0 {
		return 0
	}
	return uint64(now)
}

// PathExists reports whether os.Stat succeeds for path.
func PathExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

// SHA256Hex returns the lowercase hex-encoded SHA-256 digest of data.
func SHA256Hex(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}
