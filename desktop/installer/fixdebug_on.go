//go:build rc2fxdev

package installer

// fixDebugAllowed: development builds (`-tags rc2fxdev`) let a storage fix
// request ask for the script's debug log on the mixer (Request.FixDebug).
const fixDebugAllowed = true
