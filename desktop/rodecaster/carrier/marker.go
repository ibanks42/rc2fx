package carrier

import (
	"bytes"
	"encoding/hex"
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// Result is the outcome recorded by the carrier script.
type Result string

const (
	ResultFixed        Result = "fixed"
	ResultAlreadyFixed Result = "already-fixed"
	ResultUnrecognized Result = "unrecognized"
	ResultFailed       Result = "failed"
)

// Marker is the parsed contents of RCFIX.TXT.
type Marker struct {
	Result        Result
	Slot          int      // active root slot (1 or 2; 0 if unknown from /proc/cmdline)
	Hopped        bool     // true if the script hopped from slot 1 to slot 2 first
	P1Sectors     uint32   // partition 1 size in sectors when written (0 if unreadable)
	Table         []string // partition lines from `sfdisk -d`
	Reason        string   // populated for ResultUnrecognized and ResultFailed
	Sector0Before []byte   // 512-byte MBR backup taken before resizing, if any
	Script        string   // carrier script version
}

const (
	maxMarkerRaw = 1 << 20 // including trailing padding
	maxMarker    = 16 << 10
)

var markerKeys = map[string]bool{
	"result": true, "slot": true, "hopped": true, "p1_sectors": true,
	"table": true, "reason": true, "sector0_before": true, "script": true,
}

// ParseMarker strictly parses RCFIX.TXT (must start with rcfix=1, end with
// end=1, and contain only known non-duplicate keys). CRLF line endings and
// trailing NUL/whitespace padding are allowed.
func ParseMarker(data []byte) (Marker, error) {
	var m Marker
	if len(data) > maxMarkerRaw {
		return m, errors.New("marker: file too large")
	}
	data = bytes.TrimRight(data, "\x00 \t\r\n")
	if len(data) > maxMarker {
		return m, errors.New("marker: file too large")
	}
	for i, c := range data {
		if c >= 0x7f || (c < 0x20 && c != '\n' && c != '\r' && c != '\t') {
			return m, fmt.Errorf("marker: byte %d is not printable ASCII", i)
		}
	}
	lines := strings.Split(string(data), "\n")
	for i := range lines {
		lines[i] = strings.TrimSuffix(lines[i], "\r")
	}
	if len(lines) < 2 || lines[0] != "rcfix=1" {
		return m, errors.New("marker: first line is not rcfix=1")
	}
	if lines[len(lines)-1] != "end=1" {
		return m, errors.New("marker: incomplete, last line is not end=1")
	}
	vals := map[string]string{}
	for _, line := range lines[1 : len(lines)-1] {
		k, v, ok := strings.Cut(line, "=")
		if !ok {
			return m, fmt.Errorf("marker: line %q is not key=value", line)
		}
		if !markerKeys[k] {
			return m, fmt.Errorf("marker: unknown key %q", k)
		}
		if _, dup := vals[k]; dup {
			return m, fmt.Errorf("marker: key %q appears twice", k)
		}
		vals[k] = v
	}
	for _, k := range []string{"result", "slot", "hopped", "script"} {
		if _, ok := vals[k]; !ok {
			return m, fmt.Errorf("marker: %s is missing", k)
		}
	}

	switch r := Result(vals["result"]); r {
	case ResultFixed, ResultAlreadyFixed, ResultUnrecognized, ResultFailed:
		m.Result = r
	default:
		return m, fmt.Errorf("marker: unknown result %q", vals["result"])
	}
	switch vals["slot"] {
	case "0", "1", "2":
		m.Slot = int(vals["slot"][0] - '0')
	default:
		return m, fmt.Errorf("marker: bad slot %q", vals["slot"])
	}
	switch vals["hopped"] {
	case "0":
	case "1":
		m.Hopped = true
	default:
		return m, fmt.Errorf("marker: bad hopped %q", vals["hopped"])
	}
	if v, ok := vals["p1_sectors"]; ok {
		n, err := strconv.ParseUint(v, 10, 32)
		if err != nil || n == 0 || v[0] == '0' {
			return m, fmt.Errorf("marker: bad p1_sectors %q", v)
		}
		m.P1Sectors = uint32(n)
	}
	if v, ok := vals["table"]; ok {
		if v == "" {
			return m, errors.New("marker: empty table")
		}
		m.Table = strings.Split(v, "|")
		if len(m.Table) > 16 {
			return m, errors.New("marker: table has too many lines")
		}
		for _, l := range m.Table {
			if l == "" {
				return m, errors.New("marker: empty table line")
			}
		}
	}
	m.Reason = vals["reason"]
	if (m.Result == ResultUnrecognized || m.Result == ResultFailed) && m.Reason == "" {
		return m, fmt.Errorf("marker: result %s without a reason", m.Result)
	}
	if v, ok := vals["sector0_before"]; ok {
		if len(v) != 1024 {
			return m, fmt.Errorf("marker: sector0_before has %d hex digits, expected 1024", len(v))
		}
		b, err := hex.DecodeString(v)
		if err != nil {
			return m, fmt.Errorf("marker: bad sector0_before: %v", err)
		}
		m.Sector0Before = b
	}
	m.Script = vals["script"]
	if !validVersion(m.Script) {
		return m, fmt.Errorf("marker: bad script version %q", m.Script)
	}
	return m, nil
}

func validVersion(s string) bool {
	if s == "" || len(s) > 32 {
		return false
	}
	for _, c := range s {
		ok := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' ||
			c == '.' || c == '_' || c == '-'
		if !ok {
			return false
		}
	}
	return true
}
