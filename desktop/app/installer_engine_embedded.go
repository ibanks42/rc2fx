//go:build embedded

package app

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// installerEngine is the installer over the payload this executable embeds
// (installer/mod.rs engine).
func installerEngine(work string) *installer.Engine { return installer.Embedded(work) }

// startInstallerHelper is the privileged helper for the same payload as
// installerEngine (installer/mod.rs start_helper).
func startInstallerHelper(work string) (*helper.Helper, error) { return helper.StartEmbedded(work) }
