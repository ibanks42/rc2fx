//go:build rc2fxdev

package app

import "os"

// fixDebug, in development builds only (packaging refuses rc2fxdev):
// RC2FX_DEV_FIX_DEBUG=1 has the storage fix's script keep its debug log on
// the mixer, under /Application/update-staging/rcfix-debug/, for hardware
// runs (installer.Request.FixDebug).
func fixDebug() bool { return os.Getenv("RC2FX_DEV_FIX_DEBUG") == "1" }
