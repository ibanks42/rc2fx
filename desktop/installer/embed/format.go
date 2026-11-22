// Package embed defines the installer payload layout, validation, and binary
// envelope format compiled into the desktop app under `-tags embedded`.
package embed

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// PayloadNames, Pinned, Binaries, and Scripts are generated from
// schema/payload.go into lists_gen.go (and into Rust at
// crates/rc2fx-device/src/mixer_update/payload_lists_gen.rs).

const ReleaseManifest = "mixer-release.json"

const ReleaseSchema uint64 = 1

const Magic = "RC2FX-EMBEDDED-PAYLOAD 1\n"

// BuildLine and ReleaseLine control default GUI diagnostic verbosity in rc2fx.sh.
const BuildLine = "rc2fx_build=development"
const ReleaseLine = "rc2fx_build=release"

const (
	Development = "development"
	Release     = "release"
)

const maxBody = 256 << 20 // sanity cap; real payloads are ~10 MB

type File struct {
	Name string
	Data []byte
}

// Files holds payload files in PayloadNames order.
type Files []File

func IsBinary(name string) bool { return slices.Contains(Binaries[:], name) }

func IsScript(name string) bool { return slices.Contains(Scripts[:], name) }

func CRLFToLF(data []byte) []byte {
	out := make([]byte, 0, len(data))
	for i, b := range data {
		if !(b == '\r' && i+1 < len(data) && data[i+1] == '\n') {
			out = append(out, b)
		}
	}
	return out
}

type buildLine struct {
	kind       string
	start, end int
}

func utf8Error(script []byte) error {
	for i := 0; i < len(script); {
		r, size := utf8.DecodeRune(script[i:])
		if r == utf8.RuneError && size <= 1 {
			return fmt.Errorf("invalid utf-8 sequence of 1 bytes from index %d", i)
		}
		i += size
	}
	return nil
}

// parseBuildLine locates the single exact rc2fx_build= assignment in rc2fx.sh
// and rejects any other assignment or duplicate.
func parseBuildLine(script []byte) (buildLine, error) {
	if err := utf8Error(script); err != nil {
		return buildLine{}, fmt.Errorf("rc2fx.sh is not UTF-8: %v", err)
	}
	text := string(script)
	var found *buildLine
	at := 0
	for _, raw := range common.SplitLinesInclusive(text) {
		line := strings.TrimRight(raw, "\r\n")
		trimmed := strings.TrimSpace(line)
		if trimmed == BuildLine || trimmed == ReleaseLine {
			if found != nil {
				return buildLine{}, fmt.Errorf("rc2fx.sh must have exactly one `%s` line; found more than one", BuildLine)
			}
			indent := len(line) - len(strings.TrimLeftFunc(line, common.IsSpace))
			kind := Development
			if trimmed == ReleaseLine {
				kind = Release
			}
			found = &buildLine{kind: kind, start: at + indent, end: at + len(line)}
		} else if strings.HasPrefix(trimmed, "rc2fx_build") || strings.HasPrefix(trimmed, "export rc2fx_build") {
			return buildLine{}, fmt.Errorf("rc2fx.sh has an unsupported `rc2fx_build` line: `%s`; only one exact `%s` line is allowed", trimmed, BuildLine)
		}
		at += len(raw)
	}
	if found == nil {
		return buildLine{}, fmt.Errorf("rc2fx.sh must have exactly one `%s` line; found none", BuildLine)
	}
	return *found, nil
}

func BuildKind(script []byte) (string, error) {
	line, err := parseBuildLine(script)
	if err != nil {
		return "", err
	}
	return line.kind, nil
}

// ReleaseScript rewrites rc2fx.sh's build line to release without touching
// anything else.
func ReleaseScript(script []byte) ([]byte, error) {
	line, err := parseBuildLine(script)
	if err != nil {
		return nil, err
	}
	if line.kind == Release {
		return slices.Clone(script), nil
	}
	text := string(script)
	out := []byte(text[:line.start] + ReleaseLine + text[line.end:])
	kind, err := BuildKind(out)
	if err != nil {
		return nil, err
	}
	if kind != Release {
		return nil, fmt.Errorf("rc2fx.sh still does not say `release` after rewriting")
	}
	return out, nil
}

func isFile(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.Mode().IsRegular()
}

func isDir(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.IsDir()
}

// SourceDir returns runtime/ in a release folder or target/mixer in a source checkout.
func SourceDir(root string) string {
	runtime := filepath.Join(root, "runtime")
	// In a source checkout, never let a leftover runtime/ directory shadow target/mixer.
	if !isFile(filepath.Join(root, "Cargo.toml")) && isDir(runtime) {
		return runtime
	}
	return filepath.Join(root, "target", "mixer")
}

// Read loads and validates a payload from a checkout or release folder.
// When release is true, rc2fx.sh is rewritten to the release build line.
func Read(root string, release bool) (Files, error) {
	source := SourceDir(root)
	files := make(Files, 0, len(PayloadNames))
	for _, name := range PayloadNames {
		script := IsScript(name)
		path := filepath.Join(source, name)
		if script {
			path = filepath.Join(root, "mixer", "scripts", name)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, fmt.Errorf("runtime bundle missing %s: %v", path, err)
		}
		if script {
			data = CRLFToLF(data)
			if name == "rc2fx.sh" {
				if _, err := BuildKind(data); err != nil {
					return nil, err
				}
				if release {
					if data, err = ReleaseScript(data); err != nil {
						return nil, err
					}
				}
			}
		}
		files = append(files, File{Name: name, Data: data})
	}
	catalogPath := filepath.Join(root, "config", "firmware-profiles.json")
	installerData, err := os.ReadFile(catalogPath)
	if err != nil {
		return nil, fmt.Errorf("installer catalog missing %q: %v", catalogPath, err)
	}
	installer, _ := sjson.Value(installerData)
	bundledData, err := dataOf(files, "firmware-profiles.json")
	if err != nil {
		return nil, err
	}
	bundled, _ := sjson.Value(bundledData)
	if !ValueEqual(installer, bundled) {
		return nil, fmt.Errorf("runtime and installer catalogs differ")
	}
	// Ensure the runtime wasn't built against a different mixer-release.json.
	releasePath := filepath.Join(root, "config", ReleaseManifest)
	sourceRelease, err := os.ReadFile(releasePath)
	if err != nil {
		return nil, fmt.Errorf("mixer release manifest missing %q: %v", releasePath, err)
	}
	carried, err := dataOf(files, ReleaseManifest)
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(CRLFToLF(sourceRelease), carried) {
		return nil, fmt.Errorf("the runtime's %s differs from config/%s; rebuild the mixer runtime", ReleaseManifest, ReleaseManifest)
	}
	if err := Validate(files); err != nil {
		return nil, err
	}
	return files, nil
}

// Validate verifies payload completeness, SHA256SUMS, ARM64 ELF headers,
// firmware catalog schema, mixer-release.json, and rc2fx.sh's build line.
func Validate(files Files) error {
	if len(files) != len(PayloadNames) {
		return fmt.Errorf("incomplete runtime bundle")
	}
	for i, want := range PayloadNames {
		if files[i].Name != want {
			return fmt.Errorf("runtime missing %s", want)
		}
	}
	var manifest strings.Builder
	for _, name := range Pinned {
		data, err := dataOf(files, name)
		if err != nil {
			return err
		}
		fmt.Fprintf(&manifest, "%s  %s\n", common.SHA256Hex(data), name)
	}
	sums, err := dataOf(files, "SHA256SUMS")
	if err != nil {
		return err
	}
	if manifest.String() != string(sums) {
		return fmt.Errorf("runtime checksum mismatch")
	}
	for _, name := range Binaries {
		b, err := dataOf(files, name)
		if err != nil {
			return err
		}
		if len(b) < 64 || !bytes.Equal(b[:6], []byte{127, 'E', 'L', 'F', 2, 1}) || b[18] != 0xb7 || b[19] != 0 {
			return fmt.Errorf("%s is not ARM64 ELF", name)
		}
	}
	data, err := dataOf(files, "firmware-profiles.json")
	if err != nil {
		return err
	}
	// Check JSON syntax before schema decoding for clearer error reporting.
	if err := sjson.Valid(data); err != nil {
		return fmt.Errorf("the runtime catalog is not JSON: %v", err)
	}
	var catalog Catalog
	if err := sjson.Unmarshal(data, &catalog); err != nil {
		return fmt.Errorf("the runtime catalog is not what this installer supports: %v", err)
	}
	if catalog.Schema != 1 {
		return fmt.Errorf("unsupported runtime catalog")
	}
	manifestData, err := dataOf(files, ReleaseManifest)
	if err != nil {
		return err
	}
	if _, err := ReleaseVersion(manifestData); err != nil {
		return err
	}
	script, err := dataOf(files, "rc2fx.sh")
	if err != nil {
		return err
	}
	_, err = BuildKind(script)
	return err
}

// AsU64 parses a json.Number as an exact uint64 (no fraction or exponent; "-0" is 0).
func AsU64(v any) (uint64, bool) {
	n, ok := v.(json.Number)
	if !ok || strings.ContainsAny(string(n), ".eE") {
		return 0, false
	}
	if strings.HasPrefix(string(n), "-") {
		i, err := strconv.ParseInt(string(n), 10, 64)
		if err != nil || i != 0 {
			return 0, false
		}
		return 0, true
	}
	u, err := strconv.ParseUint(string(n), 10, 64)
	return u, err == nil
}

// ReleaseVersion validates mixer-release.json (matching core.ParseMixerRelease)
// and returns its version string.
func ReleaseVersion(data []byte) (string, error) {
	value, err := sjson.Value(data)
	if err != nil {
		return "", fmt.Errorf("%s is not JSON: %v", ReleaseManifest, err)
	}
	object, ok := value.(map[string]any)
	if !ok {
		return "", fmt.Errorf("%s is not an object", ReleaseManifest)
	}
	keys := make([]string, 0, len(object))
	for key := range object {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	if !slices.Equal(keys, []string{"protocolVersion", "schemaVersion", "version"}) {
		return "", fmt.Errorf("%s must have exactly schemaVersion, version and protocolVersion", ReleaseManifest)
	}
	if schema, ok := AsU64(object["schemaVersion"]); !ok || schema != ReleaseSchema {
		return "", fmt.Errorf("unsupported %s schema", ReleaseManifest)
	}
	if p, ok := AsU64(object["protocolVersion"]); !ok || p < 1 || p > 0xffffffff {
		return "", fmt.Errorf("%s protocolVersion must be a positive integer", ReleaseManifest)
	}
	version, ok := object["version"].(string)
	valid := ok && version != ""
	if valid {
		for _, part := range strings.Split(version, ".") {
			if part == "" || len(part) > 9 || strings.Trim(part, "0123456789") != "" {
				valid = false
			}
		}
	}
	if !valid {
		return "", fmt.Errorf("%s version must be dotted numeric", ReleaseManifest)
	}
	return version, nil
}

func dataOf(files Files, name string) ([]byte, error) {
	for _, f := range files {
		if f.Name == name {
			return f.Data, nil
		}
	}
	return nil, fmt.Errorf("runtime missing %s", name)
}

// Body encodes files deterministically: one "<sha256>  <size>  <name>\n" header
// line per file in PayloadNames order, followed by the concatenated file bytes.
func Body(files Files) []byte {
	total := 0
	for _, f := range files {
		total += len(f.Data)
	}
	out := make([]byte, 0, total+96*len(files))
	for _, f := range files {
		out = fmt.Appendf(out, "%s  %d  %s\n", common.SHA256Hex(f.Data), len(f.Data), f.Name)
	}
	for _, f := range files {
		out = append(out, f.Data...)
	}
	return out
}

// Wrap frames body with Magic, length, and SHA-256 headers.
func Wrap(body []byte) []byte {
	out := make([]byte, 0, len(Magic)+96+len(body))
	out = append(out, Magic...)
	out = fmt.Appendf(out, "length %d\n", len(body))
	out = fmt.Appendf(out, "sha256 %s\n", common.SHA256Hex(body))
	return append(out, body...)
}

// Envelope is a parsed and checksum-verified embedded payload.
type Envelope struct {
	blob       []byte
	start, end int
}

// Standalone parses a standalone payload blob with no prefix or trailing bytes.
// Checks structure and checksums only, not publisher signatures.
func Standalone(data []byte) (*Envelope, error) {
	if len(data) > maxBody+256 || !bytes.HasPrefix(data, []byte(Magic)) {
		return nil, fmt.Errorf("invalid standalone mixer payload header or size")
	}
	envelope, err := parseAt(data, 0)
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(envelope.AsBytes(), data) {
		return nil, fmt.Errorf("standalone mixer payload has trailing bytes")
	}
	if _, err := envelope.Bundle(); err != nil {
		return nil, err
	}
	return envelope, nil
}

func Expected(root string, release bool) (*Envelope, error) {
	files, err := Read(root, release)
	if err != nil {
		return nil, err
	}
	return Find(Wrap(Body(files)))
}

// Find locates the single valid payload envelope inside an executable image.
// Because Magic may appear in unrelated data, every occurrence is parsed and
// exactly one valid envelope is required.
func Find(executable []byte) (*Envelope, error) {
	var valid []*Envelope
	candidates := 0
	var damaged error
	for _, at := range occurrences(executable, []byte(Magic)) {
		candidates++
		envelope, err := parseAt(executable, at)
		if err != nil {
			if damaged == nil {
				damaged = err
			}
			continue
		}
		valid = append(valid, envelope)
	}
	switch {
	case len(valid) == 1:
		return valid[0], nil
	case len(valid) == 0 && candidates == 0:
		return nil, fmt.Errorf("the executable does not embed an installer payload; rebuild it from a staged payload")
	case len(valid) == 0:
		return nil, fmt.Errorf("the embedded installer payload is damaged: %v", damaged)
	}
	return nil, fmt.Errorf("the executable embeds %d installer payloads; exactly one is required", len(valid))
}

func (e *Envelope) AsBytes() []byte { return e.blob }

func (e *Envelope) Body() []byte { return e.blob[e.start:e.end] }

// Files extracts and checksum-verifies all files in PayloadNames order.
func (e *Envelope) Files() (Files, error) {
	rest := e.Body()
	type pin struct {
		name, digest string
		size         int
	}
	pinned := make([]pin, 0, len(PayloadNames))
	for _, name := range PayloadNames {
		end := bytes.IndexByte(rest, '\n')
		if end < 0 {
			return nil, fmt.Errorf("the embedded payload has no manifest line for %s", name)
		}
		if !utf8.Valid(rest[:end]) {
			return nil, fmt.Errorf("the embedded payload's manifest is not UTF-8")
		}
		line := string(rest[:end])
		rest = rest[end+1:]
		parts := strings.SplitN(line, "  ", 3)
		if len(parts) != 3 {
			return nil, fmt.Errorf("the embedded payload's manifest is malformed: %s", line)
		}
		digest, size, entry := parts[0], parts[1], parts[2]
		if entry != name {
			return nil, fmt.Errorf("the embedded payload has %s where %s belongs", entry, name)
		}
		if len(digest) != 64 || !common.IsHex(digest) {
			return nil, fmt.Errorf("%s's embedded checksum is malformed", name)
		}
		n, err := parseUsize(size)
		if err != nil {
			return nil, fmt.Errorf("%s's embedded size `%s` is not a number", name, size)
		}
		pinned = append(pinned, pin{name, digest, n})
	}
	files := make(Files, 0, len(pinned))
	for _, p := range pinned {
		if p.size > len(rest) {
			return nil, fmt.Errorf("%s is truncated in the embedded payload", p.name)
		}
		data := rest[:p.size]
		if common.SHA256Hex(data) != p.digest {
			return nil, fmt.Errorf("%s does not match its embedded checksum", p.name)
		}
		files = append(files, File{Name: p.name, Data: data})
		rest = rest[p.size:]
	}
	if len(rest) != 0 {
		return nil, fmt.Errorf("the embedded payload has bytes after its last file")
	}
	return files, nil
}

func parseUsize(text string) (int, error) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" || strings.Trim(digits, "0123456789") != "" {
		return 0, fmt.Errorf("invalid digit")
	}
	n, err := strconv.ParseUint(digits, 10, 63)
	return int(n), err
}

// Bundle validates the envelope and returns a copy of its files keyed by name.
func (e *Envelope) Bundle() (map[string][]byte, error) {
	files, err := e.Files()
	if err != nil {
		return nil, err
	}
	if err := Validate(files); err != nil {
		return nil, err
	}
	out := make(map[string][]byte, len(files))
	for _, f := range files {
		out[f.Name] = slices.Clone(f.Data)
	}
	return out, nil
}

// RequireMatches verifies byte-for-byte equality with expected, naming any
// differing files on mismatch.
func (e *Envelope) RequireMatches(expected *Envelope) error {
	if bytes.Equal(e.blob, expected.blob) {
		return nil
	}
	var differing []string
	got, err1 := e.Files()
	want, err2 := expected.Files()
	if err1 == nil && err2 == nil {
		for i := range min(len(got), len(want)) {
			if !bytes.Equal(got[i].Data, want[i].Data) {
				differing = append(differing, want[i].Name)
			}
		}
	}
	if len(differing) == 0 {
		differing = append(differing, "(the payload's length or order)")
	}
	return fmt.Errorf("the executable's embedded payload is not this build's: differs in %s", strings.Join(differing, ", "))
}

func occurrences(haystack, needle []byte) []int {
	var found []int
	from := 0
	for {
		at := bytes.Index(haystack[from:], needle)
		if at < 0 {
			return found
		}
		found = append(found, from+at)
		from += at + 1
	}
}

func parseAt(data []byte, at int) (*Envelope, error) {
	used := 0
	offset := at + len(Magic)
	length, n, err := header(data[offset:], "length ")
	if err != nil {
		return nil, err
	}
	used += n
	digest, n, err := header(data[offset+used:], "sha256 ")
	if err != nil {
		return nil, err
	}
	used += n
	if length == "" || strings.Trim(length, "0123456789") != "" {
		return nil, fmt.Errorf("the embedded payload's length `%s` is not a number", length)
	}
	size, err := strconv.ParseUint(length, 10, 63)
	if err != nil {
		return nil, fmt.Errorf("the embedded payload's length `%s` is not a number", length)
	}
	if size > maxBody {
		return nil, fmt.Errorf("the embedded payload is implausibly large")
	}
	if len(digest) != 64 || !common.IsHex(digest) {
		return nil, fmt.Errorf("the embedded payload's digest is malformed")
	}
	bodyAt := offset + used
	end := bodyAt + int(size)
	if end > len(data) {
		return nil, fmt.Errorf("the embedded payload is truncated")
	}
	if common.SHA256Hex(data[bodyAt:end]) != digest {
		return nil, fmt.Errorf("the embedded payload does not match its digest")
	}
	envelope := &Envelope{
		blob:  slices.Clone(data[at:end]),
		start: bodyAt - at,
		end:   end - at,
	}
	if _, err := envelope.Files(); err != nil {
		return nil, err
	}
	return envelope, nil
}

func header(data []byte, prefix string) (string, int, error) {
	end := bytes.IndexByte(data, '\n')
	if end < 0 {
		return "", 0, fmt.Errorf("the embedded payload header is truncated")
	}
	if !utf8.Valid(data[:end]) {
		return "", 0, fmt.Errorf("the embedded payload header is not UTF-8")
	}
	line := string(data[:end])
	value, ok := strings.CutPrefix(line, prefix)
	if !ok {
		return "", 0, fmt.Errorf("the embedded payload header has no `%s` line", prefix)
	}
	return value, end + 1, nil
}

// ValueEqual compares sjson.Value trees ignoring object key order. Integer and
// float json.Numbers never compare equal to each other.
func ValueEqual(a, b any) bool {
	switch a := a.(type) {
	case nil:
		return b == nil
	case bool:
		bb, ok := b.(bool)
		return ok && a == bb
	case string:
		bb, ok := b.(string)
		return ok && a == bb
	case json.Number:
		bb, ok := b.(json.Number)
		return ok && numberEqual(a, bb)
	case []any:
		bb, ok := b.([]any)
		if !ok || len(a) != len(bb) {
			return false
		}
		for i := range a {
			if !ValueEqual(a[i], bb[i]) {
				return false
			}
		}
		return true
	case map[string]any:
		bb, ok := b.(map[string]any)
		if !ok || len(a) != len(bb) {
			return false
		}
		for k, v := range a {
			w, ok := bb[k]
			if !ok || !ValueEqual(v, w) {
				return false
			}
		}
		return true
	}
	return false
}

func numberEqual(a, b json.Number) bool {
	af, bf := strings.ContainsAny(string(a), ".eE"), strings.ContainsAny(string(b), ".eE")
	if af != bf {
		return false
	}
	if af {
		x, err1 := a.Float64()
		y, err2 := b.Float64()
		return err1 == nil && err2 == nil && x == y
	}
	if x, err := strconv.ParseInt(string(a), 10, 64); err == nil {
		y, err := strconv.ParseInt(string(b), 10, 64)
		return err == nil && x == y
	}
	x, err1 := strconv.ParseUint(string(a), 10, 64)
	y, err2 := strconv.ParseUint(string(b), 10, 64)
	if err1 == nil && err2 == nil {
		return x == y
	}
	// Integers beyond 64 bits compare as floats.
	xf, err1 := a.Float64()
	yf, err2 := b.Float64()
	return err1 == nil && err2 == nil && xf == yf
}
