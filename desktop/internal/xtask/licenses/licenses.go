// Package licenses builds the one notice document that feeds the app's
// embedded licence dialog (desktop/app/licenses), the Linux package's HTML
// and a standalone mixer release's THIRD-PARTY-NOTICES.html.
package licenses

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// VPKVersion is the vpk release packaging is written for. Changing it needs
// a new licenses/velopack-VERSION.json, covering the Windows, macOS and
// Linux (musl) builds of the programs vpk adds
// (.github/scripts/velopack-licenses.py).
const VPKVersion = "1.3.39-g8d2844f"

// Section is one notice: a title and its text.
type Section struct {
	Title string
	Text  string
}

// Document is the notices as the app's JSON bundle and as HTML.
type Document struct {
	JSON []byte
	HTML []byte
}

// Limits for reports read here: cargo-about's JSON carries every licence
// text.
const (
	maxReportBytes = 256 << 20
	maxReportNodes = 8_000_000
)

func readText(root, path string) (string, error) {
	data, err := os.ReadFile(filepath.Join(root, path))
	if err == nil && !utf8.Valid(data) {
		return "", task.Errorf("%s: stream did not contain valid UTF-8", path)
	}
	if err != nil {
		return "", task.Errorf("%s: %s", path, task.IOError(err))
	}
	return string(data), nil
}

// Generate is the notice document: RC2FX's and its bundled components'
// licences, the Rust standard library's, every crate cargo-about reports
// for the workspace and the native library, Velopack's report, then extra.
func Generate(root string, extra []Section) (*Document, error) {
	var sections []Section
	for _, fixed := range []struct{ title, path string }{
		{"RC2FX and trained weights — MIT", "LICENSE-MIT"},
		{"DeepFilterNet — MIT", "crates/rc2fx-dfn/LICENSE-DEEPFILTERNET"},
		{"Lucide and Feather icons — ISC / MIT", "licenses/LICENSE-LUCIDE"},
		{"Velopack — MIT", "licenses/LICENSE-VELOPACK"},
		{"Third-party notices", "THIRD_PARTY_NOTICES.md"},
		{"Trained on", "licenses/TRAINING-CREDITS.txt"},
	} {
		text, err := readText(root, fixed.path)
		if err != nil {
			return nil, err
		}
		sections = append(sections, Section{fixed.title, text})
	}
	std, err := rustStandardLibrary()
	if err != nil {
		return nil, err
	}
	sections = append(sections, Section{"Rust standard library", std})
	// Keep each distinct notice, including its copyright holders; only merge
	// byte-identical texts, never all MIT texts into a generic MIT template.
	groups := Groups{}
	for _, manifest := range []string{"Cargo.toml", "crates/rc2fx-native/Cargo.toml"} {
		command := exec.Command("cargo", "about", "generate", "--locked", "--format", "json",
			"--config", "about.toml", "--manifest-path", manifest)
		command.Dir = root
		var stdout, stderr bytes.Buffer
		command.Stdout = &stdout
		command.Stderr = &stderr
		if err := command.Start(); err != nil {
			return nil, task.Errorf("could not run cargo-about: %s", task.IOError(err))
		}
		_ = command.Wait()
		if !command.ProcessState.Success() {
			return nil, task.Errorf("cargo-about failed for %s: %s", manifest, strings.ToValidUTF8(stderr.String(), "�"))
		}
		report, err := core.ParseJSONBytes(stdout.Bytes(), maxReportBytes, maxReportNodes)
		if err != nil {
			return nil, task.Error(err.Error())
		}
		if err := MergeReport(groups, report); err != nil {
			return nil, err
		}
	}
	// Velopack's launcher, updater and installer ship in the Windows and
	// macOS packages, and its updater (UpdateNix, a static musl build) in the
	// AppImage; their notices come from a report generated from Velopack's
	// source for those three targets by .github/scripts/velopack-licenses.py.
	velopack := "licenses/velopack-" + VPKVersion + ".json"
	report, err := VelopackReport(root)
	if err != nil {
		return nil, err
	}
	if v, _ := report.Get("velopack"); !isString(v, VPKVersion) {
		return nil, task.Errorf("%s is not for vpk %s", velopack, VPKVersion)
	}
	if err := MergeReport(groups, report); err != nil {
		return nil, err
	}
	sections = append(sections, groups.Sections("Dependencies — ")...)
	sections = append(sections, extra...)
	return Render(sections), nil
}

// VelopackReport reads licenses/velopack-VPKVersion.json.
func VelopackReport(root string) (core.Value, error) {
	velopack := "licenses/velopack-" + VPKVersion + ".json"
	data, err := os.ReadFile(filepath.Join(root, velopack))
	if err != nil {
		return core.Value{}, task.Errorf("%s: %s\nRun .github/scripts/velopack-licenses.py for this vpk version.", velopack, task.IOError(err))
	}
	report, err := core.ParseJSONBytes(data, maxReportBytes, maxReportNodes)
	if err != nil {
		return core.Value{}, task.Errorf("%s: %s", velopack, err)
	}
	return report, nil
}

func isString(v core.Value, want string) bool {
	s, ok := v.AsString()
	return ok && s == want
}

func rustStandardLibrary() (string, error) {
	output, err := exec.Command("rustc", "--print", "sysroot").Output()
	if err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			return "", task.Error("rustc could not locate its standard-library notices")
		}
		return "", task.Error(task.IOError(err))
	}
	if !utf8.Valid(output) {
		return "", task.Error("invalid utf-8 sequence")
	}
	path := filepath.Join(strings.TrimSpace(string(output)), "share", "doc", "rust", "COPYRIGHT-library.html")
	html, err := os.ReadFile(path)
	if err != nil {
		return "", task.Errorf("%s: %s; install rustup component add rust-docs", path, task.IOError(err))
	}
	return HTMLText(string(html))
}

// HTMLText reads the pinned toolchain's simple copyright HTML as character
// data. Fail on an unfamiliar entity instead of silently dropping
// attribution text.
func HTMLText(html string) (string, error) {
	_, afterBody, ok := strings.Cut(html, "<body>")
	if !ok {
		return "", task.Error("Rust copyright document has no body")
	}
	body, _, ok := strings.Cut(afterBody, "</body>")
	if !ok {
		return "", task.Error("Rust copyright document has no body")
	}
	var result strings.Builder
	rest := body
	for rest != "" {
		switch rest[0] {
		case '<':
			end := strings.IndexByte(rest, '>')
			if end < 0 {
				return "", task.Error("unterminated HTML tag")
			}
			result.WriteByte('\n')
			rest = rest[end+1:]
		case '&':
			end := strings.IndexByte(rest, ';')
			if end < 0 {
				return "", task.Error("unterminated HTML entity")
			}
			entity := rest[1:end]
			decoded, ok := decodeEntity(entity)
			if !ok {
				return "", task.Errorf("unknown copyright HTML entity %s", entity)
			}
			result.WriteRune(decoded)
			rest = rest[end+1:]
		default:
			end := strings.IndexAny(rest, "<&")
			if end < 0 {
				end = len(rest)
			}
			result.WriteString(rest[:end])
			rest = rest[end:]
		}
	}
	return result.String(), nil
}

func decodeEntity(entity string) (rune, bool) {
	switch entity {
	case "amp":
		return '&', true
	case "lt":
		return '<', true
	case "gt":
		return '>', true
	case "quot":
		return '"', true
	case "apos":
		return '\'', true
	case "nbsp":
		return ' ', true
	}
	var value uint64
	var err error
	switch {
	case strings.HasPrefix(entity, "#x"):
		value, err = strconv.ParseUint(entity[2:], 16, 32)
	case strings.HasPrefix(entity, "#"):
		var ok bool
		value, ok = task.ParseUint(entity[1:], 32)
		if !ok {
			err = errors.New("invalid")
		}
	default:
		return 0, false
	}
	// char::from_u32: no surrogates, nothing past U+10FFFF.
	if err != nil || value > 0x10FFFF || (value >= 0xD800 && value <= 0xDFFF) {
		return 0, false
	}
	return rune(value), true
}

// Groups is notices by licence name, then by exact text, with the
// components that use each.
type Groups map[string]map[string]map[string]bool

// add records that user uses notice text under licence name.
func (g Groups) add(name, text, user string) {
	texts, ok := g[name]
	if !ok {
		texts = map[string]map[string]bool{}
		g[name] = texts
	}
	users, ok := texts[text]
	if !ok {
		users = map[string]bool{}
		texts[text] = users
	}
	if user != "" {
		users[user] = true
	}
}

// Sections is one section per licence name, in name order, titled prefix +
// name: each distinct text with the sorted components that use it.
func (g Groups) Sections(prefix string) []Section {
	var out []Section
	for _, name := range sortedKeys(g) {
		var text strings.Builder
		texts := g[name]
		for _, notice := range sortedKeys(texts) {
			fmt.Fprintf(&text, "Used by: %s\n\n%s\n\n", strings.Join(sortedKeys(texts[notice]), ", "), notice)
		}
		out = append(out, Section{prefix + name, text.String()})
	}
	return out
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

// MergeReport adds a cargo-about report's licences to groups.
func MergeReport(groups Groups, report core.Value) error {
	licensesValue, _ := report.Get("licenses")
	licenses, ok := licensesValue.AsArray()
	if !ok {
		return task.Error("cargo-about report has no licenses")
	}
	if len(licenses) == 0 {
		return task.Error("cargo-about report contains no licenses")
	}
	for _, license := range licenses {
		nameValue, _ := license.Get("name")
		name, ok := nameValue.AsString()
		if !ok {
			return task.Error("license has no name")
		}
		textValue, _ := license.Get("text")
		text, ok := textValue.AsString()
		if !ok || strings.TrimSpace(text) == "" {
			return task.Error("license has no text")
		}
		groups.add(name, text, "")
		usedValue, _ := license.Get("used_by")
		used, ok := usedValue.AsArray()
		if !ok {
			return task.Error("license has no crate list")
		}
		for _, item := range used {
			crate, _ := item.Get("crate")
			crateName, _ := crate.Get("name")
			cname, ok := crateName.AsString()
			if !ok {
				return task.Error("crate has no name")
			}
			crateVersion, _ := crate.Get("version")
			version, ok := crateVersion.AsString()
			if !ok {
				return task.Error("crate has no version")
			}
			groups.add(name, text, cname+" "+version)
		}
	}
	return nil
}

// Render is both formats: the app's JSON bundle and the HTML page.
func Render(sections []Section) *Document {
	escape := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;").Replace
	var html strings.Builder
	html.WriteString("<!doctype html><html><head><meta charset=\"utf-8\"><title>RC2FX licenses and credits</title><style>body{font:16px system-ui;max-width:72em;margin:2em auto;padding:0 1em}pre{white-space:pre-wrap;overflow-wrap:anywhere}</style></head><body><h1>RC2FX licenses and credits</h1>")
	values := []core.Value{}
	for _, s := range sections {
		fmt.Fprintf(&html, "<details><summary>%s</summary><pre>%s</pre></details>", escape(s.Title), escape(s.Text))
		values = append(values, core.ObjectValue(
			core.Member{Key: "title", Value: core.StringValue(s.Title)},
			core.Member{Key: "text", Value: core.StringValue(s.Text)},
		))
	}
	html.WriteString("</body></html>")
	document := core.ObjectValue(
		core.Member{Key: "schemaVersion", Value: core.U64Value(1)},
		core.Member{Key: "sections", Value: core.ArrayValue(values...)},
	)
	return &Document{JSON: []byte(document.String()), HTML: []byte(html.String())}
}

// VerifyEmbedded requires the executable to carry document byte for byte.
func VerifyEmbedded(executable, document []byte) error {
	if len(document) == 0 || !bytes.Contains(executable, document) {
		return task.Error("the executable does not embed the current license document; rebuild with ./xtask package")
	}
	return nil
}

// WriteLicenses is `licenses FILE`: consolidated notices for a standalone
// mixer release.
func WriteLicenses(destination string) error {
	document, err := Generate(task.Root(), nil)
	if err != nil {
		return err
	}
	if parent := filepath.Dir(destination); parent != "" {
		if err := os.MkdirAll(parent, 0o777); err != nil {
			return task.Error(task.IOError(err))
		}
	}
	if err := os.WriteFile(destination, document.HTML, 0o666); err != nil {
		return task.Error(task.IOError(err))
	}
	return nil
}
