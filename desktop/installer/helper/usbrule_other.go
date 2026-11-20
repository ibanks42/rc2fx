//go:build !linux

package helper

import "github.com/ibanks42/rc2fx/desktop/installer"

func InstallUSBRule() error { return installer.Error("Only Linux needs a USB rule") }

func Needed() bool { return false }
