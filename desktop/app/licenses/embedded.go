package licenses

import (
	_ "embed"
	"fmt"
	"sync"
)

// licenses.json is what this build shows. The copy in the repository is
// Placeholder, for development builds (xtask packaging refuses an
// executable that still carries it); release packaging substitutes the
// generated notices at build time with `go build -overlay`, leaving the
// tracked placeholder file untouched on disk. Manual regeneration via
// `go generate ./app/licenses` (with RC2FX_RELEASE_BUILD=1 and
// RC2FX_LICENSES_JSON) remains supported by gen.go.
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

// Sections is the embedded sections, parsed once. The generator already
// refused an invalid file, so a failure here means a broken build; say so
// instead of panicking.
func Sections() []Section {
	return sections()
}
