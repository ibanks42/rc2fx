//go:build !rc2fxdev

package installer

func devRepository() (string, bool) { return "", false }

// Release builds never write the storage-fix debug log to the mixer.
const fixDebugAllowed = false
