// Command rc2fx is the RC2FX desktop app.
//
// Windows builds link with -ldflags "-H windowsgui" (no console window) and
// a .syso from packaging (icon 1, VERSIONINFO, the manifest).
package main

import (
	_ "embed"

	"github.com/ibanks42/rc2fx/desktop/app"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// icon is the window icon, a copy of desktop/packaging/rc2fx.png
// (icon_test.go keeps the two the same; go:embed cannot reach
// ../../packaging, so the copy stays).
//
//go:embed rc2fx.png
var icon []byte

func main() {
	// First: Velopack runs RC2FX with special arguments while installing,
	// updating and uninstalling, then exits. Updates apply only when the
	// user chooses "Restart to update", never at the next launch by itself.
	runVelopackHooks()
	// Keeps the installer's payload-mode marker in the executable, so the
	// linker does not drop it.
	_ = installer.PayloadMode()
	app.SetWindowIcon(icon)
	app.Run()
}
