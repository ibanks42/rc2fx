//go:build !linux && !darwin && !windows

package selfupdate

// Velopack packages RC2FX for Linux, macOS and Windows only.

func VelopackLocator() (LocatorConfig, bool) { return LocatorConfig{}, false }

func PackagesDir() (string, bool) { return "", false }

func EnsurePrivate(dir string) error { return nil }
