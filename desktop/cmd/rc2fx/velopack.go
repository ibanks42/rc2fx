package main

import (
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate/vpkc"
	"github.com/ibanks42/rc2fx/desktop/app/uninstall"
)

// runVelopackHooks is main.rs's VelopackApp::build()
// .set_auto_apply_on_startup(false), .set_locator(rc2fx_app::velopack_locator())
// (the same private packages folder the app downloads updates into), on
// Windows .on_before_uninstall_fast_callback(uninstall::offer_cleanup), then
// .run(): it handles --veloapp-install|updated|obsolete|uninstall <version>
// (runs the hook, then exits). Velopack's C library is linked only with the
// `velopack` build tag (app/selfupdate/vpkc); without it nothing runs here,
// Velopack's hook arguments would open the app instead of exiting, and the
// build must not be packaged.
func runVelopackHooks() { vpkc.RunHooks(uninstall.OfferCleanup) }
