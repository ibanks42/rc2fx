//go:build ignore

// gen writes licenses.json, the bundle the licenses package embeds, the way
// the Rust app's build.rs did: RC2FX_LICENSES_JSON names the generated file,
// and with RC2FX_RELEASE_BUILD=1 a valid one is required. Without one, a
// development build embeds the placeholder.
//
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
