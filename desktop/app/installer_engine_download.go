//go:build !embedded

package app

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// Download-only installer engine used when no payload is embedded in the binary.
func installerEngine(work string) *installer.Engine { return installer.DownloadOnly(work) }

func startInstallerHelper(work string) (*helper.Helper, error) {
	return helper.StartDownloadOnly(work)
}
