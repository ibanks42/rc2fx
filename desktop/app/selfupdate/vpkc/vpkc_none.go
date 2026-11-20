//go:build !velopack

// Package vpkc is a stub when built without the `velopack` tag. Release
// packaging must always enable the tag so --veloapp-* hooks are handled.
package vpkc

import "github.com/ibanks42/rc2fx/desktop/app/selfupdate"

var Open selfupdate.Opener

func RunHooks(beforeUninstall func()) { _ = beforeUninstall }
