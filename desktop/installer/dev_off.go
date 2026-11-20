//go:build !rc2fxdev

package installer

// devRepository is development builds' local release folder; never in a
// release build.
func devRepository() (string, bool) { return "", false }

// fixDebugAllowed: a release build never puts the storage fix's debug log
// on a mixer, whatever a request asks for.
const fixDebugAllowed = false
