package embed

import (
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
)

// mixer/scripts/install.sh runs under the mixer's /bin/sh and maintains its
// own copy of the payload file list.
func installSh(t *testing.T) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "mixer", "scripts", "install.sh"))
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

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

// install.sh should chmod 700 all scripts and non-.so binaries.
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
