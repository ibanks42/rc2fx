package core

import (
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Mixer runtime release metadata (distinct from both the desktop app version
// and RØDE's stock firmware version). Wire types are generated from
// schema/core.go into schema_gen.go.

const (
	maxReleaseBytes = 4 * 1024
	maxVersionText  = 128 // firmware profile IDs can be up to 96 bytes
)

// ParseMixerRelease strictly parses and validates a mixer-release.json file.
func ParseMixerRelease(data []byte) (MixerRelease, error) {
	if !utf8.Valid(data) {
		return MixerRelease{}, CoreError("mixer-release.json is not UTF-8")
	}
	release, err := FromJSONLimited[MixerRelease](string(data), maxReleaseBytes, 16)
	if err != nil {
		return MixerRelease{}, err
	}
	if release.SchemaVersion != MixerReleaseSchema {
		return MixerRelease{}, CoreError("unsupported mixer-release.json schema " +
			strconv.FormatUint(uint64(release.SchemaVersion), 10))
	}
	if _, ok := parseVersion(release.Version); !ok {
		return MixerRelease{}, CoreError("mixer-release.json version " + rustDebugString(release.Version) +
			" is not dotted numeric")
	}
	if release.ProtocolVersion == 0 {
		return MixerRelease{}, CoreError("mixer-release.json protocolVersion must be at least 1")
	}
	return release, nil
}

func (d DeviceVersions) Validate() error {
	for _, text := range []*string{d.Runtime, d.Firmware, d.FirmwareProfile} {
		if text != nil && !ValidDisplayText(*text, maxVersionText) {
			return CoreError("Invalid mixer version text")
		}
	}
	return nil
}

func parseVersion(text string) ([]uint64, bool) {
	if text == "" {
		return nil, false
	}
	parts := strings.Split(text, ".")
	values := make([]uint64, 0, len(parts))
	for _, part := range parts {
		if part == "" || len(part) > 9 {
			return nil, false
		}
		for i := 0; i < len(part); i++ {
			if part[i] < '0' || part[i] > '9' {
				return nil, false
			}
		}
		value, err := strconv.ParseUint(part, 10, 64)
		if err != nil {
			return nil, false
		}
		values = append(values, value)
	}
	return values, true
}

// CompareVersions compares dotted numeric versions (e.g. "1.7" == "1.7.0"),
// returning (-1/0/1, true) or (0, false) for non-numeric/pre-release strings.
func CompareVersions(a, b string) (int, bool) {
	left, ok := parseVersion(a)
	if !ok {
		return 0, false
	}
	right, ok := parseVersion(b)
	if !ok {
		return 0, false
	}
	part := func(values []uint64, i int) uint64 {
		if i < len(values) {
			return values[i]
		}
		return 0
	}
	for i := 0; i < max(len(left), len(right)); i++ {
		x, y := part(left, i), part(right, i)
		if x < y {
			return -1, true
		}
		if x > y {
			return 1, true
		}
	}
	return 0, true
}

// rustDebugString quotes like Rust's `{:?}` for a str.
func rustDebugString(text string) string {
	var out strings.Builder
	out.WriteByte('"')
	for _, c := range text {
		switch c {
		case '"':
			out.WriteString(`\"`)
		case '\\':
			out.WriteString(`\\`)
		case '\n':
			out.WriteString(`\n`)
		case '\r':
			out.WriteString(`\r`)
		case '\t':
			out.WriteString(`\t`)
		case 0:
			out.WriteString(`\0`)
		default:
			if unicode.IsControl(c) || IsBidiControl(c) || !unicode.IsPrint(c) {
				out.WriteString(`\u{` + strconv.FormatInt(int64(c), 16) + `}`)
			} else {
				out.WriteRune(c)
			}
		}
	}
	out.WriteByte('"')
	return out.String()
}
