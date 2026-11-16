package licenses

// Which license text a build embeds. Used by the generator (gen.go); tested
// here.

import (
	"errors"
	"fmt"
	"os"
	"unicode/utf8"
)

// Placeholder is embedded when a development build is given no license file.
// It says so plainly rather than looking like the full notices.
const Placeholder = `{"schemaVersion":1,"sections":[{"title":"Licenses not bundled in this development build","text":"This development build does not include the license notices. Released builds show the full RC2FX license and every third-party notice here."}]}`

// Resolve is the JSON to embed. A release build (release) must be given a
// valid file; a development build without one (path "") gets Placeholder.
func Resolve(release bool, path string) (string, error) {
	if path == "" {
		if release {
			return "", errors.New("RC2FX_LICENSES_JSON is not set; release builds must embed the generated license file (run `./xtask package`)")
		}
		return Placeholder, nil
	}
	data, err := os.ReadFile(path)
	if err == nil && !utf8.Valid(data) {
		err = errors.New("stream did not contain valid UTF-8")
	}
	if err != nil {
		return "", fmt.Errorf("cannot read license file %s: %w", path, err)
	}
	text := string(data)
	if _, err := Parse(text); err != nil {
		return "", fmt.Errorf("invalid license file %s: %w", path, err)
	}
	return text, nil
}
