//go:build !linux

package helper

import "github.com/ibanks42/rc2fx/desktop/installer"

// InstallUSBRule: only Linux needs a USB rule.
func InstallUSBRule() error { return installer.Error("Only Linux needs a USB rule") }

// Needed is whether device actions need the privileged helper; only Linux
// needs one.
func Needed() bool { return false }
