//go:build !rc2fxdev

package installer

// devRepository is development builds' local release folder; never in a
// release build.
func devRepository() (string, bool) { return "", false }
