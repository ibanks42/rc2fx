package licenses

import (
	_ "embed"
	"fmt"
	"sync"
)

// licenses.json in the repo is a dev-only placeholder (xtask packaging rejects
// binaries that still contain it). Release builds swap in generated notices via
// `go build -overlay`, leaving the tracked file untouched. Manual regeneration
// via `go generate ./app/licenses` (with RC2FX_RELEASE_BUILD=1 and
// RC2FX_LICENSES_JSON) is also supported by gen.go.
//
//go:generate go run gen.go

//go:embed licenses.json
var embedded string

var sections = sync.OnceValue(func() []Section {
	parsed, err := Parse(embedded)
	if err != nil {
		return []Section{{
			Title: "Licenses unavailable",
			Text:  fmt.Sprintf("The license text in this build could not be read: %s", err),
		}}
	}
	return parsed
})

// Sections returns the parsed license sections, falling back to an error entry
// rather than panicking if the embedded JSON is malformed.
func Sections() []Section {
	return sections()
}
