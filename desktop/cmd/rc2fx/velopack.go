package main

import (
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate/vpkc"
	"github.com/ibanks42/rc2fx/desktop/app/uninstall"
)

// runVelopackHooks runs Velopack's startup with automatic apply off, the
// locator set to the private packages folder the app downloads updates
// into, and on Windows uninstall.OfferCleanup before an uninstall: it
// handles --veloapp-install|updated|obsolete|uninstall <version> (runs the
// hook, then exits). Velopack's C library is linked only with the
// `velopack` build tag (app/selfupdate/vpkc); without it nothing runs here,
// Velopack's hook arguments would open the app instead of exiting, and the
// build must not be packaged.
func runVelopackHooks() { vpkc.RunHooks(uninstall.OfferCleanup) }
