//go:build !velopack

package selfupdate

// Builds without the velopack tag are development builds (enables RC2FX_DEV_UPDATE).
const devBuild = true
