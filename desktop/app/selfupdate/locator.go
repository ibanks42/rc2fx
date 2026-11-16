package selfupdate

import (
	"encoding/xml"
	"errors"
	"io"
	"os"
	"regexp"
	"runtime"
)

// LocatorConfig is velopack's VelopackLocatorConfig: the paths Velopack uses
// for this copy.
type LocatorConfig struct {
	// RootAppDir is the root directory of the app, or the AppImage file on
	// Linux.
	RootAppDir string
	// UpdateExePath is Velopack's updater (Update.exe, UpdateMac,
	// UpdateNix).
	UpdateExePath string
	PackagesDir   string
	// ManifestPath is the app's sq.version.
	ManifestPath     string
	CurrentBinaryDir string
	IsPortable       bool
}

// manifest is the part of Velopack's sq.version (a nuspec) these paths
// need.
type manifest struct {
	ID      string
	Version string
	MainExe string
}

// semver is what semver::Version::parse accepts, closely enough to refuse
// what it refuses: three numbers without leading zeros, then an optional
// pre-release and build.
var semver = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)` +
	`(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$`)

// readManifest is velopack's read_current_manifest and
// read_manifest_from_string: the text of the last element of each name
// counts, wherever it is; id and a non-zero version are required, and on
// Windows mainExe.
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

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}
