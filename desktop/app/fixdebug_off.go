//go:build !rc2fxdev

package app

// fixDebug: a release build never asks for the storage fix's debug log.
func fixDebug() bool { return false }
