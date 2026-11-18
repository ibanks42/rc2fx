// Command stage-payload checks the installer payload in a source checkout or
// a release folder and writes the envelope the `embedded` build tag compiles
// in, at installer/embed/payload/rc2fx-payload.bin.
//
// It checks every file's checksum, the
// SHA256SUMS manifest, the ARM64 ELFs, the catalog and the single
// `rc2fx_build=` line. A missing or corrupt payload is an error, so a desktop
// build never silently loses its installer. RC2FX_RELEASE_BUILD=1 applies the
// release build line.
package main

import (
	"bytes"
	"flag"
	"fmt"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
)

func main() {
	root := flag.String("root", "..", "the repository root or release folder")
	out := flag.String("out", filepath.Join("installer", "embed", "payload", "rc2fx-payload.bin"), "where to write the envelope")
	flag.Parse()
	release := os.Getenv("RC2FX_RELEASE_BUILD") == "1"
	files, err := embed.Read(*root, release)
	if err != nil {
		fail(err.Error())
	}
	envelope := embed.Wrap(embed.Body(files))
	// Prove the envelope this build embeds reads back as exactly one payload
	// before compiling it in.
	found, err := embed.Find(envelope)
	if err != nil {
		fail(err.Error())
	}
	if !bytes.Equal(found.AsBytes(), envelope) {
		fail("the embedded payload does not read back as written")
	}
	if err := os.MkdirAll(filepath.Dir(*out), 0o755); err != nil {
		fail(fmt.Sprintf("could not create %s: %v", filepath.Dir(*out), err))
	}
	if err := os.WriteFile(*out, envelope, 0o644); err != nil {
		fail(fmt.Sprintf("could not write %s: %v", *out, err))
	}
}

// A missing or damaged payload is fatal: the desktop app embeds its
// installer, so there is no runtime fallback to files beside it.
func fail(why string) {
	fmt.Fprintf(os.Stderr, "\n\ncannot embed the installer payload: %s\n\n"+
		"The desktop app installs RC2FX from this payload, so it must be present\n"+
		"and valid when the app is built.\n\n"+
		"In a source checkout the payload is target/mixer (runtime) and\n"+
		"mixer/scripts (scripts), with config/firmware-profiles.json and\n"+
		"config/mixer-release.json:\n"+
		"- `./xtask mixer-build` builds and stages it;\n"+
		"- if target/mixer is a promoted link to a verified bundle, keep it and\n"+
		"  do not run a generic rebuild: `./xtask mixer install` deploys the\n"+
		"  staged bundle, and the build refuses to replace the link.\n"+
		"In a release folder the same files are runtime/ and mixer/scripts.\n"+
		"Then run `go run ./installer/embed/cmd/stage-payload` in desktop/ and\n"+
		"build with `-tags embedded`.\n", why)
	os.Exit(1)
}
