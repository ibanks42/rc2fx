//go:build !windows

package uninstall

// OfferCleanup does something on Windows only; Velopack runs uninstall
// hooks only there, so elsewhere this is never called and does nothing.
func OfferCleanup() {}
