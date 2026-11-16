package licenses

import (
	_ "embed"
	"fmt"
	"sync"
)

// licenses.json is what this build shows. The copy in the repository is
// Placeholder, for development builds; release packaging regenerates it from
// the generated notices before building:
//
//	RC2FX_RELEASE_BUILD=1 RC2FX_LICENSES_JSON=/path/to/licenses.json go generate ./app/licenses
//
// gen.go checks the file the way build.rs did and refuses a release build
// without one.
//
//go:generate go run gen.go

//go:embed licenses.json
var embedded string

// Embedded is the license bundle this build carries, as generated.
func Embedded() string {
	return embedded
}

// IsPlaceholder reports a build that carries Placeholder rather than the
// notices; release packaging can refuse such a build.
func IsPlaceholder() bool {
	return embedded == Placeholder
}

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
