//go:build !rc2fxdebug

package app

// previewMode is lib.rs run's debug-only preview branch. Release builds have
// no previews (preview_on.go, `-tags rc2fxdebug`), so it never takes over.
func previewMode(args []string) bool { return false }
