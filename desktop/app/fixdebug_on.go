//go:build rc2fxdev

package app

import "os"

// Dev-only toggle: when RC2FX_DEV_FIX_DEBUG=1, the storage fix script keeps its
// debug log on the mixer under /Application/update-staging/rcfix-debug/.
func fixDebug() bool { return os.Getenv("RC2FX_DEV_FIX_DEBUG") == "1" }
