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

// InstallUSBRule atomically installs USBRule to USBRulePath (0644, root-owned)
// and reloads udev rules for attached hidraw devices.
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

// Fixed paths so the root helper never relies on caller-controlled PATH.
var udevadmPaths = [3]string{"/usr/bin/udevadm", "/sbin/udevadm", "/bin/udevadm"}

func udevadm() (string, error) {
	for _, path := range udevadmPaths {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
			return path, nil
		}
	}
	return "", installer.Error("udevadm was not found")
}

func cleanCommand(program string, args ...string) *exec.Cmd {
	cmd := exec.Command(program, args...)
	cmd.Env = []string{}
	return cmd
}

// Needed reports whether device operations require the privileged helper.
func Needed() bool { return os.Geteuid() != 0 }
