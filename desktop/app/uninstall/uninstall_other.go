//go:build !windows

package uninstall

// OfferCleanup is a no-op outside Windows; Velopack only runs uninstall hooks
// on Windows.
func OfferCleanup() {}
