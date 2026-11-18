//go:build !rc2fxdev

package installer

// fixDebugAllowed: a release build never puts the storage fix's debug log
// on a mixer, whatever a request asks for.
const fixDebugAllowed = false
