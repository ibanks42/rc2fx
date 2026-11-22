package selfupdate

import (
	"encoding/xml"
	"errors"
	"io"
	"os"
	"regexp"
	"runtime"
)

// LocatorConfig mirrors Velopack's VelopackLocatorConfig paths.
type LocatorConfig struct {
	RootAppDir       string // app root directory, or AppImage path on Linux
	UpdateExePath    string // Update.exe, UpdateMac, or UpdateNix
	PackagesDir      string
	ManifestPath     string // sq.version path
	CurrentBinaryDir string
	IsPortable       bool
}

type manifest struct {
	ID      string
	Version string
	MainExe string
}

// Matches strict semver (major.minor.patch with optional prerelease/build).
var semver = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)` +
	`(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$`)

// readManifest parses Velopack's sq.version nuspec file using the same lenient
// rules as Velopack itself: the last matching element wins, id and a non-zero
// version are required, and Windows also requires mainExe.
func readManifest(path string) (manifest, error) {
	file, err := os.Open(path)
	if err != nil {
		return manifest{}, err
	}
	defer file.Close()
	var m manifest
	var stack []string
	decoder := xml.NewDecoder(io.LimitReader(file, 1<<20))
	for {
		token, err := decoder.Token()
		if err != nil {
			// Velopack stops at the first error and judges what it read.
			break
		}
		switch t := token.(type) {
		case xml.StartElement:
			stack = append(stack, t.Name.Local)
		case xml.EndElement:
			if len(stack) > 0 {
				stack = stack[:len(stack)-1]
			}
		case xml.CharData:
			if len(stack) == 0 {
				continue
			}
			text := string(t)
			switch stack[len(stack)-1] {
			case "id":
				m.ID = text
			case "version":
				if !semver.MatchString(text) {
					return manifest{}, errors.New("Semver parse error: " + text)
				}
				m.Version = text
			case "mainExe":
				m.MainExe = text
			}
		}
	}
	switch {
	case m.ID == "":
		return manifest{}, errors.New("Missing required manifest property: id")
	case m.Version == "" || m.Version == "0.0.0":
		return manifest{}, errors.New("Missing required manifest property: version")
	case runtime.GOOS == "windows" && m.MainExe == "":
		return manifest{}, errors.New("Missing required manifest property: mainExe")
	}
	return m, nil
}
