//go:build linux

package helper

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// InstallUSBRule is the privileged side of USBRuleFlag: replace the rule
// file atomically (0644, root-owned), then have udev reload its rules and
// apply them to the hidraw devices already plugged in.
func InstallUSBRule() error {
	temporary := filepath.Join(filepath.Dir(USBRulePath), ".70-rc2fx.rules.tmp")
	file, err := os.Create(temporary)
	if err != nil {
		return err
	}
	_, err = file.WriteString(USBRule)
	if err == nil {
		err = file.Chmod(0o644)
	}
	if err == nil {
		err = file.Sync()
	}
	if cerr := file.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		return err
	}
	if err := os.Rename(temporary, USBRulePath); err != nil {
		os.Remove(temporary)
		return err
	}
	tool, err := udevadm()
	if err != nil {
		return err
	}
	for _, args := range [][]string{
		{"control", "--reload-rules"},
		{"trigger", "--action=add", "--subsystem-match=hidraw"},
	} {
		cmd := cleanCommand(tool, args...)
		cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
		if err := cmd.Run(); err != nil {
			return installer.Error(fmt.Sprintf("udevadm %s failed (%v)", strings.Join(args, " "), err))
		}
	}
	return nil
}

// udevadmPaths is where distributions install udevadm. A root process never
// finds a tool through PATH, which pkexec's caller may have influenced.
var udevadmPaths = [3]string{"/usr/bin/udevadm", "/sbin/udevadm", "/bin/udevadm"}

func udevadm() (string, error) {
	for _, path := range udevadmPaths {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
			return path, nil
		}
	}
	return "", installer.Error("udevadm was not found")
}

// cleanCommand is a command that inherits nothing from the environment.
func cleanCommand(program string, args ...string) *exec.Cmd {
	cmd := exec.Command(program, args...)
	cmd.Env = []string{}
	return cmd
}

// Needed is whether device actions need the privileged helper: raw USB,
// mounting and ejection on Linux require root.
func Needed() bool { return os.Geteuid() != 0 }
