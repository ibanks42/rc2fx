// Command rc2fx is the RC2FX desktop app.
//
// Windows builds link with -ldflags "-H windowsgui" (no console window) and
// a .syso from packaging (icon 1, VERSIONINFO, the manifest).
package main

import (
	_ "embed"
	"fmt"
	"os"

	"github.com/ibanks42/rc2fx/desktop/app"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
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
	usbcontrol.RegisterControlProbe()

	// Headless privileged helper modes started through pkexec: no GUI window.
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case helper.USBRuleFlag:
			// Write the embedded udev rule, reload udev, exit. No other input.
			if err := helper.InstallUSBRule(); err != nil {
				fmt.Fprintf(os.Stderr, "rc2fx: %s\n", err)
				os.Exit(1)
			}
			return
		case helper.Flag:
			// Long-lived privileged installer process: JSON lines on stdio.
			if len(os.Args) < 4 {
				os.Exit(2)
			}
			if err := helper.Serve(os.Args[2], os.Args[3]); err != nil {
				fmt.Fprintf(os.Stderr, "rc2fx installer helper: %s\n", err)
				os.Exit(1)
			}
			return
		}
	}

	app.SetWindowIcon(icon)
	app.Run()
}
