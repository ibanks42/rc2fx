package main

import (
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate/vpkc"
	"github.com/ibanks42/rc2fx/desktop/app/uninstall"
)

// runVelopackHooks initializes Velopack with auto-apply disabled, points the
// locator at the app's private download directory, and registers
// uninstall.OfferCleanup on Windows. When invoked with Velopack lifecycle flags
// (--veloapp-install|updated|obsolete|uninstall <version>), it runs the hook
// and exits.
//
// The Velopack C library is only linked under the `velopack` build tag; without
// it this is a no-op, so untagged builds must not be packaged for distribution.
func runVelopackHooks() { vpkc.RunHooks(uninstall.OfferCleanup) }
