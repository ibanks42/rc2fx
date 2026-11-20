// Command rc2fx is the RC2FX desktop app.
//
// Windows builds link with -ldflags "-H windowsgui" and a packaging .syso
// (icon, VERSIONINFO, manifest).
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

// Local copy of desktop/packaging/rc2fx.png since go:embed cannot reach
// ../../packaging. icon_test.go verifies the two stay in sync.
//
//go:embed rc2fx.png
var icon []byte

func main() {
	// Velopack hooks must run first; they handle install/update/uninstall args
	// and exit before GUI startup.
	runVelopackHooks()
	// Keep the payload-mode marker that packaging checks for.
	installer.KeepPayloadMode()
	usbcontrol.RegisterControlProbe()

	// Headless privileged helper modes invoked via pkexec.
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case helper.USBRuleFlag:
			if err := helper.InstallUSBRule(); err != nil {
				fmt.Fprintf(os.Stderr, "rc2fx: %s\n", err)
				os.Exit(1)
			}
			return
		case helper.Flag:
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
