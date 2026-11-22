// Package mixer deploys or removes the mixer build on a RODECaster Pro II over
// SSH, runs the JACK recorder (`mixer xruns`) and suppression benchmark (`bench`),
// and sends controller API requests over USB 1. It uses the system `ssh` binary
// and passes remote scripts over stdin to avoid host shell quoting differences.
package mixer

import (
	"archive/tar"
	"bufio"
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
)

// Two of the scripts that run on the mixer (embed.Scripts), in the
// repository: the supervisor and the installer.
const (
	rc2fxShPath   = task.MixerScriptsDir + "/rc2fx.sh"
	installShPath = task.MixerScriptsDir + "/install.sh"
	maxReplyBytes = 64 << 20
	maxReplyNodes = 4_000_000
	apiLineLimit  = 1024 * 1024
)

func script(path string) (string, error) {
	data, err := os.ReadFile(filepath.Join(task.Root(), filepath.FromSlash(path)))
	if err != nil {
		return "", task.Errorf("%s: %s", path, task.IOError(err))
	}
	return string(data), nil
}

// onMixer picks whichever copy of rc2fx.sh is on the mixer, as the launcher
// shim does (a build in RAM wins); every copy shares the runtime state in
// /run/rc2fx.
func onMixer(action string) string {
	return "for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do\n  " +
		"if [ -f $s ]; then exec /bin/bash $s " + action + "; fi\ndone\n" +
		"echo 'RC2FX is not on this mixer' >&2\nexit 1\n"
}

// runScript: a build run from RAM needs the launcher shim too. Without a
// bundle the shim only execs the stock mixer; `mixer-uninstall` removes it.
const runScript = `set -e
/run/rc2fx-upload/rc2fx-firmware check-device /run/rc2fx-upload/firmware-profiles.json /usr/bin/rc_audio_mixer /usr/bin/audio-start.sh > /dev/null
if ! test -x /usr/sbin/rc_audio_mixer; then
  cp /run/rc2fx-upload/rc2fx-shim.sh /usr/sbin/rc_audio_mixer
  chmod 755 /usr/sbin/rc_audio_mixer
  echo 'Added the launcher shim /usr/sbin/rc_audio_mixer; it does nothing without a bundle, and mixer-uninstall removes it'
fi
if ! test -x /usr/sbin/rc_gui_juce; then
  cp /run/rc2fx-upload/rc2fx-gui-shim.sh /usr/sbin/rc_gui_juce
  chmod 755 /usr/sbin/rc_gui_juce
  echo 'Added the display shim /usr/sbin/rc_gui_juce; it runs the stock GUI without a bundle, and mixer-uninstall removes it; it takes effect at the next boot'
fi
rm -rf /run/rc2fx-dev
mv /run/rc2fx-upload /run/rc2fx-dev
status=0
/bin/bash /run/rc2fx-dev/rc2fx.sh restart --wait 240 || status=$?
if test "$status" -eq 3; then echo 'After the power cycle, run this again: a build in RAM does not survive it'; fi
exit "$status"
`

// commitScript: after an install, commit once RC2FX is processing audio
// (its controller runs and the mixer's callbacks advance in the native
// report), otherwise roll back to the previous runtime.
const commitScript = `set -e
test -f /opt/.rc2fx-install-pending || { echo 'Install already committed'; exit 0; }
callbacks() { grep -o '"callbacks":[0-9]*' /run/rc2fx/audit.json 2>/dev/null | head -n 1 | tr -dc 0-9; }
for n in $(seq 1 60); do
 first=$(callbacks); sleep 2; second=$(callbacks)
 if test -n "$first" && test -n "$second" && test "$second" -gt "$first" && pgrep -x rc2fxd > /dev/null; then
  /bin/sh /opt/rc2fx/install.sh commit
  echo 'RC2FX is processing audio; install committed'
  exit 0
 fi
done
echo 'RC2FX did not start processing audio; rolling back' >&2
/bin/sh /opt/rc2fx/install.sh rollback
exit 1
`

const uninstallScript = `set -e
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
  if [ -f $s ]; then /bin/bash $s stop; break; fi
done
rm -f /usr/sbin/rc_audio_mixer /usr/sbin/rc_gui_juce /etc/rc5.d/S99rc2fx /etc/init.d/rc2fx
rm -rf /run/rc2fx-dev /run/rc2fx /run/rc2fx-boot
if [ "$PURGE" = 1 ]; then
  rm -rf /opt/rc2fx
  echo "RC2FX and its settings removed"
else
  for f in /opt/rc2fx/* /opt/rc2fx/.[!.]*; do
    [ -e "$f" ] || continue
    case "${f##*/}" in data) ;; *) rm -rf "$f" ;; esac
  done
  echo "RC2FX removed; settings kept in /opt/rc2fx/data (uninstall --purge deletes them)"
fi
sync
`

// Every flag change ends in `sync`: a sudden power loss can otherwise drop a
// recent ext4 write that was never flushed (measured on the device 2026-11-01).
const enableScript = `test -x /usr/sbin/rc_audio_mixer || { echo "RC2FX is not installed"; exit 1; }
rm -f /opt/rc2fx/disabled
sync
echo "RC2FX will start at boot"
`

const disableScript = `test -x /usr/sbin/rc_audio_mixer || { echo "RC2FX is not installed"; exit 1; }
touch /opt/rc2fx/disabled
sync
echo "RC2FX will not start at boot (stop it now with: just mixer-stop)"
`

// Run is `mixer <action> ...`.
func Run(action string, rest []string) error {
	if action == "api-session" && len(rest) == 0 {
		return apiSession()
	}
	mixer, err := Load()
	if err != nil {
		return err
	}
	return act(mixer, filepath.Join(task.Root(), "target", "mixer"), action, rest)
}

// act runs one action, with build as target/mixer. Mutating actions first
// verify the mixer is not in a first-boot or production-boot sequence and,
// when pre_command is configured, run it before mutating the mixer. `status`
// and `logs` only read. `api` goes to the controller over USB 1 and never uses
// SSH.
func act(mixer *Mixer, build, action string, rest []string) error {
	prepare := func() error { return mixer.Prepare("mixer " + action) }
	switch {
	case action == "run" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		if err := mixer.upload(build); err != nil {
			return err
		}
		return mixer.script(runScript)
	case action == "install" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		if err := mixer.upload(build); err != nil {
			return err
		}
		install, err := script(installShPath)
		if err != nil {
			return err
		}
		if err := mixer.script(install); err != nil {
			return err
		}
		return mixer.script(commitScript)
	case action == "uninstall" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		return mixer.script(uninstallScript)
	case action == "uninstall" && len(rest) == 1 && rest[0] == "--purge":
		if err := prepare(); err != nil {
			return err
		}
		return mixer.script("PURGE=1\n" + uninstallScript)
	case action == "stop" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		return mixer.script(onMixer(action))
	case (action == "status" || action == "logs") && len(rest) == 0:
		return mixer.send("/bin/sh -s", []byte(onMixer(action)))
	case action == "api" && len(rest) == 2:
		return api(rest[0], rest[1], nil)
	case action == "api" && len(rest) == 3:
		return api(rest[0], rest[1], &rest[2])
	case action == "enable" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		return mixer.script(enableScript)
	case action == "xruns":
		return xruns(mixer, rest)
	case action == "disable" && len(rest) == 0:
		if err := prepare(); err != nil {
			return err
		}
		return mixer.script(disableScript)
	}
	return task.Errorf("unknown mixer action `%s`; see `./xtask help`", action)
}

// Mixer is how to reach the mixer over SSH.
type Mixer struct {
	ip           string
	user         string
	host         string
	key          string
	knownHosts   string
	hostKeyAlias string
	preCommand   string
	options      []string
	// program is `ssh`; only this package's tests put a stand-in here.
	program string
}

// Load reads the mixer connection. Settings from the environment win over
// .rc2fx-mixer.conf.
func Load() (*Mixer, error) {
	data, _ := os.ReadFile(filepath.Join(task.Root(), ".rc2fx-mixer.conf"))
	file := ""
	if utf8.Valid(data) {
		file = string(data)
	}
	setting := func(key string) (string, bool) {
		value, ok := os.LookupEnv("RC2FX_MIXER_" + strings.ToUpper(key))
		if ok && !utf8.ValidString(value) {
			ok = false
		}
		if !ok {
			for _, line := range common.Lines(file) {
				name, v, found := strings.Cut(line, "=")
				if found && strings.TrimSpace(name) == key {
					value, ok = strings.Trim(strings.TrimSpace(v), `"`), true
					break
				}
			}
		}
		return value, ok && value != ""
	}
	ip, ok := setting("ip")
	if !ok {
		return nil, task.Error("no mixer address: copy .rc2fx-mixer.conf.example to .rc2fx-mixer.conf and set ip")
	}
	user, ok := setting("user")
	if !ok {
		user = "root"
	}
	options := []string{"-o", "BatchMode=yes", "-o", "ConnectTimeout=10"}
	key, _ := setting("key")
	if key != "" {
		options = append(options, "-i", key, "-o", "IdentitiesOnly=yes")
	}
	known, _ := setting("known_hosts")
	if known != "" {
		options = append(options, "-o", "UserKnownHostsFile="+known)
	}
	// The app pins the mixer's host key under `rc2fx-<id>`, not its
	// address, so the key stays trusted when the address changes.
	alias, _ := setting("host_key_alias")
	if alias != "" {
		for i := 0; i < len(alias); i++ {
			c := alias[i]
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-') {
				return nil, task.Errorf("host_key_alias %q is not an RC2FX alias", alias)
			}
		}
		options = append(options, "-o", "HostKeyAlias="+alias)
	}
	preCommand, _ := setting("pre_command")
	if strings.HasPrefix(preCommand, "~/") {
		if home, err := os.UserHomeDir(); err == nil && home != "" {
			preCommand = filepath.Join(home, preCommand[2:])
		}
	}
	return &Mixer{
		ip:           ip,
		user:         user,
		host:         user + "@" + ip,
		key:          key,
		knownHosts:   known,
		hostKeyAlias: alias,
		preCommand:   preCommand,
		options:      options,
		program:      "ssh",
	}, nil
}

func (m *Mixer) command(remote string) *exec.Cmd {
	args := append(append([]string{}, m.options...), m.host, remote)
	return exec.Command(m.program, args...)
}

// firstBootCheck refuses mutating commands while the mixer is still in its
// first boot or production boot after a firmware update (it flashes
// controllers and restarts during that boot). Exit 4 distinguishes that
// refusal from an SSH connection error.
const firstBootCheck = "test ! -e /usr/bin/first-boot && test ! -e /usr/bin/production-boot || exit 4"

// Prepare checks that the mixer is not in a first-boot or production-boot
// sequence, then runs the optional local pre_command hook before a mutating
// mixer action.
func (m *Mixer) Prepare(action string) error {
	check := m.command(firstBootCheck)
	check.Stdout = os.Stdout
	check.Stderr = os.Stderr
	if err := check.Run(); err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			if check.ProcessState.ExitCode() == 4 {
				return task.Errorf("`%s` refused: mixer is in its first boot after an update; wait for it to finish and restart", action)
			}
			return task.Errorf("`%s` was not started: could not check mixer boot state (%s)", action, task.Status(check.ProcessState))
		}
		return task.Errorf("`%s` was not started: could not start ssh: %s", action, task.IOError(err))
	}
	if m.preCommand == "" {
		return nil
	}
	cmd := exec.Command(m.preCommand, action)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Env = append(os.Environ(),
		"RC2FX_MIXER_ACTION="+action,
		"RC2FX_MIXER_IP="+m.ip,
		"RC2FX_MIXER_USER="+m.user,
		"RC2FX_MIXER_HOST="+m.host,
		"RC2FX_MIXER_KEY="+m.key,
		"RC2FX_MIXER_KNOWN_HOSTS="+m.knownHosts,
		"RC2FX_MIXER_HOST_KEY_ALIAS="+m.hostKeyAlias,
	)
	if err := cmd.Run(); err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) {
			return task.Errorf("`%s` was not started: pre_command failed (%s)", action, task.Status(cmd.ProcessState))
		}
		return task.Errorf("`%s` was not started: could not run pre_command %q: %s", action, m.preCommand, task.IOError(err))
	}
	return nil
}

// runWithInput starts command, writes input to its stdin and waits. It
// returns the error writing input (a script cut short) separately.
func runWithInput(command *exec.Cmd, input []byte) (started bool, writeErr, waitErr error) {
	stdin, err := command.StdinPipe()
	if err != nil {
		return false, nil, err
	}
	if err := command.Start(); err != nil {
		return false, nil, err
	}
	written := make(chan error, 1)
	go func() {
		_, err := stdin.Write(input)
		if closeErr := stdin.Close(); err == nil && !errors.Is(closeErr, os.ErrClosed) {
			err = closeErr
		}
		written <- err
	}()
	waitErr = command.Wait()
	writeErr = <-written
	var exit *exec.ExitError
	if errors.As(waitErr, &exit) {
		waitErr = nil
	}
	return true, writeErr, waitErr
}

// send runs remote on the mixer with input on its stdin.
func (m *Mixer) send(remote string, input []byte) error {
	command := m.command(remote)
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr
	started, writeErr, waitErr := runWithInput(command, input)
	if !started {
		return task.Errorf("could not start ssh: %s", task.IOError(waitErr))
	}
	if waitErr != nil {
		return task.Error(task.IOError(waitErr))
	}
	if !command.ProcessState.Success() {
		return task.Errorf("command on the mixer failed (%s)", task.Status(command.ProcessState))
	}
	if writeErr != nil {
		return task.Errorf("sending to the mixer: %s", task.IOError(writeErr))
	}
	return nil
}

// capture runs remote, which only reads, and returns what it writes to
// stdout.
func (m *Mixer) capture(remote string) ([]byte, error) {
	command := m.command(remote)
	var stdout bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = os.Stderr
	if err := command.Start(); err != nil {
		return nil, task.Errorf("could not start ssh: %s", task.IOError(err))
	}
	if err := command.Wait(); err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			return nil, task.Error(task.IOError(err))
		}
	}
	if !command.ProcessState.Success() {
		return nil, task.Errorf("command on the mixer failed (%s)", task.Status(command.ProcessState))
	}
	return stdout.Bytes(), nil
}

// ssh runs remote on the mixer with input on its stdin.
func (m *Mixer) ssh(remote string, input []byte) error { return m.send(remote, input) }

// captureScript runs script with the mixer's shell and returns what it
// writes to stdout; its stderr is passed through.
func (m *Mixer) captureScript(script string) ([]byte, error) {
	command := m.command("/bin/sh -s")
	var stdout bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = os.Stderr
	started, writeErr, waitErr := runWithInput(command, []byte(script))
	if !started {
		return nil, task.Errorf("could not start ssh: %s", task.IOError(waitErr))
	}
	if waitErr != nil {
		return nil, task.Error(task.IOError(waitErr))
	}
	if !command.ProcessState.Success() {
		return nil, task.Errorf("command on the mixer failed (%s)", task.Status(command.ProcessState))
	}
	if writeErr != nil {
		return nil, task.Errorf("sending to the mixer: %s", task.IOError(writeErr))
	}
	return stdout.Bytes(), nil
}

func (m *Mixer) script(script string) error { return m.ssh("/bin/sh -s", []byte(script)) }

// ustar appends one regular file to a tar archive with a ustar header:
// mode, size, time 0, no owner.
func ustar(archive *tar.Writer, name string, data []byte, mode int64) error {
	header := &tar.Header{
		Typeflag: tar.TypeReg,
		Name:     name,
		Mode:     mode,
		Size:     int64(len(data)),
		ModTime:  time.Unix(0, 0),
		Format:   tar.FormatUSTAR,
	}
	if err := archive.WriteHeader(header); err != nil {
		return err
	}
	_, err := archive.Write(data)
	return err
}

// uploadMode is a payload file's mode in the upload: the launcher shims are
// copied to /usr/sbin and run by RØDE's own scripts, so anyone may run them;
// the other executables and scripts are root's alone; the rest is data.
func uploadMode(name string) int64 {
	switch {
	case name == "rc2fx-shim.sh" || name == "rc2fx-gui-shim.sh":
		return 0o755
	case embed.IsScript(name) || embed.IsBinary(name) && !strings.HasSuffix(name, ".so"):
		return 0o700
	}
	return 0o600
}

// upload stages the build in RAM at /run/rc2fx-upload, verified on arrival.
// The files are embed.PayloadNames: the pinned build from target/mixer, its
// SHA256SUMS and the scripts from the repository.
func (m *Mixer) upload(build string) error {
	read := func(name string) ([]byte, error) {
		data, err := os.ReadFile(filepath.Join(build, name))
		if err != nil {
			return nil, task.Errorf("target/mixer/%s is missing; run `just mixer-build`", name)
		}
		return data, nil
	}
	files := map[string][]byte{}
	for _, name := range slices.Concat(embed.Pinned[:], []string{"SHA256SUMS"}) {
		data, err := read(name)
		if err != nil {
			return err
		}
		files[name] = data
	}
	profiles := files["firmware-profiles.json"]
	if !utf8.Valid(profiles) {
		return task.Error("invalid utf-8 sequence in target/mixer/firmware-profiles.json")
	}
	if _, err := firmware.ParseCatalog(string(profiles)); err != nil {
		return task.Error(err.Error())
	}
	release := files[embed.ReleaseManifest]
	if _, err := core.ParseMixerRelease(release); err != nil {
		return task.Error(err.Error())
	}
	source, err := os.ReadFile(filepath.Join(task.Root(), "config", "mixer-release.json"))
	if err != nil {
		return task.Errorf("config/mixer-release.json: %s", task.IOError(err))
	}
	if !bytes.Equal(release, []byte(task.CRLFToLF(string(source)))) {
		return task.Error("target/mixer/mixer-release.json differs from config/mixer-release.json; run `just mixer-build`")
	}
	var expected strings.Builder
	for _, name := range embed.Pinned {
		fmt.Fprintf(&expected, "%s  %s\n", common.SHA256Hex(files[name]), name)
	}
	if string(files["SHA256SUMS"]) != expected.String() {
		return task.Error("target/mixer does not match its SHA256SUMS; rebuild it")
	}
	for _, name := range embed.Scripts {
		text, err := script(task.MixerScriptsDir + "/" + name)
		if err != nil {
			return err
		}
		files[name] = []byte(task.CRLFToLF(text))
	}
	var archive bytes.Buffer
	writer := tar.NewWriter(&archive)
	for _, name := range embed.PayloadNames {
		data, ok := files[name]
		if !ok {
			return task.Errorf("the upload has no source for %s", name)
		}
		if err := ustar(writer, name, data, uploadMode(name)); err != nil {
			return task.Error(err.Error())
		}
	}
	if err := writer.Close(); err != nil {
		return task.Error(err.Error())
	}
	fmt.Printf("Uploading to %s\n", m.host)
	return m.ssh("set -e; rm -rf /run/rc2fx-upload; mkdir -m 700 /run/rc2fx-upload; "+
		"tar -xf - -C /run/rc2fx-upload; cd /run/rc2fx-upload; sha256sum -c SHA256SUMS > /dev/null", archive.Bytes())
}

// api sends one controller API request over USB 1 (docs/reference/api.md),
// the body JSON text or `@file`, and prints the status and the reply body.
func api(method, path string, body *string) error {
	var raw []byte
	if body != nil {
		text := *body
		if file, ok := strings.CutPrefix(text, "@"); ok {
			data, err := os.ReadFile(file)
			if err == nil && !utf8.Valid(data) {
				return task.Errorf("%s: stream did not contain valid UTF-8", file)
			}
			if err != nil {
				return task.Errorf("%s: %s", file, task.IOError(err))
			}
			text = string(data)
		}
		value, err := core.ParseJSONLimited(text, maxReplyBytes, maxReplyNodes)
		if err != nil {
			return task.Errorf("body: %s", err)
		}
		raw = []byte(value.String())
	}
	request := usbcontrol.Request{Method: strings.ToUpper(method), Path: strings.TrimLeft(path, "/"), Body: raw}
	client, err := usbcontrol.Open()
	if err != nil {
		return task.Error(err.Error())
	}
	defer client.Close()
	reply, err := client.Request(&request)
	if err != nil {
		return task.Error(err.Error())
	}
	fmt.Println(reply.Status)
	pretty, err := prettyBody(reply.Body)
	if err != nil {
		return task.Error(err.Error())
	}
	fmt.Println(pretty)
	if reply.Status >= 200 && reply.Status < 300 {
		return nil
	}
	return task.Errorf("the controller answered %d", reply.Status)
}

// prettyBody is a reply body indented two spaces; "null" when empty.
func prettyBody(raw []byte) (string, error) {
	if len(raw) == 0 {
		return "null", nil
	}
	value, err := core.ParseJSONBytes(raw, maxReplyBytes, maxReplyNodes)
	if err != nil {
		return "", err
	}
	return value.Pretty(), nil
}

// flushWriter is where api-session writes replies: each is flushed at once.
type flushWriter interface {
	io.Writer
	Flush() error
}

// apiSession is JSON-lines requests/replies over ONE USB client/nonce.
// Separate one-shot invocations otherwise contend with their own previous
// five-second lease.
func apiSession() error {
	client, err := usbcontrol.Open()
	if err != nil {
		return task.Error(err.Error())
	}
	defer client.Close()
	output := bufio.NewWriter(os.Stdout)
	return apiStream(bufio.NewReader(os.Stdin), output, func(request usbcontrol.Request) (*usbcontrol.Reply, error) {
		reply, err := client.Request(&request)
		if err != nil {
			return nil, task.Error(err.Error())
		}
		return reply, nil
	})
}

func apiStream(input *bufio.Reader, output flushWriter, request func(usbcontrol.Request) (*usbcontrol.Reply, error)) error {
	line := make([]byte, 0, 4096)
	for {
		line = line[:0]
		for {
			b, err := input.ReadByte()
			if errors.Is(err, io.EOF) {
				break
			}
			if err != nil {
				return task.Error(task.IOError(err))
			}
			line = append(line, b)
			if b == '\n' || len(line) > apiLineLimit {
				break
			}
		}
		if len(line) == 0 {
			return nil
		}
		if len(line) > apiLineLimit {
			return task.Error("USB request line exceeds 1 MiB")
		}
		if blank(line) {
			continue
		}
		var parsed usbcontrol.Request
		if err := parsed.UnmarshalJSON(line); err != nil {
			return task.Errorf("Invalid request JSON: %s", err)
		}
		reply, err := request(parsed)
		if err != nil {
			return err
		}
		encoded, err := reply.MarshalJSON()
		if err != nil {
			return task.Error(err.Error())
		}
		if _, err := output.Write(append(encoded, '\n')); err != nil {
			return task.Error(task.IOError(err))
		}
		if err := output.Flush(); err != nil {
			return task.Error(task.IOError(err))
		}
	}
}

// blank is every byte ASCII whitespace.
func blank(line []byte) bool {
	for _, b := range line {
		switch b {
		case ' ', '\t', '\n', '\f', '\r':
		default:
			return false
		}
	}
	return true
}
