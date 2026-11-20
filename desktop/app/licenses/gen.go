//go:build ignore

// Writes licenses.json for go:embed. Reads RC2FX_LICENSES_JSON (required when
// RC2FX_RELEASE_BUILD=1) or falls back to the dev placeholder.
// Run with `go generate ./app/licenses` from desktop/.
package main

import (
	"fmt"
	"os"

	"github.com/ibanks42/rc2fx/desktop/app/licenses"
)

func main() {
	release := os.Getenv("RC2FX_RELEASE_BUILD") == "1"
	text, err := licenses.Resolve(release, os.Getenv("RC2FX_LICENSES_JSON"))
	if err != nil {
		fmt.Fprintf(os.Stderr, "cannot embed licenses: %s\n", err)
		os.Exit(1)
	}
	if err := os.WriteFile("licenses.json", []byte(text), 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "write licenses.json: %s\n", err)
		os.Exit(1)
	}
}
