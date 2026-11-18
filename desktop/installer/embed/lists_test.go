package embed

import (
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
)

// installSh is mixer/scripts/install.sh, which keeps its own copy of the
// payload's file list: it runs under the mixer's /bin/sh and cannot read
// this package.
func installSh(t *testing.T) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "mixer", "scripts", "install.sh"))
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

// The developer install copies, sizes and stages exactly PayloadNames, in
// the same order; a file added to the payload but not to install.sh would
// be left out of a full-flash or `./xtask mixer install` bundle.
func TestInstallShCopiesExactlyThePayload(t *testing.T) {
	text := installSh(t)
	_, rest, ok := strings.Cut(text, "\nfiles=\"")
	if !ok {
		t.Fatal("install.sh has no files=\"...\" list")
	}
	list, _, ok := strings.Cut(rest, "\"")
	if !ok {
		t.Fatal("install.sh's files list is not closed")
	}
	if got := strings.Fields(list); !slices.Equal(got, PayloadNames[:]) {
		t.Fatalf("install.sh files=\n%v\nPayloadNames\n%v", got, PayloadNames)
	}
}

// install.sh makes exactly the payload's executables and scripts runnable:
// the binaries that are not shared libraries, and every script.
func TestInstallShMakesExactlyThePayloadsExecutablesRunnable(t *testing.T) {
	text := installSh(t)
	_, rest, ok := strings.Cut(text, "\nchmod 700 \"$stage/")
	if !ok {
		t.Fatal("install.sh has no chmod 700 of the stage")
	}
	// The command and its continuation lines.
	command := "\"$stage/"
	for line := range strings.Lines(rest) {
		command += line
		if !strings.HasSuffix(strings.TrimRight(line, "\n"), "\\") {
			break
		}
	}
	var got []string
	for _, match := range regexp.MustCompile(`"\$stage/([^"]+)"`).FindAllStringSubmatch(command, -1) {
		got = append(got, match[1])
	}
	var want []string
	for _, name := range PayloadNames {
		if IsScript(name) || IsBinary(name) && !strings.HasSuffix(name, ".so") {
			want = append(want, name)
		}
	}
	slices.Sort(got)
	slices.Sort(want)
	if !slices.Equal(got, want) {
		t.Fatalf("install.sh chmod 700\n%v\nwant\n%v", got, want)
	}
}
