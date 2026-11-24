package licenses

import (
	"errors"
	"fmt"
	"os"
	"unicode/utf8"
)

// Placeholder is embedded in development builds when no license bundle is provided.
const Placeholder = `{"schemaVersion":1,"sections":[{"title":"Licenses not bundled in this development build","text":"This development build does not include the license notices. Released builds show the full RC2FX license and every third-party notice here."}]}`

// Resolve returns the license JSON to embed. Release builds require a valid
// file at path; dev builds with an empty path fall back to Placeholder.
func Resolve(release bool, path string) (string, error) {
	if path == "" {
		if release {
			return "", errors.New("RC2FX_LICENSES_JSON is not set; release builds must embed the generated license file (run `go -C tools/xtask run . package`)")
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
