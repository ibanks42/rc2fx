//go:build !windows

package uninstall

// OfferCleanup exists on Windows only in the Rust; Velopack runs uninstall
// hooks only there, so elsewhere this is never called and does nothing.
func OfferCleanup() {}
