package core

import (
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// Release identities: the RC2FX mixer runtime's own version, kept apart from
// the desktop app's version and from RØDE's firmware version.
//
// The mixer runtime ships `mixer-release.json` (see `config/`), installed as
// `/opt/rc2fx/mixer-release.json`. The device reports what it actually has
// installed through `GET api/v1/version` as DeviceVersions. A runtime that
// predates the endpoint answers 404, and its versions stay unknown.

// MixerProtocolVersion is the control protocol this build speaks over USB 1.
// Bump it when a desktop app built against an older protocol could no longer
// drive the mixer.
const MixerProtocolVersion uint32 = 1

// MixerReleaseSchema is the only `mixer-release.json` schema this build
// reads.
const MixerReleaseSchema uint32 = 1

// maxReleaseBytes is the largest manifest accepted, far above any real one.
const maxReleaseBytes = 4 * 1024

// maxVersionText: a firmware profile id may be up to 96 bytes; versions are
// far shorter.
const maxVersionText = 128

// MixerRelease is `mixer-release.json`: the RC2FX mixer runtime's release
// identity.
type MixerRelease struct {
	SchemaVersion   uint32 `json:"schemaVersion"`
	Version         string `json:"version"`
	ProtocolVersion uint32 `json:"protocolVersion"`
}

// ParseMixerRelease parses and checks a manifest: strict JSON, schema 1, a
// dotted numeric version and a non-zero protocol.
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

// DeviceVersions is what the mixer reports from `GET api/v1/version`. Every
// field is optional: a field the runtime cannot establish is absent, never
// guessed. Unknown fields are accepted so an older app can read a newer
// mixer.
type DeviceVersions struct {
	// The installed RC2FX mixer release version.
	Runtime *string `json:"runtime,omitempty"`
	// The installed runtime's control protocol version.
	Protocol *uint32 `json:"protocol,omitempty"`
	// The native library's control protocol, when older than this daemon's.
	NativeProtocol *uint32 `json:"nativeProtocol,omitempty"`
	// RØDE's firmware version, from the profile the launcher validated.
	Firmware *string `json:"firmware,omitempty"`
	// The validated firmware profile's id.
	FirmwareProfile *string `json:"firmwareProfile,omitempty"`
	// Set when this controller installs signed RC2FX releases in place over
	// USB 1 (see `docs/developer/mixer-releases.md`); absent when it cannot.
	// 1 takes whole uploads; 2 also lists its installed files and takes delta
	// uploads. Any value above zero installs in place.
	InPlaceUpdates *uint32 `json:"inPlaceUpdates,omitempty"`
}

func (DeviceVersions) AllowUnknownJSONFields() {}

// Equal compares field by field (deepEqual).
func (d DeviceVersions) Equal(other DeviceVersions) bool { return deepEqual(d, other) }

// Validate refuses text no real version has: these fields are shown as they
// arrive.
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

// CompareVersions compares dotted numeric versions (`1.7.7`, `0.1`). Missing
// trailing parts count as zero, so `1.7` equals `1.7.0`. Anything else,
// including pre-release suffixes, is not comparable and gives false. The
// order is -1, 0 or 1, as cmp.Compare.
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
