//go:build !velopack

package selfupdate

// devBuild is cfg!(debug_assertions) for RC2FX_DEV_UPDATE: a build without
// Velopack (no `velopack` tag) is a development build; release packaging
// always builds with the tag.
const devBuild = true
