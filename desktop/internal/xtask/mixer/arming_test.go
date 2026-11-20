//go:build unix

package mixer

// Tests that every mutating mixer command arms and verifies power recovery
// before sending any other remote command, and aborts cleanly if arming fails.

import (
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/setup"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// fakeSSH logs each remote command and fw_* call to `events`. It only actually
// executes `rc2fx.sh arm-power-recovery` against the local `flag` file; marker
// files in the same directory inject failure modes.
const fakeSSH = `fake=$(dirname "$0"); remote=${!#}
echo "ssh $remote" >> "$fake/events"
test ! -e "$fake/unreachable" || { echo 'ssh: connection timed out' >&2; exit 255; }
stubs="fw_printenv() { echo \"fw_printenv \$*\" >> '$fake/events'; cat '$fake/flag'; }
fw_setenv() { echo \"fw_setenv \$*\" >> '$fake/events'; test ! -e '$fake/setenv-fails' && echo \"\$2\" > '$fake/flag'; }"
case "$remote" in
  '/bin/bash -s -- arm-power-recovery')
    { echo "$stubs"; sed "s|/usr/bin/first-boot|$fake/first-boot|"; } | bash -s -- arm-power-recovery ;;
  'fw_printenv -n reboot_pwr_button_bypass') cat "$fake/readback" 2>/dev/null || cat "$fake/flag" ;;
  *) cat > /dev/null ;;
esac
`

// Expected event trace for a clean arm-and-verify starting from flag=0.
var armedEvents = []string{
	"ssh /bin/bash -s -- arm-power-recovery",
	"fw_printenv -n reboot_pwr_button_bypass",
	"fw_setenv reboot_pwr_button_bypass 1",
	"fw_printenv -n reboot_pwr_button_bypass",
	"ssh fw_printenv -n reboot_pwr_button_bypass",
}

// Each command that must arm recovery, paired with the first remote command it
// sends once armed.
var armingCommands = []struct {
	command []string
	first   string
}{
	{[]string{"mixer", "run"}, "ssh set -e; rm -rf /run/rc2fx-upload;"},
	{[]string{"mixer", "install"}, "ssh set -e; rm -rf /run/rc2fx-upload;"},
	{[]string{"mixer", "uninstall"}, "ssh /bin/sh -s"},
	{[]string{"mixer", "uninstall", "--purge"}, "ssh /bin/sh -s"},
	{[]string{"mixer", "stop"}, "ssh /bin/sh -s"},
	{[]string{"mixer", "enable"}, "ssh /bin/sh -s"},
	{[]string{"mixer", "disable"}, "ssh /bin/sh -s"},
	{[]string{"mixer", "xruns", "0"}, "ssh /bin/sh -s"},
	{[]string{"bench"}, "ssh set -e; rm -rf /run/rc2fx-bench;"},
}

// fake holds a temporary directory acting as both the mock mixer and
// target/mixer (or target/bench), starting with flag=0.
type fake struct {
	t   *testing.T
	dir string
}

// newFake creates a fake mixer directory and writes an optional failure marker file.
func newFake(t *testing.T, marker, content string) *fake {
	t.Helper()
	dir := t.TempDir()
	write := func(name string, data []byte) {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o666); err != nil {
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
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), name)
		write(name, data)
	}
	write("SHA256SUMS", []byte(sums.String()))
	write("rc2fx-bench", []byte("placeholder"))
	// An empty 48 kHz recording.
	write("speech.wav", []byte("RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\xbb\x00\x00\x00\x77\x01\x00\x02\x00\x10\x00data\x00\x00\x00\x00"))
	write("ssh", []byte(fakeSSH))
	write("flag", []byte("0\n"))
	write(marker, []byte(content))
	return &fake{t, dir}
}

// Run the fake SSH script via bash to avoid ETXTBSY races when parallel tests
// exec freshly written scripts.
func (f *fake) run(command []string) error {
	mixer := &Mixer{host: "root@fake", options: []string{filepath.Join(f.dir, "ssh")}, program: "bash"}
	if len(command) == 1 && command[0] == "bench" {
		return benchIn(f.dir, func() (*Mixer, error) { return mixer, nil }, filepath.Join(f.dir, "speech.wav"), nil)
	}
	return act(mixer, f.dir, command[1], command[2:])
}

func (f *fake) events() []string {
	data, err := os.ReadFile(filepath.Join(f.dir, "events"))
	if err != nil {
		f.t.Fatal(err)
	}
	return lines(string(data))
}

func TestEveryArmingCommandArmsAndVerifiesBeforeItSendsAnythingElse(t *testing.T) {
	for _, c := range armingCommands {
		f := newFake(t, "no-marker", "")
		// xruns and bench fail after arming because the fake doesn't return data.
		_ = f.run(c.command)
		events := f.events()
		if !(len(events) > 5 && slices.Equal(events[:5], armedEvents) && strings.HasPrefix(events[5], c.first)) {
			t.Errorf("%v: %q", c.command, events)
		}
	}
}

func TestWhenArmingFailsTheCommandIsRefusedAndNothingElseIsSent(t *testing.T) {
	// What fails, the reason given, and how much of the arming happened.
	for _, failure := range []struct {
		marker, content, reason string
		sent                    int
	}{
		{"setenv-fails", "", "(exit status: 1)", 3},
		{"readback", "0\n", "reads back as \"0\", not 1", 5},
		{"unreachable", "", "(exit status: 255) ssh: connection", 1},
		{"first-boot", "", "(exit status: 4) Refusing recovery arming", 1},
	} {
		for _, c := range armingCommands {
			f := newFake(t, failure.marker, failure.content)
			err := f.run(c.command)
			if err == nil {
				t.Fatalf("%v, %s: not refused", c.command, failure.marker)
			}
			name := strings.Join(c.command[:min(len(c.command), 2)], " ")
			refusal := fmt.Sprintf("`%s` was not started: power recovery could not be armed (", name)
			text := err.Error()
			if !strings.HasPrefix(text, refusal) || !strings.Contains(text, failure.reason) ||
				!strings.HasSuffix(text, "would stay off until its button is pressed.") {
				t.Errorf("%s: %s", failure.marker, text)
			}
			if events := f.events(); !slices.Equal(events, armedEvents[:failure.sent]) {
				t.Errorf("%s, %s: %q", name, failure.marker, events)
			}
		}
	}
}

func TestStatusAndLogsOnlyReadAndDoNotArm(t *testing.T) {
	for _, action := range []string{"status", "logs"} {
		f := newFake(t, "no-marker", "")
		if err := f.run([]string{"mixer", action}); err != nil {
			t.Fatal(err)
		}
		if events := f.events(); !slices.Equal(events, []string{"ssh /bin/sh -s"}) {
			t.Errorf("%s: %q", action, events)
		}
	}
}
