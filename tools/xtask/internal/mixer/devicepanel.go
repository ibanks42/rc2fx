package mixer

import (
	"archive/tar"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// The experimental touchscreen UI in experiments/device-panel: an LD_AUDIT
// library for RØDE's GUI, its status tool, and a RAM-only runner (ui.sh).
const (
	devicePanelDir = "experiments/device-panel"
	stockSysroot   = ".deps/stock-sysroot"
	// rcGUIHash is the RØDE GUI the panel library was written against.
	rcGUIHash = "aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca"
)

var devicePanelFiles = []string{"librc2fx_ui.so", "rc2fx-ui-status", "ui.sh"}

const devicePanelUsage = `usage: go -C tools/xtask run . device-panel build
       go -C tools/xtask run . device-panel check
       go -C tools/xtask run . device-panel preview --restart-gui [5-30 seconds | --keep-running]
preview restarts the physical mixer GUI twice. It does NOT restart audio.`

// DevicePanel is `device-panel`.
func DevicePanel(args []string) error {
	switch {
	case len(args) == 1 && args[0] == "build":
		return devicePanelBuild()
	case len(args) == 1 && args[0] == "check":
		return devicePanelCheck()
	case len(args) >= 2 && len(args) <= 3 && args[0] == "preview" && args[1] == "--restart-gui":
		return devicePanelPreview(args[2:])
	}
	return task.Error(devicePanelUsage)
}

func devicePanelOut(root string) string {
	return filepath.Join(root, "target", "device-ui")
}

// devicePanelBuild cross-builds the panel library and status tool into
// target/device-ui and stages ui.sh and SHA256SUMS beside them. It only
// builds: nothing is uploaded or restarted.
func devicePanelBuild() error {
	root := task.Root()
	sysroot := filepath.Join(root, stockSysroot)
	if _, err := os.Stat(filepath.Join(sysroot, "usr", "include", "linux", "videodev2.h")); err != nil {
		return task.Errorf("missing cached stock sysroot headers (%s)", stockSysroot)
	}
	source := filepath.Join(root, filepath.FromSlash(devicePanelDir))
	out := devicePanelOut(root)
	if err := os.MkdirAll(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	target := []string{"cc", "-target", "aarch64-linux-gnu.2.35", "-mcpu=cortex_a53", "-mno-outline-atomics", "-std=c11", "-O2"}
	library := append(append([]string{}, target...),
		"-g", "-fPIC", "-shared", "-fvisibility=hidden", "-fno-omit-frame-pointer",
		"-mbranch-protection=standard", "-Wall", "-Wextra", "-Werror",
		"-isystem", filepath.Join(sysroot, "usr", "include"), "-isystem", filepath.Join(sysroot, "usr", "include", "libdrm"),
		filepath.Join(source, "rc2fx_ui.c"), filepath.Join(source, "control.c"),
		"-o", filepath.Join(out, "librc2fx_ui.so"))
	status := append(append([]string{}, target...),
		"-fPIE", "-pie", "-Wall", "-Wextra", "-Werror", "-isystem", filepath.Join(sysroot, "usr", "include"),
		filepath.Join(source, "status.c"), "-o", filepath.Join(out, "rc2fx-ui-status"))
	for _, args := range [][]string{library, status} {
		if err := task.Run(exec.Command("zig", args...)); err != nil {
			return err
		}
	}
	// Stage the experiment's own RAM-only runner beside its binaries.
	runner, err := os.ReadFile(filepath.Join(source, "ui.sh"))
	if err != nil {
		return task.Errorf("%s: %s", filepath.Join(devicePanelDir, "ui.sh"), task.IOError(err))
	}
	if err := os.WriteFile(filepath.Join(out, "ui.sh"), runner, 0o755); err != nil {
		return task.Error(task.IOError(err))
	}
	var sums strings.Builder
	for _, name := range devicePanelFiles {
		data, err := os.ReadFile(filepath.Join(out, name))
		if err != nil {
			return task.Error(task.IOError(err))
		}
		sum := sha256.Sum256(data)
		fmt.Fprintf(&sums, "%s  %s\n", hex.EncodeToString(sum[:]), name)
	}
	if err := os.WriteFile(filepath.Join(out, "SHA256SUMS"), []byte(sums.String()), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	fmt.Printf("Actual AArch64 GUI module built: %s\n", filepath.Join(out, "librc2fx_ui.so"))
	fmt.Println("Not loaded. No mixer, audio, or desktop process was changed.")
	return nil
}

// devicePanelCheck builds and runs the host input unit tests and compiles
// client_test on the local machine. CC picks the compiler; SANITIZE=1 adds
// ASan and UBSan.
func devicePanelCheck() error {
	root := task.Root()
	cc := os.Getenv("CC")
	if cc == "" {
		cc = "cc"
	}
	flags := []string{"-std=c11", "-O1", "-g", "-fno-omit-frame-pointer", "-Wall", "-Wextra", "-Werror",
		"-idirafter", filepath.Join(root, stockSysroot, "usr", "include"),
		"-idirafter", filepath.Join(root, stockSysroot, "usr", "include", "libdrm"), "-pthread"}
	switch os.Getenv("SANITIZE") {
	case "", "0":
	case "1":
		flags = append(flags, "-fsanitize=address,undefined", "-fno-sanitize-recover=all")
	default:
		return task.Error("SANITIZE must be 0 or 1")
	}
	if err := os.MkdirAll(filepath.Join(root, "target"), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	tmp, err := os.MkdirTemp(filepath.Join(root, "target"), "device-ui-check.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(tmp)
	source := filepath.Join(root, filepath.FromSlash(devicePanelDir))
	inputTest := filepath.Join(tmp, "input_test")
	build := append(append([]string{}, flags...), filepath.Join(source, "input_test.c"), filepath.Join(source, "control.c"), "-o", inputTest, "-ldl")
	if err := task.Run(exec.Command(cc, build...)); err != nil {
		return err
	}
	if err := task.Run(exec.Command(inputTest)); err != nil {
		return err
	}
	build = append(append([]string{}, flags...), filepath.Join(source, "client_test.c"), "-o", filepath.Join(tmp, "client_test"), "-ldl")
	if err := task.Run(exec.Command(cc, build...)); err != nil {
		return err
	}
	fmt.Println("client_test: compiled (run separately against the local Rust test server)")
	return nil
}

var stagePath = regexp.MustCompile(`^/run/rc2fx-ui-upload\.[a-zA-Z0-9]+$`)

// devicePanelPreview runs a bounded preview of the panel on the mixer from
// RAM and restores the stock UI. It restarts the GUI, never audio, and
// installs nothing that survives a reboot. With --keep-running the session
// stays up under its own watchdog until ui.sh ui-stop.
func devicePanelPreview(args []string) error {
	seconds, keep := 15, false
	if len(args) == 1 {
		if args[0] == "--keep-running" {
			seconds, keep = 30, true
		} else {
			n, err := strconv.Atoi(args[0])
			if err != nil || n < 5 || n > 30 {
				return task.Error(devicePanelUsage)
			}
			seconds = n
		}
	}
	mixer, err := Load()
	if err != nil {
		return err
	}
	// The first-boot check and pre_command, as for every mutating action.
	if err := mixer.Prepare("preview"); err != nil {
		return err
	}
	if err := devicePanelBuild(); err != nil {
		return err
	}
	// Check the device before making the RAM staging folder.
	stageOutput, err := mixer.capture(`set -e
test ! -e /usr/bin/first-boot
test ! -e /usr/bin/production-boot
test "$(tr -d "\000" < /proc/device-tree/model)" = "RODECaster Pro II"
printf "%s  %s\n" ` + rcGUIHash + ` /usr/bin/rc_gui_juce | sha256sum -c - >/dev/null
umask 077; mktemp -d /run/rc2fx-ui-upload.XXXXXX`)
	if err != nil {
		return err
	}
	stage := strings.TrimSpace(string(stageOutput))
	if !stagePath.MatchString(stage) {
		return task.Errorf("unexpected RAM staging path %q", stage)
	}
	var archive bytes.Buffer
	writer := tar.NewWriter(&archive)
	out := devicePanelOut(task.Root())
	for _, name := range append(append([]string{}, devicePanelFiles...), "SHA256SUMS") {
		data, err := os.ReadFile(filepath.Join(out, name))
		if err != nil {
			return task.Error(task.IOError(err))
		}
		mode := int64(0o644)
		if name != "SHA256SUMS" {
			mode = 0o755
		}
		if err := writer.WriteHeader(&tar.Header{Name: name, Mode: mode, Size: int64(len(data)), Typeflag: tar.TypeReg}); err != nil {
			return task.Error(err.Error())
		}
		if _, err := writer.Write(data); err != nil {
			return task.Error(err.Error())
		}
	}
	if err := writer.Close(); err != nil {
		return task.Error(err.Error())
	}
	if err := mixer.send(fmt.Sprintf("set -e; tar -xf - -C '%s'; cd '%s'; sha256sum -c SHA256SUMS", stage, stage), archive.Bytes()); err != nil {
		return err
	}
	if err := mixer.send(fmt.Sprintf("'%s/rc2fx-ui-status'", stage), nil); err != nil {
		fmt.Fprintln(os.Stderr, "Touchscreen controller not ready; the panel will show offline and refuse setting changes")
	}
	if !keep {
		return mixer.send(fmt.Sprintf("/bin/bash '%s/ui.sh' ui-preview --restart-gui '%s/librc2fx_ui.so' '%d'", stage, stage, seconds), nil)
	}
	pidOutput, err := mixer.capture(fmt.Sprintf(
		"setsid /bin/bash '%[1]s/ui.sh' ui-run --restart-gui '%[1]s/librc2fx_ui.so' < /dev/null > '%[1]s/session.log' 2>&1 & echo $!", stage))
	if err != nil {
		return err
	}
	pid := strings.TrimSpace(string(pidOutput))
	if _, err := strconv.Atoi(pid); err != nil {
		return task.Errorf("unexpected supervisor PID %q", pid)
	}
	fmt.Printf("RAM UI supervisor PID %s; startup log %s/session.log\n", pid, stage)
	restore := append(append([]string{mixer.program}, mixer.options...), mixer.host, fmt.Sprintf("/bin/bash '%s/ui.sh' ui-stop", stage))
	fmt.Printf("To restore the stock UI: %s\n", shellQuote(restore))
	return nil
}

// shellQuote joins args into a command line a POSIX shell reads back as
// the same words.
func shellQuote(args []string) string {
	quoted := make([]string, len(args))
	for i, arg := range args {
		if arg != "" && strings.Trim(arg, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789@%_+=:,./-") == "" {
			quoted[i] = arg
		} else {
			quoted[i] = "'" + strings.ReplaceAll(arg, "'", `'\''`) + "'"
		}
	}
	return strings.Join(quoted, " ")
}
