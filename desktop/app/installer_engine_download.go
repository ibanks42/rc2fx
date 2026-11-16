//go:build !embedded

package app

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// installerEngine is the installer without an embedded payload: it has no
// mixer software until a signed release is selected (installer/mod.rs
// engine).
func installerEngine(work string) *installer.Engine { return installer.DownloadOnly(work) }

// startInstallerHelper is the privileged helper for the same payload as
// installerEngine (installer/mod.rs start_helper).
func startInstallerHelper(work string) (*helper.Helper, error) {
	return helper.StartDownloadOnly(work)
}
