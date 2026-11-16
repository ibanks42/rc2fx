//go:build !velopack

// Package vpkc is Velopack's C library for package selfupdate; see vpkc.go.
// This build has no `velopack` tag, so it has no Velopack: it cannot update
// itself (the Versions section links to the release page) and runs no
// startup hooks. Velopack's installers run the app with --veloapp-*
// arguments and expect it to exit, so a build without the tag must never be
// packaged.
package vpkc

import "github.com/ibanks42/rc2fx/desktop/app/selfupdate"

// Open is nil: no UpdateManager in this build.
var Open selfupdate.Opener

// RunHooks does nothing in a build without Velopack.
func RunHooks(beforeUninstall func()) { _ = beforeUninstall }
