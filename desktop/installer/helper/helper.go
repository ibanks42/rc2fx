// Package helper is a long-lived installer process for systems where raw
// USB, mounting and ejection need administrator rights the desktop app does
// not have.
//
// The desktop app starts `<app> --installer-helper <root> <work>` through
// `pkexec` once. Requests and replies are JSON lines on its stdin/stdout. A
// cancel line may arrive while a request runs; it stops a download the same
// way Engine.Cancel does in-process.
package helper

import (
	"bufio"
	_ "embed"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sync"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

// Flag starts the helper.
const Flag = "--installer-helper"

// USBPermission is the start of the error the app gets when Linux refuses
// it the mixer's USB 1 control interface (no udev rule). The app offers to
// install USBRule when it sees exactly this.
const USBPermission = platform.USBPermission

// USBRule is the one udev rule RC2FX needs on Linux, as shipped in
// config/udev (a test keeps this copy identical). The app shows these exact
// bytes before installing them, and installs nothing else.
//
//go:embed 70-rc2fx.rules
var USBRule string

// USBRulePath is where USBRule is installed.
const USBRulePath = "/etc/udev/rules.d/70-rc2fx.rules"

// USBRuleFlag: `<app> --install-usb-rule`, started through `pkexec`, writes
// USBRule to USBRulePath, reloads udev and exits. It takes no other input.
const USBRuleFlag = "--install-usb-rule"

// Embedded is the root argument that means "the payload embedded in this
// executable", which is what the desktop app passes: it has no folder to
// name.
const Embedded = "-"

// Download is the root argument of a download-only helper.
const Download = "--download-payload"

// privilegedLaunch plans `pkexec <exe>`. Keep the staged AppDir alive until
// the helper has exited, including on the long-lived request path. Both
// privilege entry points use this same planner.
func privilegedLaunch() (*exec.Cmd, *stagedAppDir, error) {
	exe, err := os.Executable()
	if err != nil {
		return nil, nil, err
	}
	executable, guard, err := prepareLaunch(exe)
	if err != nil {
		return nil, nil, err
	}
	return exec.Command("pkexec", executable), guard, nil
}

// message is serde's `{"progress": …}` / `{"done": {"Ok"|"Err": …}}`.
type message struct {
	Progress *installer.Progress
	Done     *done
}

type done struct {
	Reply *installer.Reply
	Error *string
}

func (m message) MarshalJSON() ([]byte, error) {
	if m.Progress != nil {
		return tagged("progress", m.Progress)
	}
	if m.Done.Reply != nil {
		inner, err := tagged("Ok", m.Done.Reply)
		if err != nil {
			return nil, err
		}
		return tagged("done", rawJSON(inner))
	}
	inner, err := tagged("Err", m.Done.Error)
	if err != nil {
		return nil, err
	}
	return tagged("done", rawJSON(inner))
}

func (m *message) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	switch {
	case variant == "progress" && payload != nil:
		var p installer.Progress
		if err := sjson.Unmarshal(payload, &p); err != nil {
			return err
		}
		*m = message{Progress: &p}
	case variant == "done" && payload != nil:
		result, inner, err := sjson.Tagged(payload)
		if err != nil {
			return err
		}
		switch {
		case result == "Ok" && inner != nil:
			var r installer.Reply
			if err := sjson.Unmarshal(inner, &r); err != nil {
				return err
			}
			*m = message{Done: &done{Reply: &r}}
		case result == "Err" && inner != nil:
			var text string
			if err := sjson.Unmarshal(inner, &text); err != nil {
				return err
			}
			*m = message{Done: &done{Error: &text}}
		default:
			return fmt.Errorf("unknown variant `%s`, expected `Ok` or `Err`", result)
		}
	default:
		return fmt.Errorf("unknown variant `%s`, expected `progress` or `done`", variant)
	}
	return nil
}

// inbound is what the desktop app writes to the helper:
// `{"request": …}` or `"cancel"`.
type inbound struct {
	Request *installer.Request
	Cancel  bool
}

func (i inbound) MarshalJSON() ([]byte, error) {
	if i.Cancel {
		return []byte(`"cancel"`), nil
	}
	return tagged("request", i.Request)
}

func (i *inbound) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	switch {
	case variant == "cancel" && payload == nil:
		*i = inbound{Cancel: true}
	case variant == "request" && payload != nil:
		var r installer.Request
		if err := sjson.Unmarshal(payload, &r); err != nil {
			return err
		}
		*i = inbound{Request: &r}
	default:
		return fmt.Errorf("unknown variant `%s`, expected `request` or `cancel`", variant)
	}
	return nil
}

type rawJSON []byte

func (r rawJSON) MarshalJSON() ([]byte, error) { return r, nil }

func tagged(variant string, payload any) ([]byte, error) {
	body, err := sjson.Marshal(payload)
	if err != nil {
		return nil, err
	}
	out := append([]byte{'{'}, sjson.String(variant)...)
	out = append(out, ':')
	out = append(out, body...)
	return append(out, '}'), nil
}

func writeLine(w io.Writer, value any) error {
	data, err := sjson.Marshal(value)
	if err != nil {
		return err
	}
	_, err = w.Write(append(data, '\n'))
	return err
}

// Serve serves requests on stdin until it closes.
func Serve(root, work string) error {
	var stdout sync.Mutex
	out := bufio.NewWriter(os.Stdout)
	send := func(m message) error {
		stdout.Lock()
		defer stdout.Unlock()
		if err := writeLine(out, m); err != nil {
			return err
		}
		return out.Flush()
	}
	engine := engineFor(root, work)
	engine.Progress = func(p installer.Progress) { send(message{Progress: &p}) }
	// Read stdin on its own goroutine so a cancel reaches a running request.
	type parsed struct {
		request installer.Request
		err     error
	}
	requests := make(chan parsed)
	go func() {
		defer close(requests)
		lines := bufio.NewReader(os.Stdin)
		for {
			line, err := lines.ReadString('\n')
			if len(line) == 0 && err != nil {
				if err != io.EOF {
					requests <- parsed{err: err}
				}
				return
			}
			var in inbound
			if err := sjson.Unmarshal([]byte(trimNewline(line)), &in); err != nil {
				requests <- parsed{err: err}
				continue
			}
			if in.Cancel {
				engine.Cancel.Store(true)
				continue
			}
			requests <- parsed{request: *in.Request}
		}
	}()
	for p := range requests {
		if p.err != nil {
			return p.err
		}
		engine.Cancel.Store(false)
		reply, err := engine.Handle(p.request)
		m := message{Done: &done{Reply: &reply}}
		if err != nil {
			text := err.Error()
			m = message{Done: &done{Error: &text}}
		}
		if err := send(m); err != nil {
			return err
		}
	}
	return nil
}

func trimNewline(line string) string {
	if n := len(line); n > 0 && line[n-1] == '\n' {
		line = line[:n-1]
		if n := len(line); n > 0 && line[n-1] == '\r' {
			line = line[:n-1]
		}
	}
	return line
}

// Helper is the desktop side of a running helper.
type Helper struct {
	cmd    *exec.Cmd
	stdin  *stdinBox
	stdout *bufio.Reader
	exited chan struct{}
	appdir *stagedAppDir
}

type stdinBox struct {
	mu sync.Mutex
	w  io.WriteCloser
}

// Canceller asks a running helper to stop its current download, from any
// goroutine.
type Canceller struct{ stdin *stdinBox }

// Cancel sends the cancel line.
func (c Canceller) Cancel() {
	c.stdin.mu.Lock()
	defer c.stdin.mu.Unlock()
	if c.stdin.w != nil {
		writeLine(c.stdin.w, inbound{Cancel: true})
	}
}

// StartDownloadOnly starts a download-only helper.
func StartDownloadOnly(work string) (*Helper, error) {
	return spawn(Download, work)
}

// Start starts the helper for the installer folder root through `pkexec`.
func Start(root, work string) (*Helper, error) {
	return spawn(root, work)
}

func spawn(root, work string) (*Helper, error) {
	cmd, appdir, err := privilegedLaunch()
	if err != nil {
		return nil, err
	}
	cmd.Args = append(cmd.Args, Flag, root, work)
	cmd.Stderr = os.Stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		appdir.Remove()
		return nil, err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		appdir.Remove()
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		appdir.Remove()
		return nil, installer.Error(fmt.Sprintf("could not ask for administrator permission (pkexec): %v", err))
	}
	h := &Helper{cmd: cmd, stdin: &stdinBox{w: stdin}, stdout: bufio.NewReader(stdout), exited: make(chan struct{}), appdir: appdir}
	go func() {
		cmd.Wait()
		close(h.exited)
	}()
	return h, nil
}

// Alive is whether the helper process is still running. A request can fail
// because the mixer did; only a helper that has exited needs starting again.
func (h *Helper) Alive() bool {
	select {
	case <-h.exited:
		return false
	default:
		return true
	}
}

// Canceller is a canceller for this helper.
func (h *Helper) Canceller() Canceller { return Canceller{h.stdin} }

// Handle sends one request and waits for its reply.
func (h *Helper) Handle(request installer.Request, progress func(installer.Progress)) (installer.Reply, error) {
	h.stdin.mu.Lock()
	if h.stdin.w == nil {
		h.stdin.mu.Unlock()
		return installer.Reply{}, installer.Error("the installer helper stopped")
	}
	err := writeLine(h.stdin.w, inbound{Request: &request})
	h.stdin.mu.Unlock()
	if err != nil {
		return installer.Reply{}, err
	}
	for {
		line, err := h.stdout.ReadString('\n')
		if len(line) == 0 && err != nil {
			if !errors.Is(err, io.EOF) {
				return installer.Reply{}, err
			}
			select {
			case <-h.exited:
				if code := h.cmd.ProcessState.ExitCode(); code == 126 || code == 127 {
					return installer.Reply{}, installer.Error("administrator permission was not granted")
				}
			default:
			}
			return installer.Reply{}, installer.Error("the installer helper stopped; reopen the installer")
		}
		var m message
		if err := sjson.Unmarshal([]byte(trimNewline(line)), &m); err != nil {
			return installer.Reply{}, err
		}
		if m.Progress != nil {
			progress(*m.Progress)
			continue
		}
		if m.Done.Error != nil {
			return installer.Reply{}, installer.Error(*m.Done.Error)
		}
		return *m.Done.Reply, nil
	}
}

// Close ends the helper after any request it is serving and waits for it.
func (h *Helper) Close() error {
	// Closing stdin ends the helper after any request it is serving.
	h.stdin.mu.Lock()
	if h.stdin.w != nil {
		h.stdin.w.Close()
		h.stdin.w = nil
	}
	h.stdin.mu.Unlock()
	<-h.exited
	h.appdir.Remove()
	return nil
}

// RequestUSBRule is the desktop side: ask the system for permission (its
// own password prompt) to run this app's USBRuleFlag once, and wait for it.
func RequestUSBRule() error {
	cmd, appdir, err := privilegedLaunch()
	if err != nil {
		return err
	}
	defer appdir.Remove()
	cmd.Args = append(cmd.Args, USBRuleFlag)
	cmd.Stdin = nil
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	err = cmd.Run()
	var exit *exec.ExitError
	switch {
	case err == nil:
		return nil
	case errors.As(err, &exit):
		if code := exit.ExitCode(); code == 126 || code == 127 {
			return installer.Error("The password prompt was dismissed or the password wasn't accepted.")
		}
		return installer.Error(fmt.Sprintf("Installing the USB rule failed (%v).", exit.ProcessState))
	}
	return installer.Error(fmt.Sprintf("could not ask for administrator permission (pkexec): %v", err))
}
