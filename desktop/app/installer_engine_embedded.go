//go:build embedded

package app

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// Embedded-payload installer engine.
func installerEngine(work string) *installer.Engine { return installer.Embedded(work) }

func startInstallerHelper(work string) (*helper.Helper, error) { return helper.StartEmbedded(work) }
