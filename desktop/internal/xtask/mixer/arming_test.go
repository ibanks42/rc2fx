//go:build unix

package mixer

// Arm, check, then run, against a stand-in for ssh and the mixer: every
// command that changes or runs something on the mixer arms power recovery
// and verifies it before it sends anything else, and sends nothing else when
// that fails. An error is what main turns into a non-zero exit.

import (
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/setup"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// fakeSSH logs each remote command and each use of the flag to `events`. It
// runs the rc2fx.sh it is sent, for arm-power-recovery only, against the
// file `flag`, and nothing else it is given. Files beside it make it fail.
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

// armed is a successful arming from a flag of 0: the script reads, writes
// and verifies, then the host reads the flag back.
var armedEvents = []string{
	"ssh /bin/bash -s -- arm-power-recovery",
	"fw_printenv -n reboot_pwr_button_bypass",
	"fw_setenv reboot_pwr_button_bypass 1",
	"fw_printenv -n reboot_pwr_button_bypass",
	"ssh fw_printenv -n reboot_pwr_button_bypass",
}

// armingCommands is every arming command, and the first thing it sends once
// armed.
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

// fake is a stand-in mixer with its flag at 0, in a directory that also
// serves as target/mixer and target/bench.
type fake struct {
	t   *testing.T
	dir string
}

// newFake: marker is a file the stand-in looks for, holding content.
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
	for _, name := range []string{
		"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware", "firmware-profiles.json",
		setup.GUIModule, setup.JackerrModule, setup.JackerrSnapshot, "mixer-release.json",
	} {
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

// run runs a command as ./xtask would. The stand-in goes through bash,
// never as a program just written while another test forks.
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
		// xruns and bench fail afterwards: the stand-in returns no results.
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
