//go:build unix

package mixer

// Tests for the optional pre_command hook: mutating mixer commands run it
// before sending any SSH command (and abort if it fails), skip it when
// unconfigured, and read-only commands never run it.

import (
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/setup"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const fakeSSH = `fake=$(dirname "$0"); remote=${!#}
echo "ssh $remote" >> "$fake/events"
cat > /dev/null
`

const fakeHook = `#!/bin/bash
fake=$(dirname "$0")
echo "hook $1 $RC2FX_MIXER_HOST" >> "$fake/events"
if test -e "$fake/hook-fails"; then
  echo 'hook refused' >&2
  exit 7
fi
`

var mutatingCommands = []struct {
	command []string
	action  string
	first   string
}{
	{[]string{"mixer", "run"}, "mixer run", "ssh set -e; rm -rf /run/rc2fx-upload;"},
	{[]string{"mixer", "install"}, "mixer install", "ssh set -e; rm -rf /run/rc2fx-upload;"},
	{[]string{"mixer", "uninstall"}, "mixer uninstall", "ssh /bin/sh -s"},
	{[]string{"mixer", "uninstall", "--purge"}, "mixer uninstall", "ssh /bin/sh -s"},
	{[]string{"mixer", "stop"}, "mixer stop", "ssh /bin/sh -s"},
	{[]string{"mixer", "enable"}, "mixer enable", "ssh /bin/sh -s"},
	{[]string{"mixer", "disable"}, "mixer disable", "ssh /bin/sh -s"},
	{[]string{"mixer", "xruns", "0"}, "mixer xruns", "ssh /bin/sh -s"},
	{[]string{"bench"}, "bench", "ssh set -e; rm -rf /run/rc2fx-bench;"},
}

type fake struct {
	t   *testing.T
	dir string
}

func newFake(t *testing.T, hookFails bool) *fake {
	t.Helper()
	dir := t.TempDir()
	write := func(name string, data []byte, mode os.FileMode) {
		if err := os.WriteFile(filepath.Join(dir, name), data, mode); err != nil {
			t.Fatal(err)
		}
	}
	root := task.Root()
	var sums strings.Builder
	for _, name := range embed.Pinned {
		data := []byte(name)
		switch name {
		case "firmware-profiles.json":
			catalog, err := setup.DefaultCatalog(root)
			if err != nil {
				t.Fatal(err)
			}
			data = []byte(catalog)
		case "mixer-release.json":
			release, err := os.ReadFile(filepath.Join(root, "config", "mixer-release.json"))
			if err != nil {
				t.Fatal(err)
			}
			data = release
		}
		fmt.Fprintf(&sums, "%s  %s\n", common.SHA256Hex(data), name)
		write(name, data, 0o666)
	}
	write("SHA256SUMS", []byte(sums.String()), 0o666)
	write("rc2fx-bench", []byte("placeholder"), 0o666)
	write("speech.wav", []byte("RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\xbb\x00\x00\x00\x77\x01\x00\x02\x00\x10\x00data\x00\x00\x00\x00"), 0o666)
	write("ssh", []byte(fakeSSH), 0o666)
	write("hook", []byte(fakeHook), 0o755)
	if hookFails {
		write("hook-fails", nil, 0o666)
	}
	return &fake{t, dir}
}

func (f *fake) run(command []string, withHook bool) error {
	pre := ""
	if withHook {
		pre = filepath.Join(f.dir, "hook")
	}
	m := &Mixer{
		ip:         "fake",
		user:       "root",
		host:       "root@fake",
		preCommand: pre,
		options:    []string{filepath.Join(f.dir, "ssh")},
		program:    "bash",
	}
	if len(command) == 1 && command[0] == "bench" {
		return benchIn(f.dir, func() (*Mixer, error) { return m, nil }, filepath.Join(f.dir, "speech.wav"), nil)
	}
	return act(m, f.dir, command[1], command[2:])
}

func (f *fake) events() []string {
	data, err := os.ReadFile(filepath.Join(f.dir, "events"))
	if err != nil {
		f.t.Fatal(err)
	}
	return common.Lines(string(data))
}

func TestMutatingCommandsSkipHookWhenUnconfigured(t *testing.T) {
	for _, c := range mutatingCommands {
		f := newFake(t, false)
		_ = f.run(c.command, false)
		events := f.events()
		if !(len(events) >= 1 && strings.HasPrefix(events[0], c.first)) {
			t.Errorf("%v: %q", c.command, events)
		}
	}
}

func TestMutatingCommandsRunPreCommandBeforeSSH(t *testing.T) {
	for _, c := range mutatingCommands {
		f := newFake(t, false)
		_ = f.run(c.command, true)
		events := f.events()
		wantHook := "hook " + c.action + " root@fake"
		if !(len(events) >= 2 && events[0] == wantHook && strings.HasPrefix(events[1], c.first)) {
			t.Errorf("%v: %q", c.command, events)
		}
	}
}

func TestFailingPreCommandAbortsBeforeSSH(t *testing.T) {
	for _, c := range mutatingCommands {
		f := newFake(t, true)
		err := f.run(c.command, true)
		if err == nil {
			t.Fatalf("%v: not aborted", c.command)
		}
		if !strings.Contains(err.Error(), "pre_command failed") {
			t.Errorf("%v: %s", c.command, err)
		}
		wantHook := "hook " + c.action + " root@fake"
		if events := f.events(); !slices.Equal(events, []string{wantHook}) {
			t.Errorf("%v: %q", c.command, events)
		}
	}
}

func TestStatusAndLogsNeverRunPreCommand(t *testing.T) {
	for _, action := range []string{"status", "logs"} {
		f := newFake(t, true)
		if err := f.run([]string{"mixer", action}, true); err != nil {
			t.Fatal(err)
		}
		if events := f.events(); !slices.Equal(events, []string{"ssh /bin/sh -s"}) {
			t.Errorf("%s: %q", action, events)
		}
	}
}
